use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, OsRng, rand_core::RngCore},
};

use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::{AtomicU32, AtomicU64}},
    time::{Duration, Instant},
};
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::atomic::Ordering,
};

use crate::protocol::{ACK_FLAG, ClientPacketType, RELIABLE_FLAG};

pub fn derive_key_from_phrase(phrase: &[u8], salt: &[u8]) -> Key {
    let iters = 600_000u32;
    let mut key_b = [0u8; 32];
    pbkdf2_hmac::<Sha256>(phrase, salt, iters, &mut key_b);

    Key::from_slice(&key_b).to_owned()
}

struct PendingPacket {
    data: Vec<u8>,
    addr: SocketAddr,
    last_sent: Instant,
    retries: u8,
}

struct InnerSocket {
    socket: UdpSocket,
    cipher: ChaCha20Poly1305,
    seq_counter: AtomicU32,
    pending: Mutex<HashMap<u32, PendingPacket>>,
    nonce_counter: AtomicU64,
    nonce_prefix: [u8; 4],
    connected_addr: Mutex<Option<SocketAddr>>,
}

#[derive(Clone)]
pub struct SecureUdpSocket {
    inner: Arc<InnerSocket>,
}

impl SecureUdpSocket {
    pub fn create(bind_addr: String, key: Key) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;
        let cipher = ChaCha20Poly1305::new(&key);

        let mut nonce_prefix = [0u8; 4];
        OsRng.fill_bytes(&mut nonce_prefix);
        
        Ok(Self {
            inner: Arc::new(InnerSocket {
                socket,
                cipher,
                seq_counter: AtomicU32::new(1),
                pending: Mutex::new(HashMap::new()),
                nonce_counter: AtomicU64::new(0),
                nonce_prefix,
                connected_addr: Mutex::new(None),
            }),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.socket.local_addr().unwrap()
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let addrs = addr.to_socket_addrs()?;
        if let Some(addr) = addrs.into_iter().find(|a| a.is_ipv4()) {
            *self.inner.connected_addr.lock().unwrap() = Some(addr);
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no valid IPv4 address found",
            ))
        }
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let addr =
            self.inner.connected_addr.lock().unwrap().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "socket not connected")
            })?;

        if buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty packet"));
        }

        let packet_type = ClientPacketType::try_from(buf[0]).unwrap_or(ClientPacketType::Audio);

        if packet_type.is_reliable() {
            self.send_reliable(buf.to_vec(), addr)?;
            Ok(buf.len())
        } else {
            self.send_to(buf, addr)
        }
    }

    pub fn send_to(&self, buf: &[u8], addr: SocketAddr) -> io::Result<usize> {

        let counter = self.inner.nonce_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..4].copy_from_slice(&self.inner.nonce_prefix);
        nonce_bytes[4..].copy_from_slice(&counter.to_be_bytes()); // 8-byte counter
        let nonce = Nonce::from_slice(&nonce_bytes);



        let ciphertext = self
            .inner
            .cipher
            .encrypt(nonce, buf)
            .map_err(|_| io::Error::other("encryption failure"))?;

        let mut packet = Vec::with_capacity(12 + ciphertext.len());
        packet.extend_from_slice(&nonce_bytes);
        packet.extend_from_slice(&ciphertext);

        self.inner.socket.send_to(&packet, addr)
    }

    fn send_reliable(&self, payload: Vec<u8>, addr: SocketAddr) -> io::Result<()> {
        let seq = self.inner.seq_counter.fetch_add(1, Ordering::Relaxed);
        let mut packet = Vec::with_capacity(1 + 4 + payload.len());
        packet.push(RELIABLE_FLAG);
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&payload);

        self.send_to(&packet, addr)?;

        self.inner.pending.lock().unwrap().insert(
            seq,
            PendingPacket {
                data: packet,
                addr,
                last_sent: Instant::now(),
                retries: 0,
            },
        );

        Ok(())
    }

    pub fn send_ack(&self, seq: u32, addr: SocketAddr) -> io::Result<usize> {
        let mut ack_plain = [0u8; 5];
        ack_plain[0] = ACK_FLAG;
        ack_plain[1..5].copy_from_slice(&seq.to_be_bytes());

        self.send_to(&ack_plain, addr)
    }

    pub fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, SocketAddr), (io::Error, SocketAddr)> {
        let (size, addr) = match self.inner.socket.recv_from(buf) {
            Ok(ok) => ok,
            Err(e) => return Err((e, SocketAddr::from(([0, 0, 0, 0], 0)))),
        };

        if size < 12 {
            return Err((
                io::Error::new(io::ErrorKind::InvalidData, "packet too small"),
                addr,
            ));
        }

        let (nonce_bytes, ciphertext) = buf[..size].split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = match self.inner.cipher.decrypt(nonce, ciphertext) {
            Ok(pt) => pt,
            Err(_) => {
                return Err((
                    io::Error::new(io::ErrorKind::InvalidData, "decryption failure"),
                    addr,
                ));
            }
        };

        // ACK handling
        if plaintext.len() == 5 && plaintext[0] == ACK_FLAG {
            let seq = u32::from_be_bytes(plaintext[1..5].try_into().unwrap());
            self.inner.pending.lock().unwrap().remove(&seq);
            return Ok((0, addr));
        }

        // Reliable packet handling
        if plaintext.len() >= 6 && plaintext[0] == RELIABLE_FLAG {
            let seq = u32::from_be_bytes(plaintext[1..5].try_into().unwrap());
            let _ = self.send_ack(seq, addr);

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

        if plaintext.len() > buf.len() {
            return Err((
                io::Error::new(io::ErrorKind::InvalidData, "plaintext too large"),
                addr,
            ));
        }

        buf[..plaintext.len()].copy_from_slice(&plaintext);
        Ok((plaintext.len(), addr))
    }

    pub fn tick_reliable(&self) {
        let mut pending = self.inner.pending.lock().unwrap();
        let now = Instant::now();
        let timeout = Duration::from_millis(200);
        let max_retries = 5;

        pending.retain(|_, pkt| {
            if pkt.retries >= max_retries {
                return false; // give up
            }

            if now.duration_since(pkt.last_sent) >= timeout {
                let _ = self.inner.socket.send_to(&pkt.data, pkt.addr);
                pkt.last_sent = now;
                pkt.retries += 1;
            }

            true
        });
    }
}
