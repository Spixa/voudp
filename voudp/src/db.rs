use std::io;
use std::path::PathBuf;
pub enum LookupResult {
    Correct([u8; 32]),
    MalformedHex,
    OsError,
}

pub fn lookup_password(username: &String) -> LookupResult {
    let clean = username.trim_end_matches('\0');

    let path = PathBuf::from("db").join(format!("{}.sha", clean));
    let data = std::fs::read_to_string(&path); // read as String

    let data = match data {
        Ok(data) => data,
        Err(e) => {
            return LookupResult::OsError;
        }
    };

    let hex_str = data.trim();

    let decoded = hex::decode(hex_str).map_err(|_| io::Error::other("invalid hex in SHA file"));

    if decoded.is_err() {
        return LookupResult::MalformedHex;
    }

    let decoded = decoded.unwrap();

    if decoded.len() != 32 {
        return LookupResult::MalformedHex;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);

    LookupResult::Correct(out)
}
