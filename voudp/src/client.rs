use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus2::{Application, Channels, Decoder, Encoder};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::util::{self, ChannelInfo, SecureUdpSocket, ServerCommand};

const TARGET_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const BUFFER_CAPACITY: usize = TARGET_FRAME_SIZE * 10; // 10 frames

pub enum Mode {
    Repl,
    Gui,
}

pub enum State {
    Fine,
    IncorrectPhraseError,
}

pub struct ClientState {
    pub socket: SecureUdpSocket,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    channel_id: Arc<Mutex<u32>>,
    pub list: SafeChannelList,
    pub talking: Arc<AtomicBool>,
    pub ping: Arc<AtomicU16>,
    pub rx: Option<Receiver<OwnedMessage>>,
    pub state: Arc<Mutex<State>>,
    pub cmd_list: SafeCommandList,
}

type OwnedMessage = (Message, DateTime<Local>);

pub enum Message {
    JoinMessage(String),
    LeaveMessage(String),
    ChatMessage(String, String),
    Renick(String, String),
    Broadcast(String, String),
}

pub struct GlobalListState {
    pub channels: Vec<ChannelInfo>,
    pub last_updated: Instant,
    pub current_channel: u32,
}

type SafeChannelList = Arc<Mutex<GlobalListState>>;
type SafeCommandList = Arc<Mutex<Vec<ServerCommand>>>;

