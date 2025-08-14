use std::io;
use std::io::Write;

pub fn ask(prompt: &str) -> String {
    print!("{}", prompt);
    std::io::stdout().flush().unwrap();

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .expect("failed to readline");
    answer.trim().into()
}

pub fn parse_list_packet(bytes: &[u8]) -> Option<(u32, u32, Vec<String>)> {
    if bytes.len() < 9 || bytes[0] != 0x05 {
        return None; // invalid packet
    }

    let unmasked_count = u32::from_be_bytes(bytes[1..5].try_into().unwrap());
    let masked_count = u32::from_be_bytes(bytes[5..9].try_into().unwrap());

    let mut masks = Vec::new();
    let mask_data = &bytes[9..];

    for part in mask_data.split(|&b| b == 0x01) {
        if !part.is_empty() {
            if let Ok(s) = String::from_utf8(part.to_vec()) {
                masks.push(s);
            }
        }
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
