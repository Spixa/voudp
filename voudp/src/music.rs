use std::{
    fs::File,
    io::{self, ErrorKind, Read},
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use ed25519_dalek::{VerifyingKey, pkcs8::DecodePublicKey};
use opus2::{Bitrate, Encoder};
use symphonia::{
    core::{
        audio::{AudioBufferRef, Signal},
        codecs::{CODEC_TYPE_NULL, DecoderOptions},
        formats::FormatOptions,
        io::MediaSourceStream,
        meta::MetadataOptions,
        probe::Hint,
        sample::i24,
    },
    default::{get_codecs, get_probe},
};

use crate::{
    handshake::ClientHandshake,
    protocol::{self, ClientPacketType, FromPacket},
    socket::{self, SecureUdpSocket},
    util::{ChatPacket, FlowPacket, sha256},
};

const TARGET_SAMPLE_RATE: u32 = 48_000;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz
const FRAME_DURATION: Duration = Duration::from_millis(20);
const CHANNELS: usize = 2; // Stereo

pub struct MusicClientState {
    first: bool,
    socket: SecureUdpSocket,
    volume: Arc<AtomicU8>,
    current: Arc<Mutex<String>>,
    connected: Arc<AtomicBool>,
    channel_id: u32,
    skip: Arc<AtomicBool>,
    handshake: ClientHandshake,
}

impl MusicClientState {
    pub fn new(addr: &str, channel_id: u32, phrase: &[u8]) -> Result<Self> {
        let key = socket::derive_psk_from_phrase(phrase, protocol::VOUDP_SALT);
        let socket = SecureUdpSocket::create("0.0.0.0:0".into(), key)?;
        socket.connect(addr)?;

        let addr = addr
            .to_socket_addrs()?
            .into_iter()
            .find(|a| a.is_ipv4())
            .ok_or(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad server adress",
            ))?;

        Ok(Self {
            first: true,
            socket,
            volume: Arc::new(AtomicU8::new(50)),
            current: Arc::new(Mutex::new(String::from("Nothing"))),
            connected: Arc::new(AtomicBool::new(true)),
            skip: Arc::new(AtomicBool::new(false)),
            channel_id,
            handshake: ClientHandshake::new(addr, "music".into(), "music_password".into()),
        })
    }

    fn load_pinned_public_key_from_pem(pem_path: &str) -> io::Result<[u8; 32]> {
        let pem = std::fs::read_to_string(pem_path)?;
        let verifying_key = VerifyingKey::from_public_key_pem(&pem)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(verifying_key.to_bytes())
    }

    fn do_handshake(&mut self) -> Result<()> {
        self.handshake.start(&self.socket)?;
        let trusted_pin = sha256(&Self::load_pinned_public_key_from_pem("server_public.pem")?);

        while !self.handshake.is_done() {
            let mut recv_buf = [0u8; 2048];
            match self.socket.recv_from(&mut recv_buf) {
                Ok((size, _)) => {
                    if size > 1 && recv_buf[0] == ClientPacketType::Handshake as u8 {
                        self.handshake
                            .process(&self.socket, &recv_buf[1..size], &trusted_pin)?;
                    }
                }
                Err(e) if e.0.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_micros(100));
                }
                Err(_) => {}
            }
            thread::sleep(Duration::from_micros(100));
        }

        let mut packet = vec![ClientPacketType::JoinChannel as u8];
        packet.extend_from_slice(&2u32.to_be_bytes());
        self.socket.send(&packet)?;

        Ok(())
    }

    pub fn run(&mut self, path: String) -> Result<()> {
        if self.first {
            self.do_handshake()?;
            self.first = false;

            let volume = self.volume.clone();
            let sock = self.socket.clone();
            let conn = self.connected.clone();
            let current_music = self.current.clone();
            let skip = self.skip.clone();
            thread::spawn(move || {
                Self::control_listener(sock, volume, conn, current_music, skip);
            });

            let mut deaf_packet = vec![0x08];
            deaf_packet.push(0x01);
            self.socket.send(&deaf_packet)?;
        }

        println!("(re)joined channel {}", self.channel_id);

        let queue = Self::build_queue(Path::new(&path))?;

        for track_path in queue {
            let display_name = track_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown track")
                .to_string();

            *self.current.lock().unwrap() = display_name.clone();

            let mut msg_packet = vec![0x06];
            msg_packet
                .extend_from_slice(format!("Now playing the hit song {display_name}").as_bytes());
            let _ = self.socket.send(&msg_packet);

            // reset skip before each track so a #next from the *previous* track
            // doesn't immediately kill this one too.
            self.skip.store(false, Ordering::Relaxed);

            match self.play_file(&track_path) {
                Ok(_) => {}
                Err(e) => println!("Ran into an error: {e}, skipping this track"),
            }
        }

        println!("Goodbye!");
        self.connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn build_queue(path: &Path) -> Result<Vec<PathBuf>> {
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = path
                .read_dir()?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|e| e.path())
                .collect();
            entries.sort();
            Ok(entries)
        } else {
            Ok(vec![path.to_path_buf()])
        }
    }

    fn play_file(&mut self, path: &Path) -> Result<()> {
        let mut opus_encoder = Encoder::new(
            TARGET_SAMPLE_RATE,
            opus2::Channels::Stereo,
            opus2::Application::Audio,
        )?;
        opus_encoder.set_bitrate(Bitrate::Bits(64000))?;

        let mut file = File::open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());
        let hint = Hint::new();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let decode_opts = DecoderOptions::default();

        let probed = get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;
        let mut format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .context("no supported tracks found")?;

        let mut decoder = get_codecs().make(&track.codec_params, &decode_opts)?;
        let track_id = track.id;

        let mut sample_buf = Vec::with_capacity(FRAME_SIZE * CHANNELS * 10);
        let sample_rate = track.codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);

        let start = Instant::now();
        let mut f_idx = 0;

        while let Ok(packet) = format.next_packet() {
            if self.skip.load(Ordering::Relaxed) {
                break;
            }

            if packet.track_id() != track_id {
                continue;
            }

            let vol = 0.01 * self.volume.load(Ordering::Relaxed) as f32;
            match decoder.decode(&packet)? {
                AudioBufferRef::F32(buf) => {
                    process_buffer_f32(vol, &buf, &mut sample_buf, sample_rate)?
                }
                AudioBufferRef::S16(buf) => {
                    process_buffer_i16(vol, &buf, &mut sample_buf, sample_rate)?
                }
                AudioBufferRef::S24(buf) => {
                    process_buffer_i24(vol, &buf, &mut sample_buf, sample_rate)?
                }
                AudioBufferRef::S32(buf) => {
                    process_buffer_i32(vol, &buf, &mut sample_buf, sample_rate)?
                }
                AudioBufferRef::U8(buf) => {
                    process_buffer_u8(vol, &buf, &mut sample_buf, sample_rate)?
                }
                _ => return Err(anyhow!("unsupported audio buffer type")),
            }

            while sample_buf.len() >= FRAME_SIZE * CHANNELS {
                if self.skip.load(Ordering::Relaxed) {
                    sample_buf.clear();
                    break;
                }

                let target_time = start + FRAME_DURATION * f_idx;
                f_idx += 1;

                let frame = &sample_buf[..FRAME_SIZE * CHANNELS];
                let mut opus_frame = vec![0u8; 4000];
                let len = opus_encoder.encode_float(frame, &mut opus_frame)?;

                let mut audio_packet = vec![0x02];
                audio_packet.extend_from_slice(&opus_frame[..len]);
                self.upload_packet(&audio_packet)?;

                sample_buf.drain(0..FRAME_SIZE * CHANNELS);

                let now = Instant::now();
                if now < target_time {
                    std::thread::sleep(target_time - now);
                }
            }
        }

        if !self.skip.load(Ordering::Relaxed) && !sample_buf.is_empty() {
            let mut padded = vec![0.0; FRAME_SIZE * CHANNELS];
            let copy_len = sample_buf.len().min(padded.len());
            padded[..copy_len].copy_from_slice(&sample_buf[..copy_len]);

            let mut opus_frame = vec![0u8; 4000];
            let len = opus_encoder.encode_float(&padded, &mut opus_frame)?;

            let mut packet = vec![0x02u8];
            packet.extend_from_slice(&opus_frame[..len]);
            self.upload_packet(&packet)?;
        }

        Ok(())
    }

    fn control_listener(
        sock: SecureUdpSocket,
        volume: Arc<AtomicU8>,
        conn: Arc<AtomicBool>,
        current_music: Arc<Mutex<String>>,
        skip: Arc<AtomicBool>,
    ) {
        loop {
            if !conn.load(Ordering::Relaxed) {
                break;
            }

            let mut recv_buf = [0u8; 2048];
            match sock.recv_from(&mut recv_buf) {
                Ok((size, _)) => {
                    if size > 1 && recv_buf[0] == 0x06 {
                        match ChatPacket::deserialize(&recv_buf[..size]) {
                            Ok(chat) => {
                                let caster = chat.username;
                                let cmd = chat.message;

                                if cmd.starts_with("#current") {
                                    let mut msg_packet = vec![0x06];
                                    msg_packet.extend_from_slice(
                                        format!(
                                            "{caster}, I'm currently playing {}",
                                            current_music.lock().unwrap()
                                        )
                                        .as_bytes(),
                                    );
                                    let _ = sock.send(&msg_packet);
                                } else if cmd.starts_with("#next") {
                                    skip.store(true, Ordering::Relaxed);
                                    let mut msg_packet = vec![0x06];
                                    msg_packet.extend_from_slice(
                                        format!("Skipping to the next track, {caster}!").as_bytes(),
                                    );
                                    let _ = sock.send(&msg_packet);
                                } else if cmd.starts_with("#volume") {
                                    let args = cmd.split_whitespace().collect::<Vec<&str>>();

                                    match args.get(1) {
                                        Some(vol_str) => match vol_str.parse::<u8>() {
                                            Ok(vol) => {
                                                let mut msg_packet = vec![0x06];
                                                msg_packet.extend_from_slice(
                                                    format!("Volume set to {vol}, {caster}")
                                                        .as_bytes(),
                                                );
                                                let _ = sock.send(&msg_packet);
                                                volume.store(vol, Ordering::Relaxed);
                                            }
                                            Err(e) => {
                                                let mut msg_packet = vec![0x06];
                                                msg_packet.extend_from_slice(
                                                    format!("Garbage volume, {caster}: {e}")
                                                        .as_bytes(),
                                                );
                                                let _ = sock.send(&msg_packet);
                                            }
                                        },
                                        None => {
                                            let mut msg_packet = vec![0x06];
                                            msg_packet.extend_from_slice(
                                                format!(
                                                    "{caster}, use it like this: #volume <0-100>"
                                                )
                                                .as_bytes(),
                                            );
                                            let _ = sock.send(&msg_packet);
                                        }
                                    }
                                }
                            }
                            Err(e) => eprintln!("error: {e}"),
                        }
                    }

                    if size > 1
                        && (recv_buf[0] == 0x0a || recv_buf[0] == 0x0b)
                        && let Ok(msg) = FlowPacket::deserialize(&recv_buf[..size])
                        && let FlowPacket::Join(name) = msg
                    {
                        let mut msg_packet = vec![0x06];
                        msg_packet.extend_from_slice(
                            format!(
                                "Why hello there, {name}. I'm playing {}",
                                current_music.lock().unwrap()
                            )
                            .as_bytes(),
                        );
                        let _ = sock.send(&msg_packet);
                    }
                }
                Err(e) if e.0.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_micros(100));
                }
                Err(_) => {}
            }
            thread::sleep(Duration::from_micros(1000));
        }
    }

    fn upload_packet(&mut self, packet: &[u8]) -> Result<()> {
        self.socket.send(packet)?;
        Ok(())
    }
}

