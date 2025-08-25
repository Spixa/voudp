use log::{error, info, warn};
use opus::{Application, Channels as OpusChannels, Decoder, Encoder};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    mixer,
    util::{self, ControlRequest, SecureUdpSocket},
};
const JITTER_BUFFER_LEN: usize = 50;

#[derive(Clone, Copy, PartialEq)]
pub enum Clipping {
    Soft,
    Hard,
}

#[derive(Clone, Copy)]
pub struct ServerConfig {
    pub max_users: usize,
    pub should_normalize: bool,
    pub should_compress: bool,
    pub clipping: Clipping,
    pub compress_threshold: f32,
    pub compress_ratio: f32,
    pub bind_port: u16,
    pub timeout_secs: u64,
    pub throttle_millis: u64,
    pub sample_rate: u32,
    pub tickrate: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_users: 1024,
            should_normalize: true,
            should_compress: true,
            clipping: Clipping::Soft,
            compress_threshold: 0.5,
            compress_ratio: 0.8,
            bind_port: 0,
            timeout_secs: 5,
            throttle_millis: 1,
            sample_rate: 48000,
            tickrate: 50,
        }
    }
}

impl ServerConfig {
    pub fn get_framesize(&self) -> usize {
        (self.sample_rate / self.tickrate).try_into().unwrap()
    }
}

#[derive(Default, Clone, Copy)]
struct RemoteStatus {
    deaf: bool,
    mute: bool,
}

struct Remote {
    encoder: Encoder,
    decoder: Decoder,
    last_active: Instant,
    channel_id: u32,
    addr: SocketAddr,
    mask: Option<String>,
    jitter_buffer: VecDeque<Vec<f32>>,
    status: RemoteStatus,
}

impl Remote {
    fn new(addr: SocketAddr, sample_rate: u32) -> Result<Self, opus::Error> {
        let encoder = Encoder::new(sample_rate, OpusChannels::Stereo, Application::Audio)?;
        let decoder = Decoder::new(sample_rate, OpusChannels::Stereo)?;

        info!(
            "New remote has initialized with addr {} (sample rate: {}, audio: {})",
            addr, sample_rate, "Stereo"
        );
        Ok(Self {
            encoder,
            decoder,
            last_active: Instant::now(),
            channel_id: 0,
            addr,
            mask: None,
            jitter_buffer: VecDeque::with_capacity(JITTER_BUFFER_LEN),
            status: Default::default(),
        })
    }

    fn mask(&mut self, mask: &str) {
        self.mask = Some(String::from(mask));
    }
}

type SafeRemote = Arc<Mutex<Remote>>;
struct Channel {
    remotes: Vec<SafeRemote>,
    buffers: HashMap<SocketAddr, Vec<f32>>,
    filter_states: HashMap<SocketAddr, (f32, f32)>,
    server_config: ServerConfig,
}

impl Channel {
    fn new(server_config: ServerConfig) -> Self {
        info!("Created new channel");
        Self {
            remotes: vec![],
            buffers: HashMap::new(),
            filter_states: HashMap::new(),
            server_config,
        }
    }

    fn add_remote(&mut self, remote: SafeRemote) {
        let addr = { remote.lock().unwrap().addr };
        self.remotes.push(remote);

        self.buffers
            .insert(addr, vec![0.0; self.server_config.get_framesize() * 2]);
        self.filter_states.insert(addr, (0.0, 0.0));
    }

    fn remove_remote(&mut self, addr: &SocketAddr) {
        self.remotes.retain(|c| c.lock().unwrap().addr != *addr);
        self.buffers.remove(addr);
        self.filter_states.remove(addr);
    }

