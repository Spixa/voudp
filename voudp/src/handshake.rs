use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use chacha20poly1305::Key;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use x25519_dalek::{EphemeralSecret, PublicKey};

use crate::db;
use crate::protocol::ClientPacketType;
use crate::socket::{SecureUdpSocket, derive_session_key};
use crate::util::sha256;

pub fn register_user(username: &str, password: &[u8]) -> io::Result<()> {
    let salt = rand::random::<[u8; 16]>();
    let hash = slow_hash(password, &salt);

    db::create_user(&username, &salt, &hash)
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HandshakeStep {
    // login
    ClientHello = 0x01,
    ServerHello = 0x02,
    ClientCredentials = 0x05,
    ServerConfirm = 0x07,

    // register
    RegisterRequest = 0x03,
    RegisterResponse = 0x04,

    // HandshakeFail = 0xfe,
    RegisterFail = 0xff,
    RegistrationClosed = 0xfd,
    Done = 0x00,
}

impl TryFrom<u8> for HandshakeStep {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::ClientHello),
            0x02 => Ok(Self::ServerHello),
            0x05 => Ok(Self::ClientCredentials),
            0x07 => Ok(Self::ServerConfirm),
            0x03 => Ok(Self::RegisterRequest),
            0x04 => Ok(Self::RegisterResponse),
            0xff => Ok(Self::RegisterFail),
            0xfd => Ok(Self::RegistrationClosed),
            0x00 => Ok(Self::Done),
            _ => Err(value),
        }
    }
}

#[derive(Debug)]
pub enum HandshakeMessage {
    ClientHello {
        session_id: [u8; 16],
        c_nonce: [u8; 32],
        username: [u8; 32],
    },
    ServerHello {
        session_id: [u8; 16],
        s_nonce: [u8; 32],
        e_pub_s: [u8; 32],
        salt: [u8; 16],
        server_pub_bytes: [u8; 32],
        signature: [u8; 64],
    },
    ClientCreds {
        session_id: [u8; 16],
        e_pub_c: [u8; 32],
        challenge_response: [u8; 32],
    },
    ServerConfirm {
        session_id: [u8; 16],
        confirmation: [u8; 32],
    },
}

#[derive(Debug)]
pub enum RegisterMessage {
    Request {
        session_id: [u8; 16],
        c_nonce: [u8; 32],
        username: [u8; 32],
        password: Vec<u8>,
    },
    Response {
        session_id: [u8; 16],
        s_nonce: [u8; 32],
        salt: [u8; 16],
        server_pub_bytes: [u8; 32],
        confirmation: [u8; 32],
        signature: [u8; 64],
    },
    AlreadyExists,
    RegistrationClosed,
}

pub const STEP_CLIENT_HELLO: u8 = 1;
pub const STEP_SERVER_HELLO: u8 = 2;
pub const STEP_CLIENT_CREDS: u8 = 5;
pub const STEP_SERVER_CONFIRM: u8 = 7;

pub const STEP_REGISTER_REQUEST: u8 = 3;
pub const STEP_REGISTER_RESPONSE: u8 = 4;

pub const REGISTER_FAILED: u8 = 0xff;
pub const REGISTER_CLOSED: u8 = 0xfd;

pub const CLIENT_HELLO_SIZE: usize = 1 + 16 + 32 + 32; // step + session_id + c_nonce + username
pub const SERVER_HELLO_SIZE: usize = 1 + 16 + 32 + 32 + 16 + 32 + 64; // step + sid + s_nonce + e_pub_s + salt + pub + sig
pub const CLIENT_CREDS_SIZE: usize = 1 + 16 + 32 + 32; // step + sid + e_pub_c + challenge_response
pub const SERVER_CONFIRM_SIZE: usize = 1 + 16 + 32; // step + sid + confirmation
pub const REGISTER_REQUEST_MIN_SIZE: usize = 1 + 16 + 32 + 32; // step + sid + c_nonce + username + ... [password]
pub const REGISTER_RESPONSE_SIZE: usize = 1 + 16 + 32 + 16 + 32 + 32 + 64; // step + sid + s_nonce + salt + server_pub + confirmation + signature 