// OK so these process functions i had no fucking clue how to make them
// i admit AI helped me write all of them except the first one

// no conversion needed as we deal with f32 ourselves
fn process_buffer_f32(
    vol: f32,
    buffer: &symphonia::core::audio::AudioBuffer<f32>,
    sample_buffer: &mut Vec<f32>,
    original_sample_rate: u32,
) -> Result<()> {
    let channels = buffer.spec().channels.count();
    let frames = buffer.frames();

    // Create interleaved buffer
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            interleaved.push(buffer.chan(ch)[i]);
        }
    }

    process_interleaved(
        vol,
        &interleaved,
        channels,
        original_sample_rate,
        sample_buffer,
    )
}

fn process_buffer_i16(
    vol: f32,
    buffer: &symphonia::core::audio::AudioBuffer<i16>,
    sample_buffer: &mut Vec<f32>,
    original_sample_rate: u32,
) -> Result<()> {
    let channels = buffer.spec().channels.count();
    let frames = buffer.frames();

    // Convert to f32 and normalize
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            let sample = buffer.chan(ch)[i] as f32 / 32768.0; // thanks deepseek
            interleaved.push(sample);
        }
    }

    process_interleaved(
        vol,
        &interleaved,
        channels,
        original_sample_rate,
        sample_buffer,
    )
}

