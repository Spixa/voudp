use std::io;
use std::io::Write;
use std::net::SocketAddr;

use crate::client::Message;
use crate::protocol::{ClientPacketType, CommandResultPacketType, ControlRequest, IntoPacket};

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub name: String,
    pub channel_id: u32,
    pub unmasked_count: u32,
    pub masked_users: Vec<(String, bool, bool)>,
}

#[derive(Debug, Clone)]
pub struct ServerCommand {
    pub name: String,
    pub description: String,
    pub usage: String,
    pub category: CommandCategory,
    pub aliases: Vec<String>,
    pub requires_auth: bool,
    pub admin_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandCategory {
    User,
    Channel,
    Audio,
    Chat,
    Admin,
    Utility,
    Fun,
}

#[derive(Debug, Clone)]
pub enum CommandResult {
    Success(String),
    Error(String),
    Silent,
}

pub struct CommandContext {
    pub sender_addr: SocketAddr,
    pub sender_mask: Option<String>,
    pub channel_id: u32,
    pub arguments: Vec<String>,
    pub is_admin: bool,
}

pub fn ask(prompt: &str) -> String {
    print!("{}", prompt);
    std::io::stdout().flush().unwrap();

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .expect("failed to readline");
    answer.trim().into()
}

pub fn is_whitespace_only(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '\u{200B}' | // ZERO WIDTH SPACE
            '\u{200C}' | // ZERO WIDTH NON-JOINER
            '\u{200D}' | // ZERO WIDTH JOINER
            '\u{FEFF}' // BYTE ORDER MARK
            )
    })
}

impl IntoPacket for CommandResult {
    fn serialize(&self) -> Vec<u8> {
        let mut packet = vec![ClientPacketType::Cmd as u8];
        match self {
            CommandResult::Success(content) => {
                packet.push(CommandResultPacketType::Success as u8);
                packet.extend_from_slice(content.as_bytes());
                packet
            }
            CommandResult::Error(content) => {
                packet.push(CommandResultPacketType::Error as u8);
                packet.extend_from_slice(content.as_bytes());
                packet
            }
            CommandResult::Silent => {
                packet.push(CommandResultPacketType::Silent as u8);
                packet
            }
        }
    }
}

pub fn parse_global_list(bytes: &[u8]) -> Option<(Vec<ChannelInfo>, u32)> {
    if bytes.len() < 4 {
        return None;
    }

    let current = u32::from_be_bytes(bytes[0..4].try_into().ok()?);
    let chan_count = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
    let mut channels = Vec::new();
    let mut i = 8;

    for _ in 0..chan_count {
        if i + 12 > bytes.len() {
            return None;
        }
        let chan_name_len = u8::from_be_bytes([bytes[i]]);
        i += 1;
        let name = String::from_utf8(bytes[i..i + 1 + chan_name_len as usize].to_vec()).ok()?;

        i += chan_name_len as usize;

        let channel_id = u32::from_be_bytes(bytes[i..i + 4].try_into().ok()?);
        let unmasked_count = u32::from_be_bytes(bytes[i + 4..i + 8].try_into().ok()?);
        let masked_count = u32::from_be_bytes(bytes[i + 8..i + 12].try_into().ok()?);

        i += 12;
        let mut masked_users = Vec::new();

        for _ in 0..masked_count {
            let sep_pos = bytes[i..].iter().position(|&b| b == 0x01)?;
            let mask_str = String::from_utf8(bytes[i..i + sep_pos].to_vec()).ok()?;
            i += sep_pos + 1;

            if i >= bytes.len() {
                return None;
            }

            let flags = bytes[i];
            i += 1;

            let muted = flags & 0b00000001 != 0;
            let deafened = flags & 0b00000010 != 0;

            masked_users.push((mask_str, muted, deafened));
        }

        channels.push(ChannelInfo {
            name,
            channel_id,
            unmasked_count,
            masked_users,
        });
    }

    channels.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));

    Some((channels, current))
}

pub fn parse_command_list(bytes: &[u8]) -> Option<Vec<ServerCommand>> {
    if bytes.len() < 2 {
        return None;
    }

    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut commands = Vec::new();
    let mut i = 2;

    for _ in 0..count {
        if i >= bytes.len() {
            return None;
        }

        // Parse name
        let name_len = bytes[i] as usize;
        i += 1;
        if i + name_len > bytes.len() {
            return None;
        }
        let name = String::from_utf8(bytes[i..i + name_len].to_vec()).ok()?;
        i += name_len;

        // Parse description
        let desc_len = bytes[i] as usize;
        i += 1;
        if i + desc_len > bytes.len() {
            return None;
        }
        let description = String::from_utf8(bytes[i..i + desc_len].to_vec()).ok()?;
        i += desc_len;

        // Parse usage
        let usage_len = bytes[i] as usize;
        i += 1;
        if i + usage_len > bytes.len() {
            return None;
        }
        let usage = String::from_utf8(bytes[i..i + usage_len].to_vec()).ok()?;
        i += usage_len;

        // Parse category
        let category_byte = bytes[i];
        i += 1;
        let category = match category_byte {
            0 => CommandCategory::User,
            1 => CommandCategory::Channel,
            2 => CommandCategory::Audio,
            3 => CommandCategory::Chat,
            4 => CommandCategory::Admin,
            5 => CommandCategory::Utility,
            6 => CommandCategory::Fun,
            _ => return None,
        };

        // Parse flags
        let flags = bytes[i];
        i += 1;
        let requires_auth = flags & 0b00000001 != 0;
        let admin_only = flags & 0b00000010 != 0;

        // Parse aliases
        let alias_count = bytes[i] as usize;
        i += 1;
        let mut aliases = Vec::new();

        for _ in 0..alias_count {
            if i >= bytes.len() {
                return None;
            }

            let alias_len = bytes[i] as usize;
            i += 1;
            if i + alias_len > bytes.len() {
                return None;
            }

            let alias = String::from_utf8(bytes[i..i + alias_len].to_vec()).ok()?;
            i += alias_len;
            aliases.push(alias);
        }

        commands.push(ServerCommand {
            name,
            description,
            usage,
            category,
            aliases,
            requires_auth,
            admin_only,
        });
    }

    Some(commands)
}