impl RegisterMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            RegisterMessage::Request {
                session_id,
                c_nonce,
                username,
                password,
            } => {
                let mut buf = Vec::with_capacity(REGISTER_REQUEST_MIN_SIZE + password.len());
                buf.push(STEP_REGISTER_REQUEST);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(c_nonce);
                buf.extend_from_slice(username);
                buf.extend_from_slice(password);
                buf
            }
            RegisterMessage::Response {
                session_id,
                s_nonce,
                salt,
                server_pub_bytes,
                confirmation,
                signature,
            } => {
                let mut buf = Vec::with_capacity(REGISTER_RESPONSE_SIZE);
                buf.push(STEP_REGISTER_RESPONSE);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(s_nonce);
                buf.extend_from_slice(salt);
                buf.extend_from_slice(server_pub_bytes);
                buf.extend_from_slice(confirmation);
                buf.extend_from_slice(signature);
                buf
            }
            RegisterMessage::AlreadyExists => vec![REGISTER_FAILED],
            RegisterMessage::RegistrationClosed => vec![REGISTER_CLOSED],
        }
    }

    pub fn decode(data: &[u8]) -> Result<Self, &'static str> {
        if data.is_empty() {
            return Err("empty packet");
        }

        let step = data[0];
        let payload = &data[1..];

        match step {
            STEP_REGISTER_REQUEST => {
                if payload.len() < 16 + 32 + 32 {
                    return Err("invalid RegisterRequest size");
                }

                let (sid, rest) = payload.split_at(16);
                let (c_nonce, rest) = rest.split_at(32);
                let (username, password) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let c_nonce: [u8; 32] = c_nonce.try_into().unwrap();
                let username: [u8; 32] = username.try_into().unwrap();

                let password = password.to_vec();

                Ok(RegisterMessage::Request {
                    session_id,
                    c_nonce,
                    username,
                    password,
                })
            }
            STEP_REGISTER_RESPONSE => {
                if payload.len() != 16 + 32 + 16 + 32 + 32 + 64 {
                    return Err("invalid RegisterResponse size");
                }

                let (sid, rest) = payload.split_at(16);
                let (s_nonce, rest) = rest.split_at(32);
                let (salt, rest) = rest.split_at(16);
                let (server_pub_bytes, rest) = rest.split_at(32);
                let (confirmation, signature) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let s_nonce: [u8; 32] = s_nonce.try_into().unwrap();
                let salt: [u8; 16] = salt.try_into().unwrap();
                let server_pub_bytes = server_pub_bytes.try_into().unwrap();
                let confirmation: [u8; 32] = confirmation.try_into().unwrap();
                let signature: [u8; 64] = signature.try_into().unwrap();

                Ok(RegisterMessage::Response {
                    session_id,
                    s_nonce,
                    salt,
                    server_pub_bytes,
                    confirmation,
                    signature,
                })
            }
            REGISTER_FAILED => Ok(RegisterMessage::AlreadyExists),
            REGISTER_CLOSED => Ok(RegisterMessage::RegistrationClosed),
            _ => Err("Not a register packet"),
        }
    }
}