impl ClientState {
    pub fn new(ip: &str, channel_id: u32, phrase: &[u8]) -> Result<Self, io::Error> {
        let key = util::derive_key_from_phrase(phrase, util::VOUDP_SALT);
        let mut socket = SecureUdpSocket::create("0.0.0.0:0".into(), key)?; // let OS decide port

        socket.connect(ip)?;

        Ok(Self {
            socket,
            muted: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(false)),
            connected: Arc::new(AtomicBool::new(true)),
            channel_id: Arc::new(Mutex::new(channel_id)),
            list: Arc::new(Mutex::new(GlobalListState {
                channels: vec![],
                last_updated: Instant::now(),
                current_channel: 0,
            })),
            ping: Arc::new(AtomicU16::new(u16::MAX)),
            talking: Arc::new(AtomicBool::new(false)),
            rx: None,
            state: Arc::new(Mutex::new(State::Fine)),
            cmd_list: Arc::new(Mutex::new(vec![])),
        })
    }

    pub fn join(&self, id: u32) -> Result<usize, std::io::Error> {
        let join_packet = {
            let mut p = vec![0x01];
            p.extend_from_slice(&id.to_be_bytes());
            p
        };

        self.socket.send(&join_packet)
    }

    pub fn run(&mut self, mode: Mode) -> Result<()> {
        let socket = self.socket.clone();
        let muted = self.muted.clone();
        let deafened = self.deafened.clone();
        let connected = self.connected.clone();
        let list = self.list.clone();
        let cmd_list = self.cmd_list.clone();
        let state = self.state.clone();
        let talking = self.talking.clone();
        let (tx, rx) = mpsc::channel::<OwnedMessage>();
        let ping = self.ping.clone();

        self.rx = Some(rx);
        let id = { self.channel_id.lock().unwrap() };
        match mode {
            Mode::Repl => {
                self.join(*id)?;
                Self::start_audio(
                    socket, muted, deafened, connected, state, list, cmd_list, tx, mode, talking,
                    ping,
                )?;
            }
            Mode::Gui => {
                let join_packet = {
                    let mut p = vec![0x01];
                    p.extend_from_slice(&id.to_be_bytes());
                    p
                };
                thread::spawn(move || {
                    if let Err(e) = socket.send(&join_packet) {
                        eprintln!("send error: {e:?}");
                        return;
                    }
                    if let Err(e) = Self::start_audio(
                        socket, muted, deafened, connected, state, list, cmd_list, tx, mode,
                        talking, ping,
                    ) {
                        eprintln!("audio thread error: {e:?}");
                    }
                });
                return Ok(()); // return immediately in GUI mode
            }
        }

        Ok(())
    }

    fn start_audio(
        socket: SecureUdpSocket,
        muted: Arc<AtomicBool>,
        deafened: Arc<AtomicBool>,
        connected: Arc<AtomicBool>,
        state: Arc<Mutex<State>>,
        list: SafeChannelList,
        cmd_list: SafeCommandList,
        tx: Sender<OwnedMessage>,
        mode: Mode,
        talking: Arc<AtomicBool>,
        ping: Arc<AtomicU16>,
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
            let socket = socket.clone();
            let input_clone = Arc::clone(&input_buffer);
            let output_clone = Arc::clone(&output_buffer);
            let connected_clone = Arc::clone(&connected);
            let muted_clone = Arc::clone(&muted);
            let state_clone = Arc::clone(&state);
            let list = list.clone();
            let cmd_list = cmd_list.clone();
            let ping = ping.clone();
            thread::spawn(move || {
                Self::network_thread(
                    socket,
                    input_clone,
                    output_clone,
                    list,
                    tx,
                    connected_clone,
                    state_clone,
                    cmd_list,
                    muted_clone,
                    ping,
                )
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

                    let sum_sq: f32 = buffer.iter().map(|s| s * s).sum();
                    let rms = (sum_sq / buffer.len() as f32).sqrt();

                    if rms < 0.07 {
                        talking.store(false, Ordering::Relaxed);
                    } else {
                        talking.store(true, Ordering::Relaxed);
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
        socket: SecureUdpSocket,
        input: Arc<Mutex<VecDeque<f32>>>,
        output: Arc<Mutex<VecDeque<f32>>>,
        list: SafeChannelList,
        tx: Sender<OwnedMessage>,
        connected: Arc<AtomicBool>,
        state: Arc<Mutex<State>>,
        cmd_list: SafeCommandList,
        muted: Arc<AtomicBool>,
        ping: Arc<AtomicU16>,
    ) {
        let mut encoder = Encoder::new(48000, Channels::Stereo, Application::Audio).unwrap();
        let mut decoder = Decoder::new(48000, Channels::Stereo).unwrap();
        encoder.set_bitrate(opus2::Bitrate::Bits(96000)).unwrap();

        let mut recv_buf = [0u8; 2048];
        let mut frame_buf = vec![0.0f32; TARGET_FRAME_SIZE * 2];

        let mut test = Instant::now();
        let mut ping_reply = Instant::now();
        loop {
            if !connected.load(Ordering::Relaxed) {
                break;
            }

            // send
            if test.elapsed() > Duration::from_secs(1) {
                let list_packet = vec![0x05];
                let cmd_list_packet = vec![0x0c];
                socket.send(&list_packet).unwrap();
                socket.send(&cmd_list_packet).unwrap();
                test = Instant::now();
                ping_reply = Instant::now();
            }

            {
                let mut buffer = input.lock().unwrap();
                let muted = muted.load(Ordering::Relaxed);
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
                    if !muted && let Ok(len) = encoder.encode_float(&frame_buf, &mut opus_data) {
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
                    if let Ok(decoded) = decoder.decode_float(&recv_buf[1..size], &mut pcm, false)
                        && decoded > 0
                    {
                        let mut buffer = output.lock().unwrap();
                        for s in &pcm[..(decoded * 2)] {
                            if buffer.len() >= BUFFER_CAPACITY * 2 {
                                buffer.pop_front();
                            }
                            buffer.push_back(*s);
                        }
                    }
                }
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x05 => {
                    let packet = &recv_buf[..size];
                    let Some(parsed) = util::parse_global_list(&packet[1..]) else {
                        eprintln!("error: Received bad list");
                        continue;
                    };

                    {
                        let mut list = list.lock().unwrap();
                        list.channels = parsed.0;
                        list.current_channel = parsed.1;
                        list.last_updated = Instant::now();

                        ping.store(
                            Instant::now().duration_since(ping_reply).as_millis() as u16,
                            Ordering::Relaxed,
                        );
                    }
                }
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x06 => {
                    match util::parse_msg_packet(&recv_buf[..size]) {
                        Ok((username, text)) => {
                            let _ = tx.send((Message::ChatMessage(username, text), Local::now()));
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                        }
                    }
                }
                Ok((size, _))
                    if size > 1
                        && (recv_buf[0] == 0x0a
                            || recv_buf[0] == 0x0b
                            || recv_buf[0] == 0x10
                            || recv_buf[0] == 0x11) =>
                {
                    if let Some(msg) = util::parse_flow_packet(&recv_buf[..size]) {
                        let _ = tx.send((msg, Local::now()));
                    }
                }
                Ok((size, _)) if size > 1 && recv_buf[0] == 0x0c => {
                    if let Some(commands) = util::parse_command_list(&recv_buf[1..size]) {
                        let mut list = cmd_list.lock().unwrap();
                        *list = commands;
                    }
                }
                Ok((_, _)) => {}
                Err(e) if e.0.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) if e.0.kind() == io::ErrorKind::Unsupported => {
                    connected.store(false, Ordering::Relaxed);
                    {
                        let mut state = state.lock().unwrap();
                        *state = State::IncorrectPhraseError;
                    }
                    break;
                }
                Err(_) => break,
            }
            thread::sleep(Duration::from_micros(100));
        }
    }

    fn repl(
        socket: SecureUdpSocket,
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

                    let mut mute_packet = vec![0x08];
                    let mode = if new { 0x03 } else { 0x04 };
                    mute_packet.extend_from_slice(&[mode]);
                    let _ = socket.send(&mute_packet);

                    println!("microphone {}muted", if new { "" } else { "un" });
                }
                "d" | "deaf" => {
                    let new = !deafened.load(Ordering::Relaxed);
                    deafened.store(new, Ordering::Relaxed);

                    let mut deaf_packet = vec![0x08];
                    let mode = if new { 0x01 } else { 0x02 };
                    deaf_packet.extend_from_slice(&[mode]);
                    let _ = socket.send(&deaf_packet);

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
                    println!("Latest global list:");
                    for ch in &list.channels {
                        println!("-> Channel {}", ch.channel_id);
                        println!(
                            "\tUnmasked: {} -- Masked: {}",
                            ch.unmasked_count,
                            ch.masked_users.len()
                        );

                        if !ch.masked_users.is_empty() {
                            println!("\tMasked list: ");

                            for person in ch.masked_users.iter() {
                                println!(
                                    "\t ● {} (Muted: {}) (Deafened: {})",
                                    person.0, person.1, person.2
                                );
                            }
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
        let mut mute_packet = vec![0x08];
        let mode = if muted { 0x03 } else { 0x04 };
        mute_packet.extend_from_slice(&[mode]);
        self.send(&mute_packet);

        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_deafened(&self, deafened: bool) {
        let mut deaf_packet = vec![0x08];
        let mode = if deafened { 0x01 } else { 0x02 };
        deaf_packet.extend_from_slice(&[mode]);
        self.send(&deaf_packet);

        self.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn disconnect(&self) {
        let leave = vec![0x03];
        self.socket.send(&leave).unwrap();

        self.connected.store(false, Ordering::Relaxed);
    }

    pub fn send(&self, packet: &[u8]) {
        let _ = self.socket.send(packet);
    }

    pub fn send_command(&self, command: &str) {
        let mut packet = vec![0x0d];
        packet.extend_from_slice(command.as_bytes());
        let _ = self.socket.send(&packet);
    }
}
