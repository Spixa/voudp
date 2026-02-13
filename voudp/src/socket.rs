use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, OsRng, rand_core::RngCore},
};
use log::warn;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use std::sync::atomic::AtomicU32;
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::atomic::Ordering,
};

use crate::protocol::{ACK_FLAG, RELIABLE_FLAG};

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
    seq_counter: AtomicU32,
}

impl Clone for SecureUdpSocket {
    fn clone(&self) -> Self {
        let cloned_socket = self.socket.try_clone().expect("failed to clone UdpSocket");
        let cipher = self.cipher.clone();

        Self {
            socket: cloned_socket,
            connected_addr: self.connected_addr,
            cipher,
            seq_counter: AtomicU32::new(self.seq_counter.load(Ordering::Relaxed)),
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
            seq_counter: AtomicU32::new(1),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
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

    pub fn send_reliable_to(&self, buf: &[u8], addr: SocketAddr) -> Result<u32, io::Error> {
        let seq = self.seq_counter.fetch_add(1, Ordering::Relaxed);

        let mut wrapped = Vec::with_capacity(1 + 4 + buf.len());
        wrapped.push(RELIABLE_FLAG);
        wrapped.extend_from_slice(&seq.to_be_bytes());
        wrapped.extend_from_slice(buf);

        self.send_to(&wrapped, addr)?;

        Ok(seq)
    }

    pub fn send_ack(&self, seq: u32, addr: SocketAddr) -> io::Result<usize> {
        let mut ack_plain = [0u8; 5];
        ack_plain[0] = ACK_FLAG;
        ack_plain[1..5].copy_from_slice(&seq.to_be_bytes());

        self.send_to(&ack_plain, addr)
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
            Err(e) => return Err((e, SocketAddr::from(([0, 0, 0, 0], 0)))),
        };

        if size < 12 {
            if size == 1 && buf[0] == 0xf {
                return Err((
                    io::Error::new(io::ErrorKind::Unsupported, "unencrypted"),
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

        // check if it has flags:

        // 1. ack
        if plaintext.len() >= 5 && plaintext[0] == ACK_FLAG {
            // let seq = u32::from_be_bytes([plaintext[1], plaintext[2], plaintext[3], plaintext[4]]);
            return Ok((0, addr));
        }

        // 2. reliable send
        if plaintext.len() >= 5 && plaintext[0] == RELIABLE_FLAG {
            let seq = u32::from_be_bytes([plaintext[1], plaintext[2], plaintext[3], plaintext[4]]);

            if let Err(e) = self.send_ack(seq, addr) {
                warn!("Failed to send ack {} to {}: {}", seq, addr, e);
            }

            let inner = &plaintext[5..];
            if inner.len() > buf.len() {
                return Err((
                    io::Error::new(io::ErrorKind::InvalidData, "inner too large"),
                    addr,
                ));
            }
            buf[..inner.len()].copy_from_slice(inner);
            return Ok((inner.len(), addr));
        }

        // 3. regular mode: overwrite buffer with plaintext
        if plaintext.len() > buf.len() {
            return Err((
                io::Error::new(io::ErrorKind::InvalidData, "plaintext too large"),
                addr,
            ));
        }
        buf[..plaintext.len()].copy_from_slice(&plaintext);

        Ok((plaintext.len(), addr))
    }

    pub fn send_bad_packet_notice(&self, addr: SocketAddr) -> io::Result<usize> {
        let notice = vec![0xf];
        self.socket.send_to(&notice, addr)
    }
}