impl HandshakeMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            HandshakeMessage::ClientHello {
                session_id,
                c_nonce,
                username,
            } => {
                let mut buf = Vec::with_capacity(CLIENT_HELLO_SIZE);
                buf.push(STEP_CLIENT_HELLO);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(c_nonce);
                buf.extend_from_slice(username);
                buf
            }
            HandshakeMessage::ServerHello {
                session_id,
                s_nonce,
                server_pub_bytes,
                signature,
                salt,
                e_pub_s,
            } => {
                let mut buf = Vec::with_capacity(SERVER_HELLO_SIZE);
                buf.push(STEP_SERVER_HELLO);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(s_nonce);
                buf.extend_from_slice(e_pub_s);
                buf.extend_from_slice(salt);
                buf.extend_from_slice(server_pub_bytes);
                buf.extend_from_slice(signature);
                buf
            }
            HandshakeMessage::ClientCreds {
                session_id,
                challenge_response,
                e_pub_c,
            } => {
                let mut buf = Vec::with_capacity(CLIENT_CREDS_SIZE);
                buf.push(STEP_CLIENT_CREDS);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(e_pub_c);
                buf.extend_from_slice(challenge_response);
                buf
            }
            HandshakeMessage::ServerConfirm {
                session_id,
                confirmation,
            } => {
                let mut buf = Vec::with_capacity(SERVER_CONFIRM_SIZE);
                buf.push(STEP_SERVER_CONFIRM);
                buf.extend_from_slice(session_id);
                buf.extend_from_slice(confirmation);
                buf
            }
        }
    }

    pub fn decode(data: &[u8]) -> Result<Self, &'static str> {
        if data.is_empty() {
            return Err("empty packet");
        }

        let step = data[0];
        let payload = &data[1..];

        match step {
            STEP_CLIENT_HELLO => {
                if payload.len() != 16 + 32 + 32 {
                    return Err("invalid ClientHello size");
                }

                let (sid, rest) = payload.split_at(16);
                let (c_nonce, username) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let c_nonce: [u8; 32] = c_nonce.try_into().unwrap();
                let username: [u8; 32] = username.try_into().unwrap();

                Ok(HandshakeMessage::ClientHello {
                    session_id,
                    c_nonce,
                    username,
                })
            }
            STEP_SERVER_HELLO => {
                if payload.len() != 16 + 32 + 32 + 32 + 16 + 64 {
                    println!("got {} instead of {}", payload.len(), 16 + 32 + 32 + 64);
                    return Err("invalid ServerHello size");
                }

                let (sid, rest) = payload.split_at(16);
                let (s_nonce, rest) = rest.split_at(32);
                let (e_pub_s, rest) = rest.split_at(32);
                let (salt, rest) = rest.split_at(16);
                let (pub_bytes, sig) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let s_nonce: [u8; 32] = s_nonce.try_into().unwrap();
                let e_pub_s: [u8; 32] = e_pub_s.try_into().unwrap();
                let salt: [u8; 16] = salt.try_into().unwrap();
                let server_pub_bytes: [u8; 32] = pub_bytes.try_into().unwrap();
                let signature: [u8; 64] = sig.try_into().unwrap();

                Ok(HandshakeMessage::ServerHello {
                    session_id,
                    s_nonce,
                    e_pub_s,
                    salt,
                    server_pub_bytes,
                    signature,
                })
            }
            STEP_CLIENT_CREDS => {
                if payload.len() != 16 + 32 + 32 {
                    return Err("invalid ClientCreds size");
                }

                let (sid, rest) = payload.split_at(16);
                let (e_pub_c, rest) = rest.split_at(32);
                let (challenge_response, _) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let e_pub_c: [u8; 32] = e_pub_c.try_into().unwrap();
                let challenge_response: [u8; 32] = challenge_response.try_into().unwrap();

                Ok(HandshakeMessage::ClientCreds {
                    session_id,
                    e_pub_c,
                    challenge_response,
                })
            }
            STEP_SERVER_CONFIRM => {
                if payload.len() != 16 + 32 {
                    return Err("invalid ServerConfirm size");
                }

                let (sid, rest) = payload.split_at(16);
                let (confirmation, _) = rest.split_at(32);

                let session_id: [u8; 16] = sid.try_into().unwrap();
                let confirmation: [u8; 32] = confirmation.try_into().unwrap();

                Ok(HandshakeMessage::ServerConfirm {
                    session_id,
                    confirmation,
                })
            }
            _ => Err("unknown handshake step"),
        }
    }
}

pub struct RemoteSessionState {
    pub step: HandshakeStep,
    pub session_id: [u8; 16],
    pub c_nonce: [u8; 32],
    pub s_nonce: [u8; 32],
    pub username: String,
    pub eph_secret: Option<EphemeralSecret>,
    pub client_addr: SocketAddr,
    pub created: Instant,
}

impl RemoteSessionState {
    pub fn is_expired(&self) -> bool {
        self.created.elapsed() > Duration::from_secs(10)
    }
}

use argon2::{Algorithm, Argon2, ParamsBuilder, Version};

pub fn slow_hash(password: &[u8], salt: &[u8; 16]) -> [u8; 32] {
    let params = ParamsBuilder::new()
        .m_cost(4096) // 4 MiB
        .output_len(32)
        .build()
        .unwrap();

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut output = [0u8; 32];
    argon2
        .hash_password_into(password, salt, &mut output)
        .unwrap();
    output
}

pub struct ClientHandshake {
    pub step: HandshakeStep,
    pub session_id: [u8; 16],
    pub c_nonce: [u8; 32],
    pub s_nonce: [u8; 32],
    pub e_pub_c: [u8; 32],
    pub e_pub_s: [u8; 32],
    pub eph_secret: Option<EphemeralSecret>,
    pub server_pub_bytes: [u8; 32],
    pub server_addr: SocketAddr,
    pub username: String,
    pub password: String,
    pub key: Option<Key>,
    pub start_time: Instant,
}

