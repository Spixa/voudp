use log::{error, info, trace, warn};
use opus::{Application, Channels as OpusChannels, Decoder, Encoder};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::mixer;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960; // per 20ms = 48000
const RB_CAP: usize = 1024;
const JITTER_BUFFER_LEN: usize = 10;

struct Remote {
    encoder: Encoder,
    decoder: Decoder,
    last_active: Instant,
    channel_id: u32,
    addr: SocketAddr,
    mask: Option<String>,
    jitter_buffer: VecDeque<Vec<f32>>,
}

impl Remote {
    fn new(addr: SocketAddr) -> Result<Self, opus::Error> {
        let mut encoder = Encoder::new(SAMPLE_RATE, OpusChannels::Stereo, Application::Audio)?;
        let decoder = Decoder::new(SAMPLE_RATE, OpusChannels::Stereo)?;

        info!(
            "New client has initialized with addr {} (rate: {}, audio: {})",
            addr,
            encoder.get_sample_rate()?,
            "Stereo"
        );
        Ok(Self {
            encoder,
            decoder,
            last_active: Instant::now(),
            channel_id: 0,
            addr,
            mask: None,
            jitter_buffer: VecDeque::with_capacity(JITTER_BUFFER_LEN),
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
}

impl Channel {
    fn new() -> Self {
        info!("Created new channel");
        Self {
            remotes: vec![],
            buffers: HashMap::new(),
            filter_states: HashMap::new(),
        }
    }

    fn add_remote(&mut self, remote: SafeRemote) {
        let addr = { remote.lock().unwrap().addr };
        self.remotes.push(remote);

        self.buffers.insert(addr, vec![0.0; FRAME_SIZE * 2]);
        self.filter_states.insert(addr, (0.0, 0.0));
    }

    fn remove_remote(&mut self, addr: &SocketAddr) {
        self.remotes.retain(|c| c.lock().unwrap().addr != *addr);
        self.buffers.remove(addr);
        self.filter_states.remove(addr);
    }

    fn mix(&mut self, socket: &UdpSocket) {
        // pre-proc audio for every remote:
        let mut processed_buffers = HashMap::new();
        for (addr, buf) in &self.buffers {
            if buf.len() != FRAME_SIZE * 2 || mixer::is_silent(buf) {
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

            // Compute gain once
            let gain = 1.0 / (active_count as f32).sqrt();

            let mut mix = vec![0.0f32; FRAME_SIZE * 2];
            for (mixing_remote, buf) in talkers {
                trace!(
                    "Now mixing {} with the total audio being sent to {}",
                    mixing_remote, remote_addr
                );
                for (i, sample) in buf.iter().enumerate() {
                    mix[i] += sample * gain;
                }
            }

            mixer::compress(&mut mix, 0.5, 0.8);
            mixer::normalize(&mut mix);
            mixer::soft_clip(&mut mix);

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
    socket: Arc<UdpSocket>,
    remotes: HashMap<SocketAddr, SafeRemote>,
    channels: HashMap<u32, Channel>,
    audio_rb: HeapRb<(SocketAddr, Vec<u8>)>,
}

impl ServerState {
    pub fn new(port: u16) -> Result<Self, io::Error> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{}", port))?;
        socket.set_nonblocking(true)?;
        info!("Bound to 0.0.0.0:{}", port);
        let socket = Arc::new(socket); // wrap in Arc
        info!(
            "There are {} free buffers (max remotes that can connect)",
            RB_CAP
        );
        Ok(Self {
            socket: Arc::clone(&socket),
            remotes: HashMap::new(),
            channels: HashMap::new(),
            audio_rb: HeapRb::new(RB_CAP),
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
            _ => error!("{} sent an invalid packet", addr),
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
                Remote::new(addr).expect("remote creation failed"),
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
        let channel = self.channels.entry(chan_id).or_insert_with(Channel::new);

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
            warn!("mask from unknown client: {}, skipping request...", addr);
            return;
        };

        let mut remote = remote.lock().unwrap();
        let Ok(new_mask) = String::from_utf8(data.to_vec()) else {
            warn!("mask sent over is not UTF-8, skipping request...");
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

    fn process_audio_tick(&mut self) {
        // Decode incoming packets and fill jitter buffers
        while let Some((addr, data)) = self.audio_rb.try_pop() {
            let Some(remote) = self.remotes.get(&addr) else {
                continue;
            };
            let mut remote = remote.lock().unwrap();

            let mut pcm = vec![0.0f32; FRAME_SIZE * 2];
            match remote.decoder.decode_float(&data, &mut pcm, false) {
                Ok(len) if len == FRAME_SIZE => {
                    if remote.jitter_buffer.len() < JITTER_BUFFER_LEN {
                        remote.jitter_buffer.push_back(pcm);
                    } else {
                        warn!("Jitter buffer full for {addr}");
                    }
                }
                Ok(len) => error!("Bad frame size from {addr}: got {len}, expected {FRAME_SIZE}"),
                Err(e) => error!("Decode error from {addr}: {e:?}"),
            }
        }

        // Pull one frame per remote into channel buffer
        for (addr, remote) in &self.remotes {
            let mut remote = remote.lock().unwrap();
            let chan_id = remote.channel_id;
            let frame = remote
                .jitter_buffer
                .pop_front()
                .unwrap_or(vec![0.0; FRAME_SIZE * 2]);

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

            if now.duration_since(last_active) > Duration::from_secs(5) {
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    info!("{addr} is dropped due to timeout");
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

        loop {
            while let Ok((size, addr)) = self.socket.recv_from(&mut buf) {
                self.handle_packet(addr, &buf[..size]);
            }

            if Instant::now() >= next_tick {
                self.process_audio_tick();
                self.cleanup();
                next_tick += Duration::from_millis(20);
            }

            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
