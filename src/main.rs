use anyhow::Result;

use crate::{client::ClientState, server::ServerState};

mod client;
mod mixer;
mod server;
mod util;

fn main() -> Result<()> {
    let result = util::ask("server or client: ");
    match result.as_str() {
        "c" => {
            let mut client = ClientState::new("127.0.0.1:37549")?;
            client.run().unwrap();
        }
        "s" => {
            let mut server = ServerState::new(37549)?;
            server.run();
        }
        _ => {
            eprintln!("write c/s");
        }
    }
    Ok(())
}
