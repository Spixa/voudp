use std::io;
use std::io::Write;
use std::net::SocketAddr;

use crate::protocol::{
    ClientPacketType, CommandResultPacketType, ControlRequest, FromPacket, IntoPacket, PacketError,
};

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

// Define your packet types
#[derive(Debug, Clone)]
pub struct GlobalListPacket {
    pub channels: Vec<ChannelInfo>,
    pub current: u32,
}

#[derive(Debug, Clone)]
pub struct CommandListPacket {
    pub commands: Vec<ServerCommand>,
}

#[derive(Debug, Clone)]
pub struct CommandResponsePacket {
    pub result: CommandResult,
}

#[derive(Debug, Clone)]
pub struct ChatPacket {
    pub username: String,
    pub message: String,
    pub is_self: bool,
}

#[derive(Debug, Clone)]
pub enum FlowPacket {
    Join(String),
    Leave(String),
    Renick { old_mask: String, new_mask: String },
    Broadcast { from: String, message: String },
}

#[derive(Debug, Clone)]
pub struct ControlPacket {
    pub request: ControlRequest,
}

impl FromPacket for GlobalListPacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < 8 {
            return Err(PacketError::TooShort(8, bytes.len()));
        }

        let current = u32::from_be_bytes(bytes[0..4].try_into()?);
        let chan_count = u32::from_be_bytes(bytes[4..8].try_into()?);
        let mut channels = Vec::new();
        let mut i = 8;

        for _ in 0..chan_count {
            // Ensure we have at least the channel name length byte
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }

            let chan_name_len = bytes[i] as usize;
            i += 1;

            // Check if we have enough bytes for the channel name
            if i + chan_name_len > bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }

            let name = String::from_utf8(bytes[i..i + chan_name_len].to_vec())?;
            i += chan_name_len;

            // Check if we have enough bytes for channel metadata
            if i + 12 > bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }

            let channel_id = u32::from_be_bytes(bytes[i..i + 4].try_into()?);
            let unmasked_count = u32::from_be_bytes(bytes[i + 4..i + 8].try_into()?);
            let masked_count = u32::from_be_bytes(bytes[i + 8..i + 12].try_into()?);
            i += 12;

            let mut masked_users = Vec::new();

            for _ in 0..masked_count {
                // Find the delimiter (0x01)
                let sep_pos = bytes[i..]
                    .iter()
                    .position(|&b| b == 0x01)
                    .ok_or(PacketError::MissingDelimiter)?;

                if i + sep_pos > bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }

                let mask_str = String::from_utf8(bytes[i..i + sep_pos].to_vec())?;
                i += sep_pos + 1; // +1 for the delimiter

                if i >= bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
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

        Ok(GlobalListPacket { channels, current })
    }
}

impl FromPacket for CommandListPacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < 2 {
            return Err(PacketError::TooShort(2, bytes.len()));
        }

        let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        let mut commands = Vec::new();
        let mut i = 2;

        for _ in 0..count {
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }

            // Parse name
            let name_len = bytes[i] as usize;
            i += 1;
            if i + name_len > bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let name = String::from_utf8(bytes[i..i + name_len].to_vec())?;
            i += name_len;

            // Parse description
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let desc_len = bytes[i] as usize;
            i += 1;
            if i + desc_len > bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let description = String::from_utf8(bytes[i..i + desc_len].to_vec())?;
            i += desc_len;

            // Parse usage
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let usage_len = bytes[i] as usize;
            i += 1;
            if i + usage_len > bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let usage = String::from_utf8(bytes[i..i + usage_len].to_vec())?;
            i += usage_len;

            // Parse category
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
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
                _ => return Err(PacketError::InvalidCommandCategory(category_byte)),
            };

            // Parse flags
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let flags = bytes[i];
            i += 1;
            let requires_auth = flags & 0b00000001 != 0;
            let admin_only = flags & 0b00000010 != 0;

            // Parse aliases
            if i >= bytes.len() {
                return Err(PacketError::BufferUnderflow(i));
            }
            let alias_count = bytes[i] as usize;
            i += 1;
            let mut aliases = Vec::new();

            for _ in 0..alias_count {
                if i >= bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }

                let alias_len = bytes[i] as usize;
                i += 1;
                if i + alias_len > bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }

                let alias = String::from_utf8(bytes[i..i + alias_len].to_vec())?;
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

        Ok(CommandListPacket { commands })
    }
}

