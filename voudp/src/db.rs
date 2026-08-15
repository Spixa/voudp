use crate::util::sha256;

pub fn lookup_password(_username: &String) -> [u8; 32] {
    sha256(b"password")
}
