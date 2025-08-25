use std::io;
use std::io::Write;

use chacha20poly1305::Key;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;

pub fn ask(prompt: &str) -> String {
    print!("{}", prompt);
    std::io::stdout().flush().unwrap();

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .expect("failed to readline");
    answer.trim().into()
}

pub fn parse_list_packet(bytes: &[u8]) -> Option<(u32, u32, Vec<(String, bool, bool)>)> {
    if bytes.len() < 9 || bytes[0] != 0x05 {
        return None;
    }

    let unmasked_count = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
    let masked_count = u32::from_be_bytes(bytes[5..9].try_into().ok()?);

    let mut masks = Vec::new();
    let mut i = 9;

    for _ in 0..masked_count {
        // Find string terminator
        let sep_pos = bytes[i..].iter().position(|&b| b == 0x01)?;
        let mask_str = String::from_utf8(bytes[i..i + sep_pos].to_vec()).ok()?;
        i += sep_pos + 1; // move past mask + separator

        // Read flags
        if i >= bytes.len() {
            return None;
        }
        let flags = bytes[i];
        i += 1;

        let muted = flags & 0b00000001 != 0;
        let deafened = flags & 0b00000010 != 0;

        masks.push((mask_str, muted, deafened));
    }

    Some((unmasked_count, masked_count, masks))
}

pub fn parse_msg_packet(data: &[u8]) -> Result<(String, String), String> {
    // Must start with 0x06
    if data.first() != Some(&0x06) {
        return Err("packet does not start with 0x06".into());
    }

    // Find the 0x01 separator after the username
    let sep_index = match data.iter().position(|&b| b == 0x01) {
        Some(i) => i,
        None => return Err("no 0x01 separator found".into()),
    };

    if sep_index <= 1 {
        return Err("username is empty".into());
    }

    // Username slice (skip 0x06, end right before 0x01)
    let username_bytes = &data[1..sep_index];
    // Message slice (everything after 0x01)
    let message_bytes = &data[sep_index + 1..];

    // Decode UTF-8
    let username =
        String::from_utf8(username_bytes.to_vec()).map_err(|_| "invalid UTF-8 in username")?;
    let message =
        String::from_utf8(message_bytes.to_vec()).map_err(|_| "invalid UTF-8 in message")?;

    Ok((username, message))
}

pub enum ControlRequest {
    SetDeafen,
    SetUndeafen,
    SetMute,
    SetUnmute,
    SetVolume(u8),
}

pub fn parse_control_packet(data: &[u8]) -> Result<ControlRequest, String> {
    let req = match data[0] {
        0x01 => ControlRequest::SetDeafen,
        0x02 => ControlRequest::SetUndeafen,
        0x03 => ControlRequest::SetMute,
        0x04 => ControlRequest::SetUnmute,
        0x05 => ControlRequest::SetVolume(data[1]),
        _ => return Err("Invalid control packet".into()),
    };

    Ok(req)
}

pub fn derive_key_from_phrase(phrase: &[u8], salt: &[u8]) -> Key {
    let iters = 600_000u32;
    let mut key_b = [0u8; 32];
    pbkdf2_hmac::<Sha256>(phrase, salt, iters, &mut key_b);

    Key::from_slice(&key_b).to_owned()
}
