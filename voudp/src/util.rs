use std::io;
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;

pub const VOUDP_SALT: &[u8; 5] = b"voudp";

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

pub struct SecureUdpSocket {
    socket: UdpSocket,
    connected_addr: Option<SocketAddr>,
    cipher: ChaCha20Poly1305,
}

impl Clone for SecureUdpSocket {
    fn clone(&self) -> Self {
        let cloned_socket = self.socket.try_clone().expect("failed to clone UdpSocket");

        let cipher = self.cipher.clone();

        Self {
            socket: cloned_socket,
            connected_addr: self.connected_addr,
            cipher,
        }
    }
}

impl SecureUdpSocket {
    pub fn create(bind_addr: String, key: Key) -> io::Result<SecureUdpSocket> {
        let socket = UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;

        let cipher = ChaCha20Poly1305::new(&key);

        Ok(Self {
            socket,
            connected_addr: None,
            cipher,
        })
    }

    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> io::Result<()> {
        let addrs = addr.to_socket_addrs()?;
        if let Some(addr) = addrs.into_iter().find(|a| a.is_ipv4()) {
            self.connected_addr = Some(addr);
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no valid IPv4 address found",
            ))
        }
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        match self.connected_addr {
            Some(addr) => Ok(self.send_to(buf, addr)?),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    /// layout: [12-byte nonce || ciphertext+tag]
    pub fn send_to(&self, buf: &[u8], addr: SocketAddr) -> io::Result<usize> {
        // generate random nonce
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // encrypt
        let ciphertext = self
            .cipher
            .encrypt(nonce, buf)
            .map_err(|_| io::Error::other("encryption failure"))?;

        // how to build: nonce + ciphertext
        let mut packet = Vec::with_capacity(12 + ciphertext.len());
        packet.extend_from_slice(&nonce_bytes);
        packet.extend_from_slice(&ciphertext);

        self.socket.send_to(&packet, addr)
    }

    /// layout: [12-byte nonce + ciphertext + tag]
    pub fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<
        (usize, SocketAddr),
        (
            io::Error,
            SocketAddr, /* we need to forward the addr even when failing to let the remote know */
        ),
    > {
        let (size, addr) = match self.socket.recv_from(buf) {
            Ok(ok) => ok,
            Err(e) => return Err((e, SocketAddr::from(([0, 0, 0, 0], 0)))), // no addr yet
        };

        if size < 12 {
            if size == 1 && buf[0] == 0xf {
                return Err((
                    io::Error::new(io::ErrorKind::Unsupported, "unencrpyted"),
                    addr,
                ));
            } else {
                return Err((
                    io::Error::new(io::ErrorKind::InvalidData, "packet too small"),
                    addr,
                ));
            }
        }

        let (nonce_bytes, ciphertext) = buf[..size].split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = match self.cipher.decrypt(nonce, ciphertext) {
            Ok(pt) => pt,
            Err(_) => {
                return Err((io::Error::other("decryption failure"), addr));
            }
        };

        // overwrite buffer with plaintext
        buf[..plaintext.len()].copy_from_slice(&plaintext);

        Ok((plaintext.len(), addr))
    }

    pub fn send_bad_packet_notice(&self, addr: SocketAddr) -> io::Result<usize> {
        let notice = vec![0xf];
        self.socket.send_to(&notice, addr)
    }
}
