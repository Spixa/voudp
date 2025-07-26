use opus::{Application, Channels as OpusChannels, Decoder, Encoder};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::mixer;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960; // per 20ms = 48000

struct Remote {
    encoder: Encoder,
    decoder: Decoder,
    last_active: Instant,
    channel_id: u32,
    addr: SocketAddr,
    mask: Option<String>,
}

impl Remote {
    fn new(addr: SocketAddr) -> Result<Self, opus::Error> {
        let encoder = Encoder::new(SAMPLE_RATE, OpusChannels::Stereo, Application::Audio)?;
        let decoder = Decoder::new(SAMPLE_RATE, OpusChannels::Stereo)?;
        Ok(Self {
            encoder,
            decoder,
            last_active: Instant::now(),
            channel_id: 0,
            addr,
            mask: None,
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
}

impl Channel {
    fn new() -> Self {
        println!("created new channel");
        Self {
            remotes: vec![],
            buffers: HashMap::new(),
        }
    }

    fn add_remote(&mut self, remote: SafeRemote) {
        let addr = { remote.lock().unwrap().addr };
        self.remotes.push(remote);

        self.buffers.insert(addr, vec![0.0; FRAME_SIZE * 2]);
    }

    fn remove_remote(&mut self, addr: &SocketAddr) {
        self.remotes.retain(|c| c.lock().unwrap().addr != *addr);
        self.buffers.remove(addr);
    }

    fn mix(&mut self, socket: &UdpSocket) {
        // in the new implementation, each remote gets their unique mixed audio, without their own voice
        // this type of implementation looks unnecessary at this stage but later on where each remote will have different audio settings for other remotes this will come in handy
        const HPF_COEFF: f32 = 0.95;
        let mut prev_left = 0.0f32;
        let mut prev_right = 0.0f32;

        for remote in &self.remotes {
            let mut guard = remote.lock().unwrap();
            let remote_addr = guard.addr;

            let mut mix_left = vec![0.0; 960];
            let mut mix_right = vec![0.0; 960];
            let mut active_remotes = 0;

            for (addr, buf) in &self.buffers {
                if buf.len() != FRAME_SIZE * 2 {
                    eprintln!("Invalid buffer size: {}", buf.len());
                    continue;
                }

                if *addr == remote_addr {
                    continue; // skips remote's own voice
                }

                if !mixer::is_silent(buf) {
                    active_remotes += 1;
                    let stereo_buf = buf.clone();

                    for (i, sample) in stereo_buf.chunks_exact(2).enumerate() {
                        // apply high-pass filter to remove DC offset
                        let left = sample[0] - prev_left + HPF_COEFF * prev_left;
                        let right = sample[1] - prev_right + HPF_COEFF * prev_right;
                        prev_left = left;
                        prev_right = right;

                        mix_left[i] += sample[0];
                        mix_right[i] += sample[1];
                    }
                }
            }

            if active_remotes == 0 {
                continue; // no audio to send to this client
            }

            mixer::normalize(&mut mix_left);
            mixer::soft_clip(&mut mix_left);

            mixer::normalize(&mut mix_right);
            mixer::soft_clip(&mut mix_right);
            // other modules for mixer will be added later

            // time to interleave the stereo samples:
            let stereo_mix: Vec<f32> = mix_left
                .iter()
                .zip(mix_right.iter())
                .flat_map(|(&l, &r)| [l, r])
                .collect();

            let mut encoded = vec![0u8; 400];
            let len = guard
                .encoder
                .encode_float(&stereo_mix, &mut encoded)
                .unwrap();

            // build audio packet: 0x02 + encoded opus data
            if len > 0 {
                let mut packet = vec![0x02];
                packet.extend_from_slice(&encoded[..len]);
                if let Err(e) = socket.send_to(&packet, remote_addr) {
                    eprintln!("failed to send audio to {remote_addr} because: {e}");
                }
            }
        }

        // reset buffers for next frame to come
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
        let socket = Arc::new(socket); // wrap in Arc

        Ok(Self {
            socket: Arc::clone(&socket),
            remotes: HashMap::new(),
            channels: HashMap::new(),
            audio_rb: HeapRb::new(1024),
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
            _ => eprintln!("{} sent an invalid packet", addr),
        }
    }

    fn handle_join(&mut self, addr: SocketAddr, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        // this is painful:
        let chan_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

        println!("{} has joined the channel with id {}", addr, chan_id);
        // move remote to new channel or create new remote if it is new
        let remote = self.remotes.entry(addr).or_insert_with(|| {
            println!("\tthis remote is new!");
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
            eprintln!("audio buffer overflow");
            return;
        }

        self.audio_rb.try_push((addr, data.to_vec())).unwrap(); // impossible to panic because of previous check
    }

    fn handle_eof(&mut self, addr: SocketAddr) {
        self.remotes.retain(|addr_got, remote| {
            if *addr_got == addr {
                let channel_id = { remote.lock().unwrap().channel_id };
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    println!("{addr} has left");
                    channel.remove_remote(&addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                return false;
            }
            true
        });
    }

    fn handle_mask(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            eprintln!("mask from unknown client: {}", addr);
            return;
        };

        let mut remote = remote.lock().unwrap();
        let Ok(new_mask) = String::from_utf8(data.to_vec()) else {
            eprintln!("mask sent over is not UTF-8");
            return;
        };

        match &remote.mask {
            Some(old_mask) => {
                println!(
                    "\"{}\" has changed their mask to \"{}\" ({})",
                    old_mask, new_mask, addr
                );
            }
            None => {
                println!(
                    "\"{}\" has masked for the first time to \"{}\"",
                    addr, new_mask
                );
            }
        }

        remote.mask(&new_mask);
    }

    fn process_audio(&mut self) {
        // pop audio bufffers from every remote with their associated data
        while let Some((addr, data)) = self.audio_rb.try_pop() {
            let Some(remote) = self.remotes.get(&addr) else {
                // if the peer's remote can be retreived get it
                continue;
            };
            let mut remote = remote.lock().unwrap();

            let mut pcm = vec![0.0f32; FRAME_SIZE * 2];
            match remote.decoder.decode_float(&data, &mut pcm, false) {
                Ok(len) => {
                    if len == FRAME_SIZE {
                        if let Some(channel) = self.channels.get_mut(&remote.channel_id) {
                            channel.buffers.insert(addr, pcm);
                        }
                    } else {
                        eprintln!("incomplete frame: {} samples", len);
                    }
                }
                Err(e) => eprintln!("decoding error: {:?}", e),
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
                    println!("{addr} is dropped due to timeout");
                    channel.remove_remote(addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                false // remote hasn't updated in the past N seconds, needs to be kicked
            } else {
                true // remote can stay alive
            }
        });
    }

    pub fn run(&mut self) {
        let mut buf = [0u8; 2048]; // max datagram size is 2048

        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((size, addr)) => self.handle_packet(addr, &buf[..size]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) => eprintln!("recv error: {}", e),
            }

            self.process_audio();
            self.cleanup();

            // throttle loop
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
