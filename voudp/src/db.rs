use std::io;

pub fn user_exists(username: &str) -> bool {
    std::path::Path::new(&format!("db/{}.hash", username)).exists()
        && std::path::Path::new(&format!("db/{}.salt", username)).exists()
}

pub fn create_user(username: &str, salt: &[u8; 16], hash: &[u8; 32]) -> io::Result<()> {
    std::fs::create_dir_all("db")?;
    std::fs::write(format!("db/{}.salt", username), salt)?;
    std::fs::write(format!("db/{}.hash", username), hash)?;
    Ok(())
}

pub fn get_user_salt(username: &str) -> Option<[u8; 16]> {
    let path = format!("db/{}.salt", username);
    let data = std::fs::read(&path).ok()?;
    if data.len() != 16 {
        return None;
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&data);
    Some(salt)
}

pub fn lookup_password_hash(username: &str) -> Option<[u8; 32]> {
    let path = format!("db/{}.hash", username);
    let data = std::fs::read(&path).ok()?;
    if data.len() != 32 {
        return None;
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&data);
    Some(hash)
}