impl ClientHandshake {
    pub fn new(server_addr: SocketAddr, username: String, password: String) -> Self {
        Self {
            step: HandshakeStep::ClientHello, // will be set after start
            session_id: [0u8; 16],
            c_nonce: [0u8; 32],
            s_nonce: [0u8; 32],
            e_pub_c: [0u8; 32],
            e_pub_s: [0u8; 32],
            server_pub_bytes: [0u8; 32],
            eph_secret: None,
            key: None,
            server_addr,
            username,
            password,
            start_time: Instant::now(),
        }
    }

    pub fn register_handshake(
        &mut self,
        socket: &SecureUdpSocket,
        username: &str,
        password: &str,
        trusted_pin: &[u8; 32],
    ) -> io::Result<()> {
        let session_id_local = rand::random();
        let c_nonce_local = rand::random();

        let mut uname_arr = [0u8; 32];
        let bytes = username.as_bytes();
        let len = bytes.len().min(32);
        uname_arr[..len].copy_from_slice(&bytes[..len]);

        let req: RegisterMessage = RegisterMessage::Request {
            session_id: session_id_local,
            c_nonce: c_nonce_local,
            username: uname_arr,
            password: password.as_bytes().to_vec(),
        };
        self.send(&req.encode(), socket)?;

        let mut buf = [0u8; 2048];
        let len;
        loop {
            if self.is_expired() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"));
            }

            match socket.recv_from(&mut buf) {
                Ok((l, _)) => {
                    if l != 0 {
                        len = l;
                        break;
                    }
                }
                Err((e, _)) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err((e, _)) => return Err(e),
            }

            std::thread::sleep(Duration::from_millis(2));
        }
        let resp = RegisterMessage::decode(&buf[1..len]).map_err(|e| io::Error::other(e))?;

        match resp {
            RegisterMessage::Response {
                session_id,
                s_nonce,
                salt,
                server_pub_bytes,
                confirmation,
                signature,
            } => {
                if session_id != session_id_local {
                    return Err(io::Error::other("sesion_id mistmatch"));
                }

                let actual_hash = sha256(&server_pub_bytes);
                if actual_hash != *trusted_pin {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "server public key pin mismatch",
                    ));
                }

                let verify_key =
                    VerifyingKey::from_bytes(&server_pub_bytes).map_err(|e| io::Error::other(e))?;
                let msg_to_verify = [
                    c_nonce_local.as_ref(),
                    s_nonce.as_ref(),
                    salt.as_ref(),
                    b"OK".as_ref(),
                ]
                .concat();
                verify_key
                    .verify(&msg_to_verify, &Signature::from_bytes(&signature))
                    .map_err(|e| io::Error::other(e))?;

                let expected = sha256(
                    &[
                        c_nonce_local.as_ref(),
                        s_nonce.as_ref(),
                        salt.as_ref(),
                        b"OK",
                    ]
                    .concat(),
                );
                if confirmation != expected {
                    return Err(io::Error::other("confirmation mismatch"));
                }