    fn mix(&mut self, socket: &SecureUdpSocket) {
        // pre-proc audio for every remote:
        let mut processed_buffers = HashMap::new();
        for (addr, buf) in &self.buffers {
            if buf.len() != self.server_config.get_framesize() * 2 || mixer::is_silent(buf) {
                continue;
            }

            let state = self.filter_states.entry(*addr).or_insert((0.0, 0.0));
            let mut processed = buf.clone();
            mixer::remove_dc_bias(&mut processed, state);
            processed_buffers.insert(*addr, processed);
        }

        // personalized mix which is done separately
        for remote in &self.remotes {
            let mut guard = remote.lock().unwrap();
            let remote_addr = guard.addr;

            if !self.buffers.contains_key(&remote_addr) {
                continue;
            }

            // collect all active talkers excluding self
            let talkers: Vec<_> = processed_buffers
                .iter()
                .filter(|(addr, _)| **addr != remote_addr)
                .collect();

            let active_count = talkers.len();
            if active_count == 0 {
                continue;
            }

            // compute gain once
            let gain = 1.0 / (active_count as f32).sqrt();

            let mut mix = vec![0.0f32; self.server_config.get_framesize() * 2];
            for (_, buf) in talkers {
                for (i, sample) in buf.iter().enumerate() {
                    mix[i] += sample * gain;
                }
            }

            if self.server_config.should_compress {
                mixer::compress(
                    &mut mix,
                    self.server_config.compress_threshold,
                    self.server_config.compress_ratio,
                );
            }

            if self.server_config.should_normalize {
                mixer::normalize(&mut mix);
            }

            match self.server_config.clipping {
                Clipping::Soft => mixer::soft_clip(&mut mix),
                Clipping::Hard => {
                    mix.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
                }
            }

            let mut encoded = vec![0u8; 400];
            let len = guard.encoder.encode_float(&mix, &mut encoded).unwrap_or(0);

            if len > 0 {
                let mut packet = vec![0x02];
                packet.extend_from_slice(&encoded[..len]);
                if let Err(e) = socket.send_to(&packet, remote_addr) {
                    error!("Failed to send audio to {remote_addr}: {e}");
                }
            }
        }

        // Clear buffers for next tick
        for buf in self.buffers.values_mut() {
            buf.fill(0.0);
        }
    }
}

pub struct ServerState {
    socket: Arc<SecureUdpSocket>,
    remotes: HashMap<SocketAddr, SafeRemote>,
    channels: HashMap<u32, Channel>,
    audio_rb: HeapRb<(SocketAddr, Vec<u8>)>,
    config: ServerConfig,
}

impl ServerState {
    pub fn new(config: ServerConfig, phrase: &[u8]) -> Result<Self, io::Error> {
        info!("Deriving key from phrase...");
        let key = util::derive_key_from_phrase(phrase, util::VOUDP_SALT);
        let socket = SecureUdpSocket::create(format!("0.0.0.0:{}", config.bind_port), key)?;

        info!("Bound to 0.0.0.0:{}", config.bind_port);
        let socket = Arc::new(socket); // wrap in Arc
        info!(
            "There are {} free buffers (max remotes that can connect)",
            config.max_users
        );
        Ok(Self {
            socket: Arc::clone(&socket),
            remotes: HashMap::new(),
            channels: HashMap::new(),
            audio_rb: HeapRb::new(config.max_users),
            config,
        })
    }

    fn handle_packet(&mut self, addr: SocketAddr, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        match data[0] {
            0x01 => self.handle_join(addr, &data[1..]),
            0x02 => self.handle_audio(addr, &data[1..]),
            0x03 => self.handle_eof(addr),
            0x04 => self.handle_mask(addr, &data[1..]),
            0x05 => self.handle_list(addr),
            0x06 => self.handle_chat(addr, &data[1..]),
            0x08 => self.handle_ctrl(addr, &data[1..]),
            _ => error!(
                "{} sent an invalid packet (starts with {:#?})",
                addr, data[0]
            ),
        }
    }

    fn handle_join(&mut self, addr: SocketAddr, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        // this is painful:
        let chan_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

        info!("{} has joined the channel with id {}", addr, chan_id);
        // move remote to new channel or create new remote if it is new
        let remote = self.remotes.entry(addr).or_insert_with(|| {
            info!("{} is a new remote", addr);
            Arc::new(Mutex::new(
                Remote::new(addr, self.config.sample_rate).expect("remote creation failed"),
            ))
        });

        // remove from previous channel:
        {
            let mut remote = remote.lock().unwrap();

            if let Some(prev_chan) = self.channels.get_mut(&remote.channel_id) {
                prev_chan.remove_remote(&addr);
            }
            remote.channel_id = chan_id;
        }

        // get the channel that the remote is trying to join, or create it if it doesn't exist
        let channel = self
            .channels
            .entry(chan_id)
            .or_insert_with(|| Channel::new(self.config));

        channel.add_remote(remote.to_owned());
    }

    fn handle_audio(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            return;
        };
        let mut remote = remote.lock().unwrap();

        remote.last_active = Instant::now();

        // push to ring buffer for audio processing:
        if self.audio_rb.is_full() {
            error!("audio buffer overflow");
            return;
        }

