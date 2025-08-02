use anyhow::Result;
use log::error;

use crate::{client::ClientState, music::MusicClientState, server::ServerState};

mod client;
mod mixer;
mod music;
mod server;
mod util;

fn main() -> Result<()> {
    pretty_env_logger::init_timed();

    let result = util::ask("> [s]erver/[c]lient/[m]usic client: ");
    match result.as_str() {
        "c" => {
            let mut client = ClientState::new("127.0.0.1:37549")?;
            client.run()?;
        }
        "s" => {
            let mut server = ServerState::new(37549)?;
            server.run();
        }
        "m" => {
            let path = util::ask("path/to/file to stream: ");
            let mut client = MusicClientState::new("127.0.0.1:37549")?;
            client.run(path)?;
        }
        _ => {
            error!("write c/s/m");
        }
    }
    Ok(())
}