                println!("[voudp tls] registered successfully");
                println!("[voudp tls] salt: {:02x?}", salt);
                Ok(())
            }
            RegisterMessage::AlreadyExists => {
                eprintln!("[voudp tls] FAIL user already exists and cannot be registered");
                Err(io::Error::other(
                    "As it turns out, that user is already in the database",
                ))
            }
            RegisterMessage::RegistrationClosed => {
                eprintln!("[voudp tls] FAIL registration is closed on this server");
                Err(io::Error::other("Server is not registering new users"))
            }
            _ => return Err(io::Error::other("invalid register message")),
        }
    }

    pub fn start(&mut self, socket: &SecureUdpSocket) -> io::Result<()> {
        if self.step != HandshakeStep::ClientHello {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handshake already started",
            ));
        }

        self.session_id = rand::random();
        self.c_nonce = rand::random();

        println!(
            "starting handshake process with username '{}' and password '{}' [session {}]",
            self.username,
            self.password,
            u128::from_be_bytes(self.session_id)
        );

        println!("[voudp tls] generating ephemeral keypair...");
        let client_eph = EphemeralSecret::random();
        let e_pub_c = PublicKey::from(&client_eph);
        self.e_pub_c = e_pub_c.to_bytes();
        self.eph_secret = Some(client_eph); // store for later ECDH

        let mut uname_arr = [0u8; 32];
        let bytes = self.username.as_bytes();
        let len = bytes.len().min(32);
        uname_arr[..len].copy_from_slice(&bytes[..len]);

        let hello = HandshakeMessage::ClientHello {
            session_id: self.session_id,
            c_nonce: self.c_nonce,
            username: uname_arr,
        };

        self.send(&hello.encode(), socket)?;

        println!("[voudp tls] sending unique nonce and session id...");
        self.step = HandshakeStep::ClientHello; // waiting for ServerHello
        self.start_time = Instant::now();
        Ok(())
    }

    fn send(&mut self, data: &[u8], socket: &SecureUdpSocket) -> io::Result<()> {
        let mut packet = vec![ClientPacketType::Handshake as u8];
        packet.extend_from_slice(data);

        socket.send_reliable(packet, self.server_addr)
    }

    pub fn process(
        &mut self,
        socket: &SecureUdpSocket,
        data: &[u8],
        trusted_pin: &[u8; 32],
    ) -> io::Result<()> {
        let msg = HandshakeMessage::decode(data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        match (self.step, msg) {
            (
                HandshakeStep::ClientHello,
                HandshakeMessage::ServerHello {
                    session_id,
                    s_nonce,
                    server_pub_bytes,
                    salt,
                    signature,
                    e_pub_s,
                },
            ) => {
                if session_id != self.session_id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session_id mismatch",
                    ));
                }

                // verify pin
                let actual_pin = sha256(&server_pub_bytes);

                if actual_pin != *trusted_pin {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "server public key pin mismatch",
                    ));
                } else {
                    println!(
                        "[voudp tls] local server pubkey matched with the fingerprint sent by server"
                    );
                }

                println!("[voudp tls] verifying nonce mix...");
                // verify signature
                use ed25519_dalek::VerifyingKey;
                let verify_key = VerifyingKey::from_bytes(&server_pub_bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid server pub key")
                })?;
                let msg_to_verify = [
                    self.c_nonce.as_ref(),
                    s_nonce.as_ref(),
                    e_pub_s.as_ref(),
                    salt.as_ref(),
                ]
                .concat();
                verify_key
                    .verify(&msg_to_verify, &Signature::from_bytes(&signature))
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::PermissionDenied, "signature invalid")
                    })?;

                println!("[voudp tls] verified nonce mix. creating password challenge...");

                // store s_nonce and pub key
                self.s_nonce = s_nonce;
                self.server_pub_bytes = server_pub_bytes;
                self.e_pub_s = e_pub_s;

                let pw_hash = slow_hash(self.password.as_bytes(), &salt);

                let challenge = sha256(&[s_nonce.as_ref(), pw_hash.as_ref()].concat());

                let creds = HandshakeMessage::ClientCreds {
                    session_id: self.session_id,
                    challenge_response: challenge,
                    e_pub_c: self.e_pub_c,
                };

                let packet = creds.encode();
                self.send(&packet, socket)?;

                self.step = HandshakeStep::ClientCredentials; // waiting for ServerConfirm
                Ok(())
            }

            (
                HandshakeStep::ClientCredentials,
                HandshakeMessage::ServerConfirm {
                    session_id,
                    confirmation,
                },
            ) => {
                if session_id != self.session_id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session_id mismatch",
                    ));
                }

                let expected =
                    sha256(&[self.c_nonce.as_ref(), self.s_nonce.as_ref(), b"OK"].concat());
                if confirmation != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "server confirmation mismatch",
                    ));
                }

                let client_eph_secret = self.eph_secret.take().unwrap();
                let server_pub = PublicKey::from(self.e_pub_s);
                let shared_secret = client_eph_secret.diffie_hellman(&server_pub);

                let session_key =
                    derive_session_key(shared_secret.as_bytes(), &self.c_nonce, &self.s_nonce);

                println!("[voudp tls] derived inner AEAD session key using HKDF");
                self.key = Some(session_key);

                self.step = HandshakeStep::Done;
                println!(
                    "[voudp tls] handshake complete! verified server's confirmation signature"
                );
                println!("[voudp tls] continuing 2 layer AEAD enceyption");
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected message for current step",
            )),
        }
    }

    pub fn is_done(&self) -> bool {
        self.step == HandshakeStep::Done
    }

    pub fn session_key(&self) -> Result<Key, io::Error> {
        if !self.is_done() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "handshake not completed, session key unavailable",
            ));
        }
        if self.is_expired() {
            return Err(io::Error::other(
                "handshake expired, session key no longer valid",
            ));
        }
        match self.key {
            Some(key) => Ok(key),
            None => Err(io::Error::other(
                "handshake marked complete but session key missing",
            )),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.start_time.elapsed() > std::time::Duration::from_secs(5)
    }
}
