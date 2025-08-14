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
