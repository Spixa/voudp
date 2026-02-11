// console_commands.rs
use crate::server::{Channel, ServerConfig};
use crate::util::SecureUdpSocket;

pub enum ConsoleCommandResult {
    Reply(String),
}

pub fn handle_command(
    cmd: &str,
    parts: &[&str],
    channels: &mut std::collections::HashMap<u32, Channel>,
    config: &ServerConfig,
    _socket_sender: Option<&mut SecureUdpSocket>,
) -> ConsoleCommandResult {
    match cmd {
        "help" => ConsoleCommandResult::Reply("you are connected to a voudp 0.1 server".into()),
        "ping" => ConsoleCommandResult::Reply("pong".into()),
        "list" => {
            ConsoleCommandResult::Reply("global list cannot be displayed with crossterm".into())
        }
        "rename" => {
            if parts.len() < 3 {
                ConsoleCommandResult::Reply("usage: rename <channel> <new-name>".to_string())
            } else {
                let ident = parts[1];
                let new_name = parts[2..].join(" ");

                let channel_opt = channels
                    .iter_mut()
                    .find(|(_, c)| c.name.as_deref() == Some(ident));

                match channel_opt {
                    Some((_key, channel)) => {
                        let old_name = channel
                            .name
                            .replace(new_name.clone())
                            .unwrap_or_else(|| "unnamed".into());

                        log::info!("Channel '{}' renamed to '{}'", old_name, new_name);
                        ConsoleCommandResult::Reply(format!(
                            "renamed channel '{}' -> '{}'",
                            old_name, new_name
                        ))
                    }
                    None => ConsoleCommandResult::Reply(format!("channel '{}' not found", ident)),
                }
            }
        }
        "chans" => {
            let s = channels
                .iter()
                .map(|(id, channel)| {
                    format!(
                        "{} ({})",
                        channel.name.clone().unwrap_or_else(|| "unnamed".into()),
                        id
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            ConsoleCommandResult::Reply(s)
        }
        "create" => {
            if parts.len() < 2 {
                ConsoleCommandResult::Reply("usage: create <channel_name>".into())
            } else {
                let name = parts[1..].join(" ");
                let new_id = channels.keys().max().map_or(1, |id| id + 1);
                channels.insert(new_id, Channel::new(*config, name.clone(), new_id));
                ConsoleCommandResult::Reply(format!(
                    "created channel '{}' with id {} ({}kHz)",
                    name,
                    new_id,
                    config.sample_rate as f64 / 1000.0
                ))
            }
        }
        "del" => {
            if parts.len() < 2 {
                ConsoleCommandResult::Reply("usage: del <channel_id|channel_name>".into())
            } else {
                let target = parts[1];
                let maybe_channel_id = target.parse::<u32>().ok();

                let channel_id_to_delete = if let Some(id) = maybe_channel_id {
                    if id == 1 { None } else { Some(id) }
                } else {
                    channels
                        .iter()
                        .find(|(_, c)| c.name.as_deref() == Some(target))
                        .map(|(id, _)| *id)
                };

                if let Some(channel_id) = channel_id_to_delete {
                    if channel_id == 1 {
                        ConsoleCommandResult::Reply(
                            "cannot delete the default channel defined by the voudp protocol"
                                .into(),
                        )
                    } else if let Some(channel) = channels.remove(&channel_id) {
                        // Notify users
                        for remote in channel.remotes.iter() {
                            if let Ok(remote) = remote.lock() {
                                // If you need to send messages, you'll need to handle this differently
                                // Option 1: Return the notifications in ConsoleCommandResult
                                // Option 2: Pass a callback/sender
                                log::info!("Would notify {} about channel deletion", remote.addr);
                            }
                        }

                        ConsoleCommandResult::Reply(format!(
                            "deleted channel '{}' (id {}) and moved users to default",
                            channel.name.unwrap_or_else(|| "unknown".into()),
                            channel_id
                        ))
                    } else {
                        ConsoleCommandResult::Reply("channel not found".into())
                    }
                } else {
                    ConsoleCommandResult::Reply("channel not found".into())
                }
            }
        }
        _ => ConsoleCommandResult::Reply(
            "unknown command. read the manual on executing remote commands".into(),
        ),
    }
}
