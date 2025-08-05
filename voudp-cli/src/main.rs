use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use log::Level;
use pretty_env_logger::env_logger::fmt::Color;
use std::io::Write;

use voudp::{
    client::{self, ClientState},
    music::MusicClientState,
    server::{Clipping, ServerConfig, ServerState},
};

/// A lightweight UDP VoIP system with server/client/music modes
#[derive(Parser)]
#[clap(
    name = "voudp",
    version = "0.1",
    author = "spixa",
    about = "A VoIP server/client using UDP"
)]
struct Cli {
    #[clap(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Start the VoIP server
    Server {
        /// Port to bind the server on (required)
        #[clap(long)]
        port: u16,

        /// Maximum allowed users
        #[clap(long, default_value_t = 1024)]
        max_users: usize,

        /// Whether to normalize incoming audio
        #[clap(long)]
        no_normalize: bool,

        /// Whether to apply compression
        #[clap(long)]
        no_compress: bool,

        /// Compression threshold
        #[clap(long, default_value_t = 0.5)]
        compress_threshold: f32,

        /// Compression ratio
        #[clap(long, default_value_t = 0.8)]
        compress_ratio: f32,

        /// Use hard clipping instead of soft
        #[clap(long)]
        hard_clip: bool,

        /// Idle timeout in seconds
        #[clap(long, default_value_t = 5)]
        timeout_secs: u64,

        /// Main loop throttle in milliseconds
        #[clap(long, default_value_t = 1)]
        throttle_millis: u64,

        /// Sample rate (Hz)
        #[clap(long, default_value_t = 48000)]
        sample_rate: u32,

        /// Tickrate (ticks per second)
        #[clap(long, default_value_t = 50)]
        tickrate: u32,
    },

    /// Start a client that captures and streams microphone audio
    Client {
        /// Address to connect to (e.g., 127.0.0.1:37549)
        #[clap(long)]
        connect: String,

        /// ID of the channel to connect to
        #[clap(long, default_value_t = 1)]
        channel_id: u32,
    },

    /// Start a client that streams audio from a file
    Music {
        /// Address to connect to
        #[clap(long)]
        connect: String,

        /// ID of the channel to connect to
        #[clap(long, default_value_t = 1)]
        channel_id: u32,

        /// Path to file to stream
        #[clap(long)]
        file: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.mode {
        Mode::Client {
            connect,
            channel_id,
        } => {
            let mut client = ClientState::new(&connect, channel_id)?;
            client.run(client::Mode::Repl)?;
        }

        Mode::Music {
            connect,
            channel_id,
            file,
        } => {
            let mut client = MusicClientState::new(&connect, channel_id)?;
            client.run(file)?;
        }

        Mode::Server {
            port,
            max_users,
            no_normalize,
            no_compress,
            compress_threshold,
            compress_ratio,
            hard_clip,
            timeout_secs,
            throttle_millis,
            sample_rate,
            tickrate,
        } => {
            let config = ServerConfig {
                bind_port: port,
                max_users,
                should_normalize: !no_normalize,
                should_compress: !no_compress,
                compress_threshold,
                compress_ratio,
                clipping: if hard_clip {
                    Clipping::Hard
                } else {
                    Clipping::Soft
                },
                timeout_secs,
                throttle_millis,
                sample_rate,
                tickrate,
            };
            init_logger();
            let mut server = ServerState::new(config)?;
            server.run();
        }
    }

    Ok(())
}

fn init_logger() {
    pretty_env_logger::formatted_builder()
        .format(|buf, record| {
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");

            let mut style = buf.style();
            let level = match record.level() {
                Level::Error => style.set_color(Color::Red).set_bold(true),
                Level::Warn => style.set_color(Color::Yellow).set_bold(true),
                Level::Info => style.set_color(Color::Green).set_bold(true),
                Level::Debug => style.set_color(Color::Blue),
                Level::Trace => style.set_color(Color::Magenta),
            };

            let mut style = buf.style();
            let ts_style = style.set_color(Color::Rgb(128, 128, 128)).set_dimmed(true);

            writeln!(
                buf,
                "{} [{}] {}",
                ts_style.value(timestamp),
                level.value(record.level()),
                record.args()
            )
        })
        .filter_level(log::LevelFilter::Info)
        .parse_default_env() // allows RUST_LOG to still override it
        .init();
}
