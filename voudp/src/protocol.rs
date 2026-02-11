/*
    Protocol definiton for VoUDP v0.1
*/
use std::convert::TryFrom;

pub const VOUDP_SALT: &[u8; 5] = b"voudp";
pub const PASSWORD: &str = "password";

// internal flags for packet processing:
pub const RELIABLE_FLAG: u8 = 0x80;
pub const ACK_FLAG: u8 = 0x81;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientPacketType {
    Join = 0x01,
    Audio = 0x02,
    Eof = 0x03,
    Mask = 0x04,
    List = 0x05,
    Chat = 0x06,
    // 0x07 is reserved
    Ctrl = 0x08,
    // 0x09 is reserved
    FlowJoin = 0x0a,
    FlowLeave = 0x0b,
    SyncCommands = 0x0c,
    Cmd = 0x0d,
    CommandResponse = 0x0e,
    // 0x0f is reserved
    FlowRenick = 0x10,
    Dm = 0x11,
    // 0x12-0xfe are reserved
    RegisterConsole = 0xff,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolePacketType {
    Cmd = 0x0d,
    Eof = 0x03,
    Keepalive = 0x04,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRequest {
    SetDeafen = 0x01,
    SetUndeafen = 0x02,
    SetMute = 0x03,
    SetUnmute = 0x04,
    // SetVolume takes a parameter, so it's handled separately
}

impl TryFrom<u8> for ClientPacketType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Join),
            0x02 => Ok(Self::Audio),
            0x03 => Ok(Self::Eof),
            0x04 => Ok(Self::Mask),
            0x05 => Ok(Self::List),
            0x06 => Ok(Self::Chat),
            0x08 => Ok(Self::Ctrl),
            0x0a => Ok(Self::FlowJoin),
            0x0b => Ok(Self::FlowLeave),
            0x0c => Ok(Self::SyncCommands),
            0x0d => Ok(Self::Cmd),
            0x10 => Ok(Self::FlowRenick),
            0x11 => Ok(Self::Dm),
            0xff => Ok(Self::RegisterConsole),
            _ => Err(value),
        }
    }
}

impl TryFrom<u8> for ConsolePacketType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x03 => Ok(Self::Eof),
            0x04 => Ok(Self::Keepalive),
            0x0d => Ok(Self::Cmd),
            _ => Err(value),
        }
    }
}

impl TryFrom<u8> for ControlRequest {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::SetDeafen),
            0x02 => Ok(Self::SetUndeafen),
            0x03 => Ok(Self::SetMute),
            0x04 => Ok(Self::SetUnmute),
            _ => Err(value),
        }
    }
}

pub trait PacketSerializer {
    fn to_bytes(&self) -> Vec<u8>;
}

impl PacketSerializer for ClientPacketType {
    fn to_bytes(&self) -> Vec<u8> {
        vec![*self as u8]
    }
}

pub fn create_audio_packet(opus_data: &[u8]) -> Vec<u8> {
    let mut packet = vec![ClientPacketType::Audio as u8];
    packet.extend_from_slice(opus_data);
    packet
}

pub fn create_list_request() -> Vec<u8> {
    ClientPacketType::List.to_bytes()
}

pub fn create_sync_commands_request() -> Vec<u8> {
    ClientPacketType::SyncCommands.to_bytes()
}

pub fn is_flow_packet(packet_type: ClientPacketType) -> bool {
    matches!(
        packet_type,
        ClientPacketType::FlowJoin
            | ClientPacketType::FlowLeave
            | ClientPacketType::FlowRenick
            | ClientPacketType::Dm
    )
}

pub fn is_client_to_server_only(packet_type: ClientPacketType) -> bool {
    matches!(
        packet_type,
        ClientPacketType::Join
            | ClientPacketType::Mask
            | ClientPacketType::Ctrl
            | ClientPacketType::RegisterConsole
    )
}

// useful for legacy clients for audio initialization after this update
pub fn is_client_ready() -> bool {
    todo!()
}