pub fn parse_command_response(data: &[u8]) -> Result<CommandResult, String> {
    if data.is_empty() {
        return Err("Command response too short!".into());
    }

    let mode = match CommandResultPacketType::try_from(data[0]) {
        Ok(mode) => {
            if mode.eq(&CommandResultPacketType::Silent) {
                return Ok(CommandResult::Silent);
            }
            mode
        }
        Err(got) => return Err(format!("Invalid command result type: {got}")),
    };

    let Ok(content) = String::from_utf8(data[1..].to_vec()) else {
        return Err("Invalid content from a non silent command response!".into());
    };

    match mode {
        CommandResultPacketType::Success => Ok(CommandResult::Success(content)),
        CommandResultPacketType::Error => Ok(CommandResult::Error(content)),
        CommandResultPacketType::Silent => {
            unreachable!("justification: voudp/src/util.rs:239:24")
        }
    }
}

pub fn parse_msg_packet(data: &[u8]) -> Result<(String, String, bool), String> {
    if data.is_empty() {
        return Err("empty packet".into());
    }
    if data.len() < 3 {
        // at least [type, username, delim, flag, ...?]
        return Err("packet too short".into());
    }

    match ClientPacketType::try_from(data[0]) {
        Ok(ClientPacketType::Chat) => {
            // Find the delimiter (first 0x01 after the packet type)
            let delimiter_pos = match data[1..].iter().position(|&b| b == 0x01) {
                Some(pos) => 1 + pos, // absolute index
                None => return Err("no 0x01 delimiter found".into()),
            };

            if delimiter_pos == 1 {
                return Err("username is empty".into());
            }

            // Username = bytes between index 1 and delimiter_pos
            let username_bytes = &data[1..delimiter_pos];
            let username = String::from_utf8(username_bytes.to_vec())
                .map_err(|_| "invalid UTF-8 in username")?;

            // After delimiter: flag byte, then message
            if data.len() <= delimiter_pos + 1 {
                return Err("missing is_self flag".into());
            }
            let is_self = data[delimiter_pos + 1] != 0;

            let message_bytes = &data[delimiter_pos + 2..];
            let message = String::from_utf8(message_bytes.to_vec())
                .map_err(|_| "invalid UTF-8 in message")?;

            Ok((username, message, is_self))
        }
        _ => Err("not a chat packet".into()),
    }
}

pub fn parse_flow_packet(data: &[u8]) -> Option<Message> {
    if data.is_empty() {
        return None;
    }

    match ClientPacketType::try_from(data[0]) {
        Ok(ClientPacketType::FlowJoin) => {
            let uname = &data[1..];
            let Ok(uname) = String::from_utf8(uname.to_vec()) else {
                return None;
            };
            Some(Message::JoinMessage(uname))
        }
        Ok(ClientPacketType::FlowLeave) => {
            let uname = &data[1..];
            let Ok(uname) = String::from_utf8(uname.to_vec()) else {
                return None;
            };
            Some(Message::LeaveMessage(uname))
        }
        Ok(ClientPacketType::FlowRenick) => {
            let mut i = 1;
            let old_mask_len = u8::from_be_bytes([data[i]]);
            i += 1;
            let old_mask = String::from_utf8(data[i..i + old_mask_len as usize].to_vec()).ok()?;
            i += old_mask_len as usize;

            let new_mask_len = u8::from_be_bytes([data[i]]);
            i += 1;
            let new_mask = String::from_utf8(data[i..i + new_mask_len as usize].to_vec()).ok()?;
            Some(Message::Renick(old_mask, new_mask))
        }
        Ok(ClientPacketType::Dm) => {
            let msg = &data[1..];
            let Ok(msg) = String::from_utf8(msg.to_vec()) else {
                return None;
            };
            Some(Message::Broadcast("Server".into(), msg))
        }
        _ => None,
    }
}

pub fn parse_control_packet(data: &[u8]) -> Result<ControlRequest, String> {
    if data.is_empty() {
        return Err("empty packet".into());
    }

    match data[0] {
        0x01 => Ok(ControlRequest::SetDeafen),
        0x02 => Ok(ControlRequest::SetUndeafen),
        0x03 => Ok(ControlRequest::SetMute),
        0x04 => Ok(ControlRequest::SetUnmute),
        // 0x05 => {
        //     if data.len() < 2 {
        //         Err("volume packet too short".into())
        //     } else {
        //         Ok(ControlRequest::SetVolume(data[1]))
        //     }
        // }
        _ => Err("Invalid control packet".into()),
    }
}
