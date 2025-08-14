use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::{Application, Channels, Decoder, Encoder};
use std::collections::VecDeque;
use std::io;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::util;

const TARGET_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const BUFFER_CAPACITY: usize = TARGET_FRAME_SIZE * 10; // 10 frames

pub enum Mode {
    Repl,
    Gui,
}

pub struct ClientState {
    socket: UdpSocket,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    channel_id: Arc<Mutex<u32>>,
    pub list: SafeChannelList,
    pub rx: Option<Receiver<OwnedMessage>>,
}

type OwnedMessage = (String, String, DateTime<Local>);

#[derive(Default)]
pub struct ChannelList {
    pub unmasked: u32,
    pub masked: Vec<String>,
}

type SafeChannelList = Arc<Mutex<ChannelList>>;

impl ClientState {
    pub fn new(ip: &str, channel_id: u32) -> Result<Self, io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0")?; // let OS decide port
        socket.connect(ip)?;
        socket.set_nonblocking(true)?;

        Ok(Self {
            socket,
            muted: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(false)),
            connected: Arc::new(AtomicBool::new(true)),
            channel_id: Arc::new(Mutex::new(channel_id)),
            list: Default::default(),
            rx: None,
        })
    }

    pub fn run(&mut self, mode: Mode) -> Result<()> {
        let join_packet = {
            let id = self.channel_id.lock().unwrap();
            let mut p = vec![0x01];
            p.extend_from_slice(&id.to_be_bytes());
            p
        };

        let socket = self.socket.try_clone()?;
        let muted = self.muted.clone();
        let deafened = self.deafened.clone();
        let connected = self.connected.clone();
        let list = self.list.clone();

        let (tx, rx) = mpsc::channel::<OwnedMessage>();

        self.rx = Some(rx);
        match mode {
            Mode::Repl => {
                self.socket.send(&join_packet)?;
                Self::start_audio(socket, muted, deafened, connected, list, tx, mode)?;
            }
            Mode::Gui => {
                thread::spawn(move || {
                    if let Err(e) = socket.send(&join_packet) {
                        eprintln!("send error: {e:?}");
                        return;
                    }
                    if let Err(e) =
                        Self::start_audio(socket, muted, deafened, connected, list, tx, mode)
                    {
                        eprintln!("audio thread error: {e:?}");
                    }
                });
                return Ok(()); // return immediately in GUI mode
            }
        }

        Ok(())
    }

    fn start_audio(
        socket: UdpSocket,
        muted: Arc<AtomicBool>,
        deafened: Arc<AtomicBool>,
        connected: Arc<AtomicBool>,
        list: SafeChannelList,
        tx: Sender<OwnedMessage>,
        mode: Mode,
    ) -> Result<()> {
        let muted_clone = muted.clone();
        let deafened_clone = deafened.clone();

        let input_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
            BUFFER_CAPACITY * 2,
        )));
        let output_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
            BUFFER_CAPACITY * 2,
        )));

        // spawn network thread
        {
            let socket = socket.try_clone()?;
            let input_clone = Arc::clone(&input_buffer);
            let output_clone = Arc::clone(&output_buffer);
            let connected_clone = Arc::clone(&connected);
            let list = list.clone();
            thread::spawn(move || {
                Self::network_thread(socket, input_clone, output_clone, list, tx, connected_clone)
            });
        }

        let host = cpal::default_host();
        let input_device = host.default_input_device().context("no input device")?;
        let output_device = host.default_output_device().context("no output device")?;

        let supported = input_device.supported_input_configs()?;

        let config_range = supported
            .filter(|c| c.min_sample_rate().0 <= 48000 && c.max_sample_rate().0 >= 48000)
            .find(|c| c.sample_format() == cpal::SampleFormat::F32)
            .ok_or_else(|| anyhow::anyhow!("No supported config with 48kHz and f32 format"))?;

        let channels = config_range.channels();
        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let input_clone = Arc::clone(&input_buffer);
        let input_stream = input_device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mut buffer = input_clone.lock().unwrap();
                    if channels == 1 {
                        for sample in data {
                            if buffer.len() >= BUFFER_CAPACITY * 2 {
                                buffer.pop_front();
                                buffer.pop_front();
                            }

                            if !muted.load(Ordering::Relaxed) {
                                let processed = (sample * 0.8).tanh();
                                buffer.push_back(processed);
                                buffer.push_back(processed);
                            } else {
                                buffer.push_back(0.0);
                                buffer.push_back(0.0);
                            }
                        }
                    } else if channels == 2 {
                        for sample in data {
                            if buffer.len() >= BUFFER_CAPACITY {
                                buffer.pop_front();
                            }

                            if !muted.load(Ordering::Relaxed) {
                                let processed = (sample * 0.8).tanh();
                                buffer.push_back(processed);
                            } else {
                                buffer.push_back(0.0);
                            }
                        }
                    }
                },
                |err| eprintln!("input stream error: {err:?}"),
                None,
            )
            .context("building input stream failed")?;

        let output_config = cpal::StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let output_clone = Arc::clone(&output_buffer);
        let output_stream = output_device
            .build_output_stream(
                &output_config,
                move |data: &mut [f32], _| {
                    let mut buffer = output_clone.lock().unwrap();
                    for sample in data {
                        *sample = if !deafened.load(Ordering::Relaxed) {
                            buffer.pop_front().unwrap_or(0.0)
                        } else {
                            0.0
                        };
                    }
                },
                |err| eprintln!("output stream error: {err:?}"),
                None,
            )
            .context("building output stream failed")?;

        input_stream.play()?;
        output_stream.play()?;

        match mode {
            Mode::Gui => {
                while connected.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            }
            Mode::Repl => {
                let list = list.clone();
                Self::repl(socket, muted_clone, deafened_clone, list)
            }
        }
    }

    fn network_thread(
        socket: UdpSocket,
        input: Arc<Mutex<VecDeque<f32>>>,
        output: Arc<Mutex<VecDeque<f32>>>,
        list: SafeChannelList,
        tx: Sender<OwnedMessage>,
        connected: Arc<AtomicBool>,
    ) {
        let mut encoder = Encoder::new(48000, Channels::Stereo, Application::Audio).unwrap();
        let mut decoder = Decoder::new(48000, Channels::Stereo).unwrap();
        encoder.set_bitrate(opus::Bitrate::Bits(96000)).unwrap();

        let mut recv_buf = [0u8; 2048];
        let mut frame_buf = vec![0.0f32; TARGET_FRAME_SIZE * 2];

        let mut test = Instant::now();
        loop {
            if !connected.load(Ordering::Relaxed) {
                break;
            }

            // send
            if test.elapsed() > Duration::from_secs(1) {
                let list_packet = vec![0x05];
                socket.send(&list_packet).unwrap();
                test = Instant::now();
            }

            {
                let mut buffer = input.lock().unwrap();
                while buffer.len() >= TARGET_FRAME_SIZE * 2 {
                    for i in 0..TARGET_FRAME_SIZE {
                        frame_buf[i * 2] = buffer.pop_front().unwrap_or(0.0);
                        frame_buf[i * 2 + 1] = buffer.pop_front().unwrap_or(0.0);
                    }

                    for s in &mut frame_buf {
                        if s.abs() < 0.001 {
                            *s = 0.0;
                        }
                    }

                    let mut opus_data = vec![0u8; 400];
                    if let Ok(len) = encoder.encode_float(&frame_buf, &mut opus_data) {
                        let mut packet = vec![0x02];
                        packet.extend_from_slice(&opus_data[..len]);
                        let _ = socket.send(&packet);
                    }
                }
            }

            // receive
            match socket.recv_from(&mut recv_buf) {
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x02 => {
                    let mut pcm = vec![0.0f32; TARGET_FRAME_SIZE * 2];
                    if let Ok(decoded) = decoder.decode_float(&recv_buf[1..size], &mut pcm, false) {
                        if decoded > 0 {
                            let mut buffer = output.lock().unwrap();
                            for s in &pcm[..(decoded * 2)] {
                                if buffer.len() >= BUFFER_CAPACITY * 2 {
                                    buffer.pop_front();
                                }
                                buffer.push_back(*s);
                            }
                        }
                    }
                }
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x05 => {
                    let packet = &recv_buf[..size];
                    let Some(parsed) = util::parse_list_packet(packet) else {
                        println!("error: Received bad list");
                        continue;
                    };

                    {
                        let mut list = list.lock().unwrap();
                        list.masked = parsed.2;
                        list.unmasked = parsed.0;
                    }
                }
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x06 => {
                    match util::parse_msg_packet(&recv_buf[..size]) {
                        Ok((username, text)) => {
                            let _ = tx.send((username, text, Local::now()));
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                        }
                    }
                }
                Ok((_, _)) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
            thread::sleep(Duration::from_micros(100));
        }
    }

    fn repl(
        socket: UdpSocket,
        muted: Arc<AtomicBool>,
        deafened: Arc<AtomicBool>,
        list: SafeChannelList,
    ) -> Result<()> {
        loop {
            let prompt = util::ask("> ");
            let (cmd, arg) = prompt.split_once(' ').unwrap_or((prompt.as_str(), ""));
            print!(":: ");
            match cmd.to_lowercase().as_str() {
                "q" | "quit" => {
                    println!("goodbye!");
                    break;
                }
                "m" | "mute" => {
                    let new = !muted.load(Ordering::Relaxed);
                    muted.store(new, Ordering::Relaxed);
                    println!("microphone {}muted", if new { "" } else { "un" });
                }
                "d" | "deaf" => {
                    let new = !deafened.load(Ordering::Relaxed);
                    deafened.store(new, Ordering::Relaxed);
                    println!("speaker {}deafened", if new { "" } else { "un" });
                }
                "s" | "send" => {
                    if arg.is_empty() {
                        println!("empty will not be sent!");
                        continue;
                    }

                    let mut msg_packet = vec![0x06];
                    msg_packet.extend_from_slice(arg.as_bytes());
                    let _ = socket.send(&msg_packet);
                    println!();
                }
                "n" | "nick" => {
                    if arg.is_empty() {
                        println!("no nick provided!");
                        continue;
                    }
                    let mut nick_packet = vec![0x04];
                    nick_packet.extend_from_slice(arg.as_bytes());
                    let _ = socket.send(&nick_packet);
                    println!("you are now masked as '{}'", arg);
                }
                "l" | "list" => {
                    let list = list.lock().unwrap();
                    println!(
                        "Unmasked: {} -- Masked: {}",
                        list.unmasked,
                        list.masked.len()
                    );

                    if list.masked.is_empty() {
                        println!("Masked list: ");

                        for person in list.masked.iter() {
                            println!("\t● {person}");
                        }
                    }
                }
                "h" | "help" => {
                    println!("possible commands");
                    let content = String::from_utf8(include_bytes!("help.txt").to_vec())?;
                    for line in content.lines() {
                        println!("\t{}", line);
                    }
                }
                _ => println!("unknown command. type 'h' for help"),
            }
        }

        let leave_packet = vec![0x03];
        let _ = socket.send(&leave_packet);
        Ok(())
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn disconnect(&self) {
        let leave = vec![0x03];
        self.socket.send(&leave).unwrap();

        self.connected.store(false, Ordering::Relaxed);
    }
}