// Process i24 buffer
fn process_buffer_i24(
    vol: f32,
    buffer: &symphonia::core::audio::AudioBuffer<i24>,
    sample_buffer: &mut Vec<f32>,
    original_sample_rate: u32,
) -> Result<()> {
    let channels = buffer.spec().channels.count();
    let frames = buffer.frames();

    // Convert to f32 and normalize
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            let sample = buffer.chan(ch)[i].0 as f32 / 8388608.0; // thanks deepseek
            interleaved.push(sample);
        }
    }

    process_interleaved(
        vol,
        &interleaved,
        channels,
        original_sample_rate,
        sample_buffer,
    )
}

// Process i32 buffer
fn process_buffer_i32(
    vol: f32,
    buffer: &symphonia::core::audio::AudioBuffer<i32>,
    sample_buffer: &mut Vec<f32>,
    original_sample_rate: u32,
) -> Result<()> {
    let channels = buffer.spec().channels.count();
    let frames = buffer.frames();

    // Convert to f32 and normalize
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            let sample = buffer.chan(ch)[i] as f32 / 2147483648.0; // thanks deepseek
            interleaved.push(sample);
        }
    }

    process_interleaved(
        vol,
        &interleaved,
        channels,
        original_sample_rate,
        sample_buffer,
    )
}