impl FromPacket for CommandResponsePacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.is_empty() {
            return Err(PacketError::TooShort(1, 0));
        }

        let mode = CommandResultPacketType::try_from(bytes[0])
            .map_err(|_| PacketError::InvalidType(bytes[0]))?;

        if mode == CommandResultPacketType::Silent {
            return Ok(CommandResponsePacket {
                result: CommandResult::Silent,
            });
        }

        if bytes.len() < 2 {
            return Err(PacketError::TooShort(2, bytes.len()));
        }

        let content = String::from_utf8(bytes[1..].to_vec())?;

        let result = match mode {
            CommandResultPacketType::Success => CommandResult::Success(content),
            CommandResultPacketType::Error => CommandResult::Error(content),
            CommandResultPacketType::Silent => unreachable!(),
        };

        Ok(CommandResponsePacket { result })
    }
}

impl FromPacket for ChatPacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.is_empty() {
            return Err(PacketError::TooShort(1, 0));
        }

        match ClientPacketType::try_from(bytes[0]) {
            Ok(ClientPacketType::Chat) => {
                if bytes.len() < 3 {
                    return Err(PacketError::TooShort(3, bytes.len()));
                }

                // Find the delimiter (first 0x01 after the packet type)
                let delimiter_pos = bytes[1..]
                    .iter()
                    .position(|&b| b == 0x01)
                    .ok_or(PacketError::MissingDelimiter)?
                    + 1;

                if delimiter_pos == 1 {
                    return Err(PacketError::InvalidData("username is empty".into()));
                }

                let username = String::from_utf8(bytes[1..delimiter_pos].to_vec())?;

                if bytes.len() <= delimiter_pos + 1 {
                    return Err(PacketError::InvalidData("missing is_self flag".into()));
                }

                let is_self = bytes[delimiter_pos + 1] != 0;
                let message = String::from_utf8(bytes[delimiter_pos + 2..].to_vec())?;

                Ok(ChatPacket {
                    username,
                    message,
                    is_self,
                })
            }
            _ => Err(PacketError::InvalidType(bytes[0])),
        }
    }
}

impl FromPacket for FlowPacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.is_empty() {
            return Err(PacketError::TooShort(1, 0));
        }

        match ClientPacketType::try_from(bytes[0])
            .map_err(|_| PacketError::InvalidType(bytes[0]))?
        {
            ClientPacketType::FlowJoin => {
                let uname = String::from_utf8(bytes[1..].to_vec())?;
                Ok(FlowPacket::Join(uname))
            }
            ClientPacketType::FlowLeave => {
                let uname = String::from_utf8(bytes[1..].to_vec())?;
                Ok(FlowPacket::Leave(uname))
            }
            ClientPacketType::FlowRenick => {
                if bytes.len() < 2 {
                    return Err(PacketError::TooShort(2, bytes.len()));
                }

                let mut i = 1;
                let old_mask_len = bytes[i] as usize;
                i += 1;

                if i + old_mask_len > bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }
                let old_mask = String::from_utf8(bytes[i..i + old_mask_len].to_vec())?;
                i += old_mask_len;

                if i >= bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }
                let new_mask_len = bytes[i] as usize;
                i += 1;

                if i + new_mask_len > bytes.len() {
                    return Err(PacketError::BufferUnderflow(i));
                }
                let new_mask = String::from_utf8(bytes[i..i + new_mask_len].to_vec())?;

                Ok(FlowPacket::Renick { old_mask, new_mask })
            }
            ClientPacketType::Dm => {
                let msg = String::from_utf8(bytes[1..].to_vec())?;
                Ok(FlowPacket::Broadcast {
                    from: "Server".into(),
                    message: msg,
                })
            }
            _ => Err(PacketError::InvalidType(bytes[0])),
        }
    }
}

impl FromPacket for ControlPacket {
    fn deserialize(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.is_empty() {
            return Err(PacketError::TooShort(1, 0));
        }

        let request = match bytes[0] {
            0x01 => ControlRequest::SetDeafen,
            0x02 => ControlRequest::SetUndeafen,
            0x03 => ControlRequest::SetMute,
            0x04 => ControlRequest::SetUnmute,
            _ => return Err(PacketError::InvalidType(bytes[0])),
        };

        Ok(ControlPacket { request })
    }
}
