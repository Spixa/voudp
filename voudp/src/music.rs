use std::{
    fs::File,
    io::Read,
    net::UdpSocket,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use opus::{Bitrate, Encoder};
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

const TARGET_SAMPLE_RATE: u32 = 48_000;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz
const FRAME_DURATION: Duration = Duration::from_millis(20);
const CHANNELS: usize = 2; // Stereo

pub struct MusicClientState {
    socket: UdpSocket,
    channel_id: u32,
}

impl MusicClientState {
    pub fn new(addr: &str, channel_id: u32) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(addr)?;
        socket.set_nonblocking(true)?;

        Ok(Self { socket, channel_id })
    }

    pub fn run(&mut self, path: String) -> Result<()> {
        let mut join_packet = vec![0x01];
        join_packet.extend_from_slice(&self.channel_id.to_be_bytes());
        self.socket.send(&join_packet)?;
        println!("joined channel {}", self.channel_id);

        let mut opus_encoder = Encoder::new(
            TARGET_SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )?;

        opus_encoder.set_bitrate(Bitrate::Bits(96000))?;

        // open and decode file
        let mut file = File::open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        // stuff for decoding the file
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default()); // cursor implements a Seek
        let hint = Hint::new(); // information
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

        // init sample buffer
        let mut sample_buf = Vec::with_capacity(FRAME_SIZE * CHANNELS * 10); // 10 frames
        let sample_rate = track.codec_params.sample_rate.unwrap_or(TARGET_SAMPLE_RATE);

        // timing stuff:
        let start = Instant::now();
        let mut f_idx = 0; // frame index

        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != track_id {
                continue;
            }

            // holy hell it was a pain to figure all of them out except the first one maybe
            match decoder.decode(&packet)? {
                AudioBufferRef::F32(buf) => process_buffer_f32(&buf, &mut sample_buf, sample_rate)?,
                AudioBufferRef::S16(buf) => process_buffer_i16(&buf, &mut sample_buf, sample_rate)?,
                AudioBufferRef::S24(buf) => process_buffer_i24(&buf, &mut sample_buf, sample_rate)?,
                AudioBufferRef::S32(buf) => process_buffer_i32(&buf, &mut sample_buf, sample_rate)?,
                AudioBufferRef::U8(buf) => process_buffer_u8(&buf, &mut sample_buf, sample_rate)?,
                _ => return Err(anyhow!("unsupported audio buffer type")),
            }

            // this ensures that we are dealing with complete frames every time
            while sample_buf.len() >= FRAME_SIZE * CHANNELS {
                // calculate target time: (frame index * frame duration) + begin offset
                let target_time = start + FRAME_DURATION * f_idx;
                f_idx += 1;

                let frame = &sample_buf[..FRAME_SIZE * CHANNELS];
                let mut opus_frame = vec![0u8; 4000]; // idk deepseek said its a good size

                let len = opus_encoder.encode_float(frame, &mut opus_frame)?;

                // create packet with 0x02 header
                let mut audio_packet = vec![0x02];
                audio_packet.extend_from_slice(&opus_frame[..len]);

                // request upload
                self.upload_packet(&audio_packet)?;

                // remove the samples we read:
                sample_buf.drain(0..FRAME_SIZE * CHANNELS);

                // timing logic:
                let now = Instant::now();
                if now < target_time {
                    std::thread::sleep(target_time - now); // wait until we are back to schedule
                }
            }
        }

        // after this, there is usually samples left that dont fit a whole FRAME_SIZE*CHANNELS. we will pad them:
        if !sample_buf.is_empty() {
            let mut padded = vec![0.0; FRAME_SIZE * CHANNELS];
            let copy_len = sample_buf.len().min(padded.len());
            padded[..copy_len].copy_from_slice(&sample_buf[..copy_len]); // the rest that are untouched are left as 0.0 samples

            let mut opus_frame = vec![0u8; 4000]; // deja vu
            let len = opus_encoder.encode_float(&padded, &mut opus_frame)?;

            let mut packet = vec![0x02u8];
            packet.extend_from_slice(&opus_frame[..len]);
            self.upload_packet(&packet)?;
        }

        Ok(())
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

    process_interleaved(&interleaved, channels, original_sample_rate, sample_buffer)
}

fn process_buffer_i16(
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

    process_interleaved(&interleaved, channels, original_sample_rate, sample_buffer)
}

// Process i24 buffer
fn process_buffer_i24(
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

    process_interleaved(&interleaved, channels, original_sample_rate, sample_buffer)
}

// Process i32 buffer
fn process_buffer_i32(
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

    process_interleaved(&interleaved, channels, original_sample_rate, sample_buffer)
}

// Process u8 buffer
fn process_buffer_u8(
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

    process_interleaved(&interleaved, channels, original_sample_rate, sample_buffer)
}

fn process_interleaved(
    interleaved: &[f32],
    channels: usize,
    original_sample_rate: u32,
    sample_buffer: &mut Vec<f32>,
) -> Result<()> {
    // resample if necessary
    let resampled = if original_sample_rate != TARGET_SAMPLE_RATE {
        println!(
            "sample rate mismatch. resampling... [{} -> {}]",
            original_sample_rate, TARGET_SAMPLE_RATE
        );
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

    let final_samples = if channels == 1 {
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
        for ch in 0..channels {
            resampled_interleaved.push(resampled_channels[ch][i]);
        }
    }

    Ok(resampled_interleaved)
}
