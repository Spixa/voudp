use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::{Application, Channels, Decoder, Encoder};
use std::collections::VecDeque;
use std::io;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::util;

const CHANNEL_ID: u32 = 1;
const TARGET_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const BUFFER_CAPACITY: usize = TARGET_FRAME_SIZE * 10; // 10 frames

pub struct ClientState {
    socket: UdpSocket,
}

impl ClientState {
    pub fn new(ip: &str) -> Result<Self, io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0")?; // let os decide the port
        socket.connect(ip)?;
        socket.set_nonblocking(true)?;

        Ok(Self { socket })
    }

    pub fn run(&mut self) -> Result<()> {
        // join packet
        let mut join_packet = vec![0x01];
        join_packet.extend_from_slice(&CHANNEL_ID.to_be_bytes());
        self.socket.send(&join_packet)?;

        // audio buffers are double ended queues that stage BUFFER_CAPACITY/TARGET_FRAME_SIZE frames ahead for headroom (2x the cap for stereo)
        let input_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
            BUFFER_CAPACITY * 2,
        )));
        let output_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
            BUFFER_CAPACITY * 2,
        )));

        // network thread stuff
        let socket_clone = self.socket.try_clone()?;
        let input_net = Arc::clone(&input_buffer);
        let output_net = Arc::clone(&output_buffer);
        thread::spawn(move || ClientState::network_thread(socket_clone, input_net, output_net));

        // audio processing
        let host = cpal::default_host();
        let input_device = host.default_input_device().expect("no input device");
        let output_device = host.default_output_device().expect("no output device");
        let muted = Arc::new(AtomicBool::new(false));
        let muted_clone = muted.clone();
        let deafened = Arc::new(AtomicBool::new(false));
        let deafened_clone = deafened.clone();

        let config = cpal::StreamConfig {
            channels: 1, // mono input but will turn into stereo
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
            /*
                used to do `BufferSize::Fixed(TARGET_FRAME_SIZE)`` but a microphone (including mine) might not even support fixed buffer sizes of say 960
                so now we just request the default settings for buffer size
                using the vector double ended queues we keep all the audio together to ensure we take 960 bytes each time for a frame

                the input logic (our microphone) uses cpal's callback to upload 960 bytes to the VecDeque and uses the network thread to encode 960 of them for each send
                the output logic (our speakers) uses the network thread to receive opus data and upload it to the VecDeque, decodes it and cpal's callback takes them out one by one
            */
        };

        // input stream (microphone)
        let input_buffer_clone = Arc::clone(&input_buffer);
        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                // we will be uploading to the input buffer
                let mut buffer = input_buffer_clone.lock().unwrap();

                for sample in data {
                    if buffer.len() >= BUFFER_CAPACITY * 2 {
                        // remove oldest sample to make space
                        buffer.pop_front();
                        buffer.pop_front();
                    }

                    if !muted.load(Ordering::Relaxed) {
                        // pcm samples when we arent muted
                        let loudness: f32 = 0.8; // this can be adjusted later
                        let processed = (sample * loudness).tanh();

                        // write both for left and right (mono -> stereo)
                        buffer.push_back(processed);
                        buffer.push_back(processed);
                    } else {
                        // muted (write 0.0 pair)
                        buffer.push_back(0.0);
                        buffer.push_back(0.0);
                    }
                }
            },
            |err| eprintln!("audio input error: {:?}", err),
            None,
        )?;

        // output stream (stereo)
        let output_config = cpal::StreamConfig {
            channels: 2, // stereo output
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        // output stream
        let output_buffer_clone = Arc::clone(&output_buffer);
        let output_stream = output_device.build_output_stream(
            &output_config,
            move |data: &mut [f32], _| {
                let mut buffer = output_buffer_clone.lock().unwrap();
                for sample in data.iter_mut() {
                    // write our samples that cpal will be playing for us like so:
                    *sample = if !deafened.load(Ordering::Relaxed) {
                        buffer.pop_front().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                }
            },
            |err| eprintln!("audio output error: {:?}", err),
            None,
        )?;

        // start streams
        output_stream.play()?;
        input_stream.play()?;

        println!(":: type q to exit");

        loop {
            // repl type beat
            let prompt = util::ask("> ");
            let prompt = prompt.to_lowercase();
            let (cmd, arg) = prompt.split_once(' ').unwrap_or((prompt.as_str(), ""));

            print!(":: ");
            match cmd {
                "q" | "quit" => {
                    println!("goodbye! ");
                    break;
                }
                "m" | "mute" => {
                    let new_state = !muted_clone.load(Ordering::Relaxed);
                    muted_clone.store(new_state, Ordering::Relaxed);

                    let un = if new_state { "" } else { "un" };
                    println!("microphone {}muted", un);
                }
                "d" | "deaf" => {
                    let new_state = !deafened_clone.load(Ordering::Relaxed);
                    deafened_clone.store(new_state, Ordering::Relaxed);

                    let un = if new_state { "" } else { "un" };
                    println!("speaker {}deafened", un);
                }
                "n" | "nick" => {
                    if arg.is_empty() {
                        println!("no nick provided!");
                        continue;
                    }

                    let mut nick_packet = vec![0x04];
                    nick_packet.extend_from_slice(arg.as_bytes());
                    self.socket
                        .send(&nick_packet)
                        .expect("failed to send leave packet");

                    println!("you are now masked as '{}'", arg);
                }
                "h" | "help" => {
                    println!("possible commands");
                    let content = String::from_utf8(include_bytes!("help.txt").to_vec())?;

                    for line in content.lines() {
                        println!("\t{}", line);
                    }
                }
                _ => {
                    println!("unknown command. type 'h' for help");
                }
            }
        }

        // send leave after leave, otherwise the server will time us out which also works ig
        let leave_packet = vec![0x03];
        self.socket
            .send(&leave_packet)
            .expect("failed to send leave packet");
        Ok(())
    }
    fn network_thread(
        socket: UdpSocket,
        input_buffer: Arc<Mutex<VecDeque<f32>>>,
        output_buffer: Arc<Mutex<VecDeque<f32>>>,
    ) {
        // create Opus encoder/decoder
        let mut encoder = Encoder::new(48000, Channels::Stereo, Application::Audio).unwrap();
        let mut decoder = Decoder::new(48000, Channels::Stereo).unwrap();

        // set encoder options
        encoder
            .set_bitrate(opus::Bitrate::Bits(128000))
            .expect("couldn't set bitrate"); // 128 kbps is good (64kbps each OpusChannel)

        // the buffers for sending and receiving
        let mut recv_buf = [0u8; 2048]; // stereo audio from server (2048 byte datagram)
        let mut frame_buf = vec![0.0f32; TARGET_FRAME_SIZE * 2]; // stereo audio we will send

        loop {
            // send audio to server (from input buffer)
            {
                let mut buffer = input_buffer.lock().unwrap();
                while buffer.len() >= TARGET_FRAME_SIZE * 2 {
                    // the bufffer contains multiple frames. lets start by taking them out one by one
                    // take a frame
                    for i in 0..TARGET_FRAME_SIZE {
                        let left = buffer.pop_front().unwrap_or(0.0);
                        let right = buffer.pop_front().unwrap_or(0.0);
                        frame_buf[i * 2] = left;
                        frame_buf[i * 2 + 1] = right;
                    }

                    for sample in frame_buf.iter_mut() {
                        // Noise gate - reduce very quiet sounds
                        if sample.abs() < 0.001 {
                            *sample = 0.0;
                        }
                    }

                    // encode the frame we took out, this time it has no excuse to cause opus invalid arguments error
                    let mut opus_data = vec![0u8; 400];
                    let len = match encoder.encode_float(&frame_buf, &mut opus_data) {
                        Ok(len) => len,
                        Err(e) => {
                            eprintln!("encode error: {:?}", e);
                            0
                        }
                    };

                    // sending packet logic:
                    if len > 0 {
                        // build packet: [0x02] + [opus data]
                        let mut packet = vec![0x02];
                        packet.extend_from_slice(&opus_data[..len]);
                        if let Err(e) = socket.send(&packet) {
                            eprintln!("send error: {}", e);
                        }
                    }
                }
            }

            // receive audio from server
            match socket.recv_from(&mut recv_buf) {
                Ok((size, _)) => {
                    if size > 1 && recv_buf[0] == 0x02 {
                        let mut pcm = vec![0.0f32; TARGET_FRAME_SIZE * 2 /* stereo */];
                        match decoder.decode_float(&recv_buf[1..size], &mut pcm, false) {
                            Ok(decoded) => {
                                if decoded > 0 {
                                    // push to output buffer
                                    let mut buffer = output_buffer.lock().unwrap();
                                    for sample in &pcm[..(decoded * 2/* stereo */)] {
                                        if buffer.len() >= BUFFER_CAPACITY * 2
                                        /* stereo */
                                        {
                                            buffer.pop_front();
                                        }
                                        buffer.push_back(*sample);
                                    }
                                }
                            }
                            Err(e) => eprintln!("decode error: {:?}", e),
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => {
                    eprintln!("recv error: {:?}", e);
                    break;
                }
            }

            // idk apparently good practice
            thread::sleep(Duration::from_micros(100));
        }
    }
}