        self.audio_rb.try_push((addr, data.to_vec())).unwrap(); // impossible to panic because of previous check
    }

    fn handle_eof(&mut self, addr: SocketAddr) {
        self.remotes.retain(|addr_got, remote| {
            if *addr_got == addr {
                let channel_id = { remote.lock().unwrap().channel_id };
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    info!("{addr} has left");
                    channel.remove_remote(&addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                return false;
            }
            true
        });
    }

    fn handle_mask(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            warn!("Mask from unknown remote: {}, skipping request...", addr);
            return;
        };

        let mut remote = remote.lock().unwrap();
        let Ok(new_mask) = String::from_utf8(data.to_vec()) else {
            warn!("Mask sent over is not UTF-8, skipping request...");
            return;
        };

        match &remote.mask {
            Some(old_mask) => {
                info!(
                    "\"{}\" has changed their mask to \"{}\" ({})",
                    old_mask, new_mask, addr
                );
            }
            None => {
                info!(
                    "\"{}\" has masked for the first time to \"{}\"",
                    addr, new_mask
                );
            }
        }

        remote.mask(&new_mask);
    }

    fn handle_list(&mut self, addr: SocketAddr) {
        let Some(remote) = self.remotes.get(&addr) else {
            warn!(
                "List request from unknown remote: {}, skipping request...",
                addr
            );
            return;
        };
        let remote = remote.lock().unwrap();

        let Some(channel) = self.channels.get(&remote.channel_id) else {
            warn!(
                "Failed to retrieve the channel of remote {}, skipping request...",
                addr
            );
            return;
        };

        // prevent locking (we will lock later)
        drop(remote);

        // TODO: make this lazy. do not compute per-request
        let (masked, unmasked_count): (Vec<(String, bool, bool)>, u32) = channel
            .remotes
            .iter()
            .map(|r| {
                let r = r.lock().unwrap();
                (
                    r.mask.clone(),
                    r.status.mute, // bool
                    r.status.deaf, // bool
                )
            })
            .fold(
                (vec![], 0),
                |(mut masks, count), (mask_opt, muted, deafened)| {
                    if let Some(mask) = mask_opt {
                        masks.push((mask, muted, deafened));
                        (masks, count)
                    } else {
                        (masks, count + 1)
                    }
                },
            );

        // build payload
        let mut payload = Vec::new();
        for (mask, muted, deafened) in &masked {
            payload.extend_from_slice(mask.as_bytes());
            payload.push(0x01);
            let flags = (*muted as u8) | ((*deafened as u8) << 1);
            payload.push(flags);
        }

        // final packet
        let mut list_packet = vec![0x05];
        list_packet.extend_from_slice(&unmasked_count.to_be_bytes());
        list_packet.extend_from_slice(&(masked.len() as u32).to_be_bytes());
        list_packet.extend_from_slice(&payload);

        self.socket.send_to(&list_packet, addr).unwrap();
    }

    fn handle_chat(&mut self, addr: SocketAddr, data: &[u8]) {
        let (mask, chan_id) = {
            let Some(remote) = self.remotes.get(&addr) else {
                warn!(
                    "Chat request from unknown remote: {}, skipping request...",
                    addr
                );
                return;
            };
            let remote = remote.lock().unwrap();

            (remote.mask.clone(), remote.channel_id)
        };

        let Some(channel) = self.channels.get(&chan_id) else {
            warn!(
                "Failed to retrieve the channel of remote {}, skipping request...",
                addr
            );
            return;
        };

        match mask {
            Some(mask) => {
                let Ok(msg) = String::from_utf8(data.to_vec()) else {
                    warn!("{addr} sent a non UTF-8 encoded chat string");
                    return;
                };

                for remote in channel.remotes.iter() {
                    let addr = { remote.lock().unwrap().addr };

                    let mut msg_packet = vec![0x06];
                    msg_packet.extend_from_slice(mask.as_bytes());
                    msg_packet.extend([0x01]);
                    msg_packet.extend_from_slice(data);

                    let _ = self.socket.send_to(&msg_packet, addr);
                }

                info!("[#chan-{}] <{}> {}", chan_id, mask, msg);
            }
            None => {
                let unauth_packet = vec![0x07];
                let _ = self.socket.send_to(&unauth_packet, addr);
                warn!("{addr} tried sending chat message without having a mask!");
            }
        }
    }

    pub fn handle_ctrl(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            warn!(
                "List request from unknown remote: {}, skipping request...",
                addr
            );
            return;
        };
        let mut remote = remote.lock().unwrap();

        match util::parse_control_packet(data) {
            Ok(req) => match req {
                ControlRequest::SetDeafen => remote.status.deaf = true,
                ControlRequest::SetUndeafen => remote.status.deaf = false,
                ControlRequest::SetMute => remote.status.mute = true,
                ControlRequest::SetUnmute => remote.status.mute = false,
                ControlRequest::SetVolume(_) => warn!("{addr} accessed an unimplemented feature"),
            },
            Err(e) => {
                warn!("{addr} sent a bad control packet: {e}");
            }
        }
    }

    pub fn handle_bad(&mut self, addr: SocketAddr) {
        warn!("{addr} sent a bad packet");
        let _ = self.socket.send_bad_packet_notice(addr);
    }

    fn process_audio_tick(&mut self) {
        let framesize = self.config.get_framesize();
        // decode incoming packets and fill jitter buffers
        while let Some((addr, data)) = self.audio_rb.try_pop() {
            let Some(remote) = self.remotes.get(&addr) else {
                continue;
            };
            let mut remote = remote.lock().unwrap();

            let mut pcm = vec![0.0f32; framesize * 2];
            match remote.decoder.decode_float(&data, &mut pcm, false) {
                Ok(len) if len == framesize => {
                    if remote.jitter_buffer.len() < JITTER_BUFFER_LEN {
                        remote.jitter_buffer.push_back(pcm);
                    } else {
                        warn!("Jitter buffer full for {addr}");
                    }
                }
                Ok(len) => error!("Bad frame size from {addr}: got {len}, expected {framesize}"),
                Err(e) => error!("Decode error from {addr}: {e:?}"),
            }
        }

        // Pull one frame per remote into channel buffer
        for (addr, remote) in &self.remotes {
            let mut remote = remote.lock().unwrap();
            let chan_id = remote.channel_id;
            let frame =
                remote
                    .jitter_buffer
                    .pop_front()
                    .unwrap_or(vec![0.0; self.config.get_framesize() * 2]);

            if let Some(channel) = self.channels.get_mut(&chan_id) {
                channel.buffers.insert(*addr, frame);
            }
        }

        for channel in self.channels.values_mut() {
            channel.mix(&self.socket);
        }
    }

    fn cleanup(&mut self) {
        let now = Instant::now();

        self.remotes.retain(|addr, remote| {
            let last_active = { remote.lock().unwrap().last_active };

            let channel_id = { remote.lock().unwrap().channel_id };

            if now.duration_since(last_active) > Duration::from_secs(self.config.timeout_secs) {
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    info!(
                        "{addr} is dropped due to timeout of {} seconds",
                        self.config.timeout_secs
                    );
                    channel.remove_remote(addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                false // remote hasn't updated in the past N seconds, needs to be kicked
            } else {
                true // remote can stay alive
            }
        });
    }

    pub fn run(&mut self) {
        let mut buf = [0u8; 2048];
        let mut next_tick = Instant::now();

        let throttle = self.config.throttle_millis;
        let tick_period = 1000 / self.config.tickrate as u64; // in ms
        info!(
            "Tick period is {}ms ({} tps) with {}ms throttles",
            tick_period, self.config.tickrate, throttle
        );
        info!(
            "Sample rate is {} ({} samples per tick per audio channel)",
            self.config.sample_rate,
            self.config.get_framesize()
        );

        if self.config.should_compress {
            info!(
                "Audio compression is enabled with threshold {} and ratio {}",
                self.config.compress_threshold, self.config.compress_ratio
            )
        } else {
            info!("Audio compression is disabled");
        }

        if self.config.should_normalize {
            info!("Audio normalization is enabled");
        } else {
            info!("Audio normalization is disabled");
        }

        if !self.config.should_compress
            && !self.config.should_normalize
            && self.config.clipping == Clipping::Hard
        {
            warn!(
                "This setting is not recommended (No compression, no normalization, with hard clipping)"
            );
        }

        match self.config.clipping {
            Clipping::Soft => info!("Samples are set to be soft-clipped"),
            Clipping::Hard => info!("Samples are set to be hard-clipped"),
        }

        loop {
            loop {
                match self.socket.recv_from(&mut buf) {
                    Ok((size, addr)) => {
                        self.handle_packet(addr, &buf[..size]);
                    }
                    Err(ref e) if e.0.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        self.handle_bad(e.1);
                        break;
                    }
                }
            }

            if Instant::now() >= next_tick {
                self.process_audio_tick();
                self.cleanup();
                next_tick += Duration::from_millis(tick_period);
            }

            std::thread::sleep(Duration::from_millis(throttle));
        }
    }
}