// Process u8 buffer
fn process_buffer_u8(
    vol: f32,
    buffer: &symphonia::core::audio::AudioBuffer<u8>,
    sample_buffer: &mut Vec<f32>,
    original_sample_rate: u32,
) -> Result<()> {
    let channels = buffer.spec().channels.count();
    let frames = buffer.frames();

    // Convert to f32 and normalize
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            let sample = (buffer.chan(ch)[i] as f32 - 128.0) / 128.0; // thanks deepseek
            interleaved.push(sample);
        }
    }

    process_interleaved(
        vol,
        &interleaved,
        channels,
        original_sample_rate,
        sample_buffer,
    )
}

fn process_interleaved(
    vol: f32,
    interleaved: &[f32],
    channels: usize,
    original_sample_rate: u32,
    sample_buffer: &mut Vec<f32>,
) -> Result<()> {
    // resample if necessary
    let resampled = if original_sample_rate != TARGET_SAMPLE_RATE {
        // println!(
        //     "sample rate mismatch. resampling... [{} -> {}]",
        //     original_sample_rate, TARGET_SAMPLE_RATE
        // );
        resample(
            interleaved,
            original_sample_rate,
            TARGET_SAMPLE_RATE,
            channels,
        )
        .unwrap()
    } else {
        interleaved.to_vec()
    };

    let mut final_samples = if channels == 1 {
        let mut stereo = Vec::with_capacity(resampled.len() * 2);
        for sample in &resampled {
            // mono audio pair for stereo channels
            stereo.push(*sample);
            stereo.push(*sample);
        }
        stereo
    } else if channels == 2 {
        resampled
    } else {
        return Err(anyhow!("unsupported number of channels: {}", channels));
    };

    for sample in &mut final_samples {
        *sample *= vol;
        *sample = sample.clamp(-1.0, 1.0);
    }

    sample_buffer.extend(final_samples);
    Ok(())
}

// i learnt a lot
fn resample(
    interleaved: &[f32],
    from_rate: u32,
    to_rate: u32,
    channels: usize,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    if channels == 0 || interleaved.is_empty() {
        return Ok(interleaved.to_vec());
    }

    // deinterlieve
    let mut deinterleaved: Vec<Vec<f32>> =
        vec![Vec::with_capacity(interleaved.len() / channels); channels];
    for (i, sample) in interleaved.iter().enumerate() {
        deinterleaved[i % channels].push(*sample);
    }

    // resample each channel
    let ratio = to_rate as f64 / from_rate as f64;
    let new_len = (deinterleaved[0].len() as f64 * ratio).ceil() as usize;
    let mut resampled_channels = Vec::with_capacity(channels);

    for channel in deinterleaved {
        let mut resampled = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let pos = i as f64 / ratio;
            let idx = pos as usize;
            let frac = pos - idx as f64;

            if idx < channel.len().saturating_sub(1) {
                let s1 = channel[idx];
                let s2 = channel[idx + 1];
                resampled.push(s1 + (s2 - s1) * (frac as f32));
            } else if !channel.is_empty() {
                resampled.push(*channel.last().unwrap());
            } else {
                resampled.push(0.0);
            }
        }
        resampled_channels.push(resampled);
    }

    // Interleave resampled channels
    let mut resampled_interleaved = Vec::with_capacity(new_len * channels);
    for i in 0..new_len {
        for ch in resampled_channels.iter().take(channels) {
            resampled_interleaved.push(ch[i]);
        }
    }

    Ok(resampled_interleaved)
}

impl Drop for MusicClientState {
    fn drop(&mut self) {
        let _ = self.socket.send(&[0x03]); // EOF packet
    }
}
