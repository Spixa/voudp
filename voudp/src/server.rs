use chrono::Local;
use log::{error, info, warn};
use opus2::{Application, Channels as OpusChannels, Decoder, Encoder};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    commands::CommandSystem,
    console_cmd::{ConsoleCommandResult, handle_command},
    mixer,
    protocol::{self, ClientPacketType, ConsolePacketType, ControlRequest, PASSWORD},
    socket::{self, SecureUdpSocket},
    util::{self, CommandCategory, CommandContext, CommandResult},
};
const JITTER_BUFFER_LEN: usize = 50;

#[derive(Clone, Copy, PartialEq)]
pub enum Clipping {
    Soft,
    Hard,
}

#[derive(Clone, Copy)]
pub struct ServerConfig {
    pub max_users: usize,
    pub should_normalize: bool,
    pub should_compress: bool,
    pub clipping: Clipping,
    pub compress_threshold: f32,
    pub compress_ratio: f32,
    pub bind_port: u16,
    pub timeout_secs: u64,
    pub throttle_millis: u64,
    pub sample_rate: u32,
    pub tickrate: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_users: 1024,
            should_normalize: true,
            should_compress: true,
            clipping: Clipping::Soft,
            compress_threshold: 0.5,
            compress_ratio: 0.8,
            bind_port: 0,
            timeout_secs: 5,
            throttle_millis: 1,
            sample_rate: 48000,
            tickrate: 50,
        }
    }
}

impl ServerConfig {
    pub fn get_framesize(&self) -> usize {
        (self.sample_rate / self.tickrate).try_into().unwrap()
    }
}

#[derive(Default, Clone, Copy)]
struct RemoteStatus {
    deaf: bool,
    mute: bool,
}

pub struct Remote {
    encoder: Encoder,
    decoder: Decoder,
    last_active: Instant,
    channel_id: u32,
    pub(crate) addr: SocketAddr,
    mask: Option<String>,
    jitter_buffer: VecDeque<Vec<f32>>,
    status: RemoteStatus,
}

impl Remote {
    fn new(addr: SocketAddr, sample_rate: u32) -> Result<Self, opus2::Error> {
        let encoder = Encoder::new(sample_rate, OpusChannels::Stereo, Application::Audio)?;
        let decoder = Decoder::new(sample_rate, OpusChannels::Stereo)?;

        info!(
            "New remote has initialized with addr {} (sample rate: {}, audio: {})",
            addr, sample_rate, "Stereo"
        );
        Ok(Self {
            encoder,
            decoder,
            last_active: Instant::now(),
            channel_id: 0,
            addr,
            mask: None,
            jitter_buffer: VecDeque::with_capacity(JITTER_BUFFER_LEN),
            status: Default::default(),
        })
    }
}

struct Console {
    _addr: SocketAddr,
    last_active: Instant,
}

impl Console {
    fn new(_addr: SocketAddr) -> Self {
        Self {
            _addr,
            last_active: Instant::now(),
        }
    }
}

type SafeRemote = Arc<Mutex<Remote>>;
type SafeConsole = Arc<Mutex<Console>>;
pub struct Channel {
    pub name: Option<String>,
    pub _id: u32,
    pub remotes: Vec<SafeRemote>,
    pub buffers: HashMap<SocketAddr, Vec<f32>>,
    pub filter_states: HashMap<SocketAddr, (f32, f32)>,
    pub server_config: ServerConfig,
}

impl Channel {
    pub fn new(server_config: ServerConfig, name: String, _id: u32) -> Self {
        info!("Created new channel with {_id}");
        Self {
            name: Some(name),
            _id,
            remotes: vec![],
            buffers: HashMap::new(),
            filter_states: HashMap::new(),
            server_config,
        }
    }

    fn add_remote(&mut self, remote: SafeRemote) {
        let addr = { remote.lock().unwrap().addr };
        self.remotes.push(remote);

        self.buffers
            .insert(addr, vec![0.0; self.server_config.get_framesize() * 2]);
        self.filter_states.insert(addr, (0.0, 0.0));
    }

    fn remove_remote(&mut self, addr: &SocketAddr) {
        self.remotes.retain(|c| c.lock().unwrap().addr != *addr);
        self.buffers.remove(addr);
        self.filter_states.remove(addr);
    }

    fn mix(&mut self, socket: &SecureUdpSocket) {
        // pre-proc audio for every remote:
        let mut processed_buffers = HashMap::new();
        for (addr, buf) in &self.buffers {
            if buf.len() != self.server_config.get_framesize() * 2 || mixer::is_silent(buf) {
                continue;
            }

            let state = self.filter_states.entry(*addr).or_insert((0.0, 0.0));
            let mut processed = buf.clone();
            mixer::remove_dc_bias(&mut processed, state);
            processed_buffers.insert(*addr, processed);
        }

        // personalized mix which is done separately
        for remote in &self.remotes {
            let mut guard = remote.lock().unwrap();
            let remote_addr = guard.addr;

            if !self.buffers.contains_key(&remote_addr) || guard.status.deaf {
                continue;
            }

            // collect all active talkers excluding self
            let talkers: Vec<_> = processed_buffers
                .iter()
                .filter(|(addr, _)| **addr != remote_addr)
                .collect();

            let active_count = talkers.len();
            if active_count == 0 {
                continue;
            }

            // compute gain once
            let gain = 1.0 / (active_count as f32).sqrt();

            let mut mix = vec![0.0f32; self.server_config.get_framesize() * 2];
            for (_, buf) in talkers {
                for (i, sample) in buf.iter().enumerate() {
                    mix[i] += sample * gain;
                }
            }

            if self.server_config.should_compress {
                mixer::compress(
                    &mut mix,
                    self.server_config.compress_threshold,
                    self.server_config.compress_ratio,
                );
            }

            if self.server_config.should_normalize {
                mixer::normalize(&mut mix);
            }

            match self.server_config.clipping {
                Clipping::Soft => mixer::soft_clip(&mut mix),
                Clipping::Hard => {
                    mix.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
                }
            }

            let mut encoded = vec![0u8; 400];
            let len = guard.encoder.encode_float(&mix, &mut encoded).unwrap_or(0);

            if len > 0 {
                let mut packet = vec![0x02];
                packet.extend_from_slice(&encoded[..len]);
                if let Err(e) = socket.send_to(&packet, remote_addr) {
                    error!("Failed to send audio to {remote_addr}: {e}");
                }
            }
        }

        // Clear buffers for next tick
        for buf in self.buffers.values_mut() {
            buf.fill(0.0);
        }
    }
}

pub struct ServerState {
    socket: Arc<SecureUdpSocket>,
    remotes: HashMap<SocketAddr, SafeRemote>,
    consoles: HashMap<SocketAddr, SafeConsole>,
    channels: HashMap<u32, Channel>,
    audio_rb: HeapRb<(SocketAddr, Vec<u8>)>,
    config: ServerConfig,
    command_system: CommandSystem,
}

impl ServerState {
    pub fn new(config: ServerConfig, phrase: &[u8]) -> Result<Self, io::Error> {
        info!("Deriving key from phrase...");
        let key = socket::derive_key_from_phrase(phrase, protocol::VOUDP_SALT);
        let socket = SecureUdpSocket::create(format!("0.0.0.0:{}", config.bind_port), key)?;

        info!("Bound to 0.0.0.0:{}", config.bind_port);
        let socket = Arc::new(socket); // wrap in Arc
        info!(
            "There are {} free buffers (max remotes that can connect)",
            config.max_users
        );

        let mut default_channels = HashMap::new();
        default_channels.insert(1, Channel::new(config, String::from("general"), 1));
        default_channels.insert(2, Channel::new(config, String::from("music"), 2));
        default_channels.insert(3, Channel::new(config, String::from("test"), 3));

        Ok(Self {
            socket: Arc::clone(&socket),
            remotes: HashMap::new(),
            consoles: HashMap::new(),
            channels: default_channels,
            audio_rb: HeapRb::new(config.max_users),
            config,
            command_system: CommandSystem::new(),
        })
    }

    fn handle_console(&mut self, addr: SocketAddr, data: &[u8]) {
        type Cpt = ConsolePacketType;
        match ConsolePacketType::try_from(data[0]) {
            Ok(Cpt::Cmd) => self.handle_console_command(addr, &data[1..]),
            Ok(Cpt::Eof) => self.handle_console_eof(addr),
            Ok(Cpt::Keepalive) => {}
            _ => error!(
                "Console {addr} sent an invalid packet (starts with {:#?}",
                data[0]
            ),
        }
    }

    fn handle_console_command(&mut self, addr: SocketAddr, data: &[u8]) {
        if let Ok(req) = String::from_utf8(data.to_vec()) {
            let parts: Vec<&str> = req.split_whitespace().collect();

            let reply: String = if !parts.is_empty() {
                let cmd = parts[0];

                match handle_command(cmd, &parts, &mut self.channels, &self.config, None) {
                    ConsoleCommandResult::Reply(msg) => msg,
                }
            } else {
                "server received your empty message".into()
            };

            if let Err(e) = self.socket.send_to(reply.as_bytes(), addr) {
                warn!("Could not reply back to console {addr} due to {e}");
            }
        } else {
            warn!("Received bad command from {addr}");
        }
    }

    fn handle_console_eof(&mut self, addr: SocketAddr) {
        self.consoles.retain(|addr_got, _| {
            if *addr_got == addr {
                info!("Console {addr} left the server");
                return false;
            }
            true
        });
    }

    fn handle_packet(&mut self, addr: SocketAddr, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if self.consoles.contains_key(&addr) {
            self.handle_console(addr, data);
            return;
        }

        type Cpt = ClientPacketType;
        match ClientPacketType::try_from(data[0]) {
            Ok(Cpt::Join) => self.handle_join(addr, &data[1..]),
            Ok(Cpt::Audio) => self.handle_audio(addr, &data[1..]),
            Ok(Cpt::Eof) => self.handle_eof(addr),
            Ok(Cpt::Mask) => self.handle_mask(addr, &data[1..]),
            Ok(Cpt::List) => self.handle_list(addr),
            Ok(Cpt::Chat) => self.handle_chat(addr, &data[1..]),
            Ok(Cpt::Ctrl) => self.handle_ctrl(addr, &data[1..]),
            Ok(Cpt::SyncCommands) => self.handle_sync_commands(addr),
            Ok(Cpt::Cmd) => self.handle_cmd(addr, &data[1..]),
            Ok(Cpt::RegisterConsole) => self.register_console(addr, &data[1..]),
            _ => error!(
                "{} sent an invalid packet (starts with {:#?})",
                addr, data[0]
            ),
        }
    }

    fn register_console(&mut self, addr: SocketAddr, data: &[u8]) {
        if let Ok(password) = String::from_utf8(data.to_vec()) {
            if password.eq(PASSWORD) {
                info!("Registered {addr} as a new console. Capabilties: cmd");
                self.consoles
                    .insert(addr, Arc::new(Mutex::new(Console::new(addr))));
            } else {
                info!("{addr} tried to log-in with the incorrect password");
                self.handle_bad(addr);
            }
        } else {
            warn!("{addr} sent a bad packet when wanting to register itself as a console")
        }
    }

    fn handle_join(&mut self, addr: SocketAddr, data: &[u8]) {
        if data.len() < 4 {
            return;
        }

        let chan_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

        if chan_id == 0 && chan_id >= u16::MAX as u32 {
            warn!("{addr} tried to join channel with id {chan_id}, but that id is invalid");
            return;
        }

        info!("{} has joined the channel with id {}", addr, chan_id);

        if !self.remotes.contains_key(&addr) {
            Self::dm(
                &self.socket,
                addr,
                format!(
                    "Welcome to this server. Server time is {}",
                    Local::now().format("%d/%m/%Y %H:%M:%S")
                ),
            );
        }

        let remote = self.remotes.entry(addr).or_insert_with(|| {
            info!("{} is a new remote", addr);
            Arc::new(Mutex::new(
                Remote::new(addr, self.config.sample_rate).expect("remote creation failed"),
            ))
        });

        let (old_channel_id, mask) = {
            let mut remote_guard = remote.lock().unwrap();
            let old_id = remote_guard.channel_id;
            let mask = remote_guard.mask.clone();
            remote_guard.channel_id = chan_id;
            (old_id, mask)
        };

        if old_channel_id != chan_id
            && old_channel_id != 0
            && let Some(old_channel) = self.channels.get_mut(&old_channel_id)
        {
            old_channel.remove_remote(&addr);
        }

        if let Some(mask) = mask {
            self.broadcast_join(chan_id, mask);
        }
        // add to new channel
        let channel = self
            .channels
            .entry(chan_id)
            .or_insert_with(|| Channel::new(self.config, format!("general-{chan_id}"), chan_id));

        if let Some(channel_name) = &channel.name {
            Self::dm(
                &self.socket,
                addr,
                format!("You have been moved to #{channel_name}"),
            );
        }

        if let Some(remote) = self.remotes.get(&addr) {
            channel.add_remote(remote.clone());
        }
    }

    fn handle_audio(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            return;
        };
        let mut remote = remote.lock().unwrap();

        remote.last_active = Instant::now();

        // push to ring buffer for audio processing:
        if self.audio_rb.is_full() {
            error!("audio buffer overflow");
            return;
        }

        self.audio_rb.try_push((addr, data.to_vec())).unwrap(); // impossible to panic because of previous check
    }

    fn handle_eof(&mut self, addr: SocketAddr) {
        self.remotes.retain(|addr_got, remote| {
            if *addr_got == addr {
                let channel_id = { remote.lock().unwrap().channel_id };
                let nick = { remote.lock().unwrap().mask.clone() };
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    info!("{addr} has left");

                    if let Some(nick) = nick {
                        info!("Broadcasting leave of {nick}");
                        let mut packet = vec![0x0b];
                        packet.extend_from_slice(nick.as_bytes());

                        for peer in &channel.remotes {
                            let peer_addr = { peer.lock().unwrap().addr };

                            if let Err(e) = self.socket.send_to(&packet, peer_addr) {
                                warn!("Failed to send leave packet to {}: {:?}", peer_addr, e);
                            }
                        }
                    }

                    channel.remove_remote(&addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                return false;
            }
            true
        });
    }

    // TODO: announce old mask in join message incase of renicking
    fn handle_mask(&mut self, addr: SocketAddr, data: &[u8]) {
        let (old_mask, new_mask, channel_id) = {
            let Some(remote) = self.remotes.get(&addr) else {
                warn!("Mask from unknown remote: {}, skipping request...", addr);
                return;
            };

            let remote_guard = remote.lock().unwrap();
            let old_mask = remote_guard.mask.clone();

            let channel_id = remote_guard.channel_id;
            let new_mask = match String::from_utf8(data.to_vec()) {
                Ok(mask) => mask,
                Err(_) => {
                    warn!("Mask sent over is not UTF-8, skipping request...");
                    return;
                }
            };

            drop(remote_guard);

            if new_mask.is_empty() {
                return;
            }

            remote.lock().unwrap().mask = Some(new_mask.clone());

            (old_mask, new_mask, channel_id)
        };

        info!(
            "{} has masked as '{}' in channel {}",
            addr, new_mask, channel_id
        );

        self.broadcast_join_masked(channel_id, new_mask, old_mask);
    }

    fn handle_list(&mut self, addr: SocketAddr) {
        let Some(remote) = self.remotes.get(&addr) else {
            warn!(
                "List request from unknown remote: {}, skipping request...",
                addr
            );
            return;
        };

        let remote_chan_id = {
            let mut remote = remote.lock().unwrap();
            remote.last_active = Instant::now();
            remote.channel_id
        };

        let mut channels_info = Vec::new();

        for (&chan_id, chan) in &self.channels {
            // if chan.remotes.is_empty() {
            //     continue;
            // }

            let (masked_users, unmasked_count): (Vec<(String, bool, bool)>, u32) = chan
                .remotes
                .iter()
                .map(|r| {
                    let r = r.lock().unwrap();
                    (r.mask.clone(), r.status.mute, r.status.deaf)
                })
                .fold(
                    (vec![], 0),
                    |(mut masks, count), (mask_opt, muted, deafened)| {
                        if let Some(mask) = mask_opt {
                            masks.push((mask, muted, deafened));
                            (masks, count)
                        } else {
                            (masks, count + 1)
                        }
                    },
                );

            let mut channel_info = Vec::new();

            if let Some(name) = &chan.name {
                channel_info.push(name.len() as u8);
                channel_info.extend_from_slice(name.as_bytes());
            } else {
                channel_info.extend_from_slice(&[0x0]);
            }

            channel_info.extend_from_slice(&chan_id.to_be_bytes());
            channel_info.extend_from_slice(&unmasked_count.to_be_bytes());
            channel_info.extend_from_slice(&(masked_users.len() as u32).to_be_bytes());

            for (mask, muted, deafened) in &masked_users {
                channel_info.extend_from_slice(mask.as_bytes());
                channel_info.push(0x01);
                let flags = (*muted as u8) | ((*deafened as u8) << 1);
                channel_info.push(flags);
            }

            channels_info.push(channel_info);
        }

        let mut list_packet = vec![0x05];
        list_packet.extend_from_slice(&remote_chan_id.to_be_bytes());
        list_packet.extend_from_slice(&(channels_info.len() as u32).to_be_bytes());

        for chan_info in channels_info {
            list_packet.extend_from_slice(&chan_info);
        }

        if let Err(e) = self.socket.send_to(&list_packet, addr) {
            warn!("Failed to send global list to {}: {}", addr, e);
        }
    }

    fn handle_chat(&mut self, addr: SocketAddr, data: &[u8]) {
        let (mask, chan_id) = {
            let Some(remote) = self.remotes.get(&addr) else {
                warn!(
                    "Chat request from unknown remote: {}, skipping request...",
                    addr
                );
                return;
            };
            let remote = remote.lock().unwrap();

            (remote.mask.clone(), remote.channel_id)
        };

        let Some(channel) = self.channels.get(&chan_id) else {
            warn!(
                "Failed to retrieve the channel of remote {}, skipping request...",
                addr
            );
            return;
        };

        match mask {
            Some(mask) => {
                let Ok(msg) = String::from_utf8(data.to_vec()) else {
                    warn!("{addr} sent a non UTF-8 encoded chat string");
                    return;
                };

                let sender_addr = addr;
                for remote in channel.remotes.iter() {
                    let addr = { remote.lock().unwrap().addr };
                    let is_self = addr.eq(&sender_addr);

                    let mut msg_packet = vec![ClientPacketType::Chat as u8];
                    msg_packet.extend_from_slice(mask.as_bytes());
                    msg_packet.push(0x01);
                    msg_packet.push(is_self as u8);
                    msg_packet.extend_from_slice(data);

                    let _ = self.socket.send_to(&msg_packet, addr);
                }

                info!("[#chan-{}] <{}> {}", chan_id, mask, msg);
            }
            None => {
                let unauth_packet = vec![0x07];
                let _ = self.socket.send_to(&unauth_packet, addr);
                warn!("{addr} tried sending chat message without having a mask!");
            }
        }
    }

    pub fn handle_ctrl(&mut self, addr: SocketAddr, data: &[u8]) {
        let Some(remote) = self.remotes.get(&addr) else {
            warn!(
                "Control request from unknown remote: {}, skipping request...",
                addr
            );
            return;
        };
        let mut remote = remote.lock().unwrap();

        type Cq = ControlRequest;
        match util::parse_control_packet(data) {
            Ok(req) => match req {
                Cq::SetDeafen => remote.status.deaf = true,
                Cq::SetUndeafen => remote.status.deaf = false,
                Cq::SetMute => remote.status.mute = true,
                Cq::SetUnmute => remote.status.mute = false,
                // Cq::SetVolume(_) => warn!("{addr} accessed an unimplemented feature"),
            },
            Err(e) => {
                warn!("{addr} sent a bad control packet: {e}");
            }
        }
    }

    pub fn handle_cmd(&mut self, addr: SocketAddr, data: &[u8]) {
        let input = match String::from_utf8(data.to_vec()) {
            Ok(s) => s,
            Err(_) => {
                warn!("Invalid UTF-8 in command from {}", addr);
                return;
            }
        };

        let (mask, channel_id, is_admin) = {
            let Some(remote) = self.remotes.get(&addr) else {
                warn!("Command from unknown remote: {}", addr);
                return;
            };

            let remote = remote.lock().unwrap();
            (remote.mask.clone(), remote.channel_id, false)
        };

        // execute command
        let result = self.execute_command(&input, addr, mask.as_deref(), channel_id, is_admin);

        match result {
            CommandResult::Success(msg) => {
                let mut packet = vec![0x0e]; // command success response 
                packet.extend_from_slice(msg.as_bytes());
                let _ = self.socket.send_to(&packet, addr);
            }
            CommandResult::Error(msg) => {
                let mut packet = vec![0x0f]; // command fail response 
                packet.extend_from_slice(msg.as_bytes());
                let _ = self.socket.send_to(&packet, addr);
            }
            CommandResult::Silent => {}
        };
    }

    pub fn handle_sync_commands(&mut self, addr: SocketAddr) {
        let is_admin = false;
        let available_commands = self.command_system.get_commands_for_user(is_admin);

        let mut packet = vec![0x0c];
        packet.extend_from_slice(&(available_commands.len() as u16).to_be_bytes());

        for cmd in available_commands {
            packet.push(cmd.name.len() as u8);
            packet.extend_from_slice(cmd.name.as_bytes());

            packet.push(cmd.description.len() as u8);
            packet.extend_from_slice(cmd.description.as_bytes());

            packet.push(cmd.usage.len() as u8);
            packet.extend_from_slice(cmd.usage.as_bytes());

            type Cc = CommandCategory;
            let category_byte = match cmd.category {
                Cc::User => 0,
                Cc::Channel => 1,
                Cc::Audio => 2,
                Cc::Chat => 3,
                Cc::Admin => 4,
                Cc::Utility => 5,
                Cc::Fun => 6,
            };
            packet.push(category_byte);

            let mut flags = 0u8;
            if cmd.requires_auth {
                flags |= 0b00000001;
            }
            if cmd.admin_only {
                flags |= 0b00000010;
            }
            packet.push(flags);

            packet.push(cmd.aliases.len() as u8);
            for alias in &cmd.aliases {
                packet.push(alias.len() as u8);
                packet.extend_from_slice(alias.as_bytes());
            }
        }

        if let Err(e) = self.socket.send_to(&packet, addr) {
            warn!("Failed to send command sync to {}: {}", addr, e);
        }
    }

    pub fn handle_bad(&mut self, addr: SocketAddr) {
        warn!("{addr} sent a bad packet");
        let _ = self.socket.send_bad_packet_notice(addr);
    }

    fn dm(socket: &SecureUdpSocket, addr: SocketAddr, msg: String) {
        let mut packet = vec![0x11];
        packet.extend_from_slice(msg.as_bytes());
        let _ = socket.send_to(&packet, addr);
    }

    fn execute_command(
        &mut self,
        input: &str,
        sender_addr: SocketAddr,
        sender_mask: Option<&str>,
        channel_id: u32,
        is_admin: bool,
    ) -> CommandResult {
        let (command, args) = match self.command_system.parse_command(input) {
            Some((cmd, args)) => (cmd, args),
            None => {
                return CommandResult::Error(
                    "Unknown command. Type /help for available commands.".to_string(),
                );
            }
        };

        if command.requires_auth && sender_mask.is_none() {
            return CommandResult::Error("You need to set a nickname first with /nick".to_string());
        }

        if command.admin_only && !is_admin {
            return CommandResult::Error(
                "You don't have permission to use this command.".to_string(),
            );
        }

        let _context = CommandContext {
            sender_addr,
            sender_mask: sender_mask.map(|s| s.to_string()),
            channel_id,
            arguments: args,
            is_admin,
        };

        match command.name.as_str() {
            "/nick" => {}
            "/other" => {}
            _ => {}
        }

        CommandResult::Silent
    }

    fn process_audio_tick(&mut self) {
        let framesize = self.config.get_framesize();
        // decode incoming packets and fill jitter buffers
        while let Some((addr, data)) = self.audio_rb.try_pop() {
            let Some(remote) = self.remotes.get(&addr) else {
                continue;
            };
            let mut remote = remote.lock().unwrap();

            let mut pcm = vec![0.0f32; framesize * 2];
            match remote.decoder.decode_float(&data, &mut pcm, false) {
                Ok(len) if len == framesize => {
                    if remote.jitter_buffer.len() < JITTER_BUFFER_LEN {
                        remote.jitter_buffer.push_back(pcm);
                    } else {
                        warn!("Jitter buffer full for {addr}");
                    }
                }
                Ok(len) => error!("Bad frame size from {addr}: got {len}, expected {framesize}"),
                Err(e) => error!("Decode error from {addr}: {e:?}"),
            }
        }

        // Pull one frame per remote into channel buffer
        for (addr, remote) in &self.remotes {
            let mut remote = remote.lock().unwrap();
            let chan_id = remote.channel_id;
            let frame =
                remote
                    .jitter_buffer
                    .pop_front()
                    .unwrap_or(vec![0.0; self.config.get_framesize() * 2]);

            if let Some(channel) = self.channels.get_mut(&chan_id) {
                channel.buffers.insert(*addr, frame);
            }
        }

        for channel in self.channels.values_mut() {
            channel.mix(&self.socket);
        }
    }

    fn broadcast_join(&mut self, channel_id: u32, mask: String) {
        self.broadcast_join_masked(channel_id, mask, None);
    }

    fn broadcast_join_masked(
        &mut self,
        channel_id: u32,
        new_mask: String,
        old_mask: Option<String>,
    ) {
        let peer_addresses: Vec<SocketAddr> = if let Some(channel) = self.channels.get(&channel_id)
        {
            channel
                .remotes
                .iter()
                .map(|r| r.lock().unwrap().addr)
                .collect()
        } else {
            Vec::new()
        };

        let packet = if let Some(old) = old_mask {
            let mut packet = vec![ClientPacketType::FlowRenick as u8];
            packet.push(old.len() as u8);
            packet.extend_from_slice(old.as_bytes());

            packet.push(new_mask.len() as u8);
            packet.extend_from_slice(new_mask.as_bytes());

            packet
        } else {
            let mut packet = vec![ClientPacketType::FlowJoin as u8];
            packet.extend_from_slice(new_mask.as_bytes());
            packet
        };

        for peer_addr in peer_addresses {
            if let Err(e) = self.socket.send_to(&packet, peer_addr) {
                warn!("Failed to send mask packet to {}: {:?}", peer_addr, e);
            }
        }
    }

    fn cleanup(&mut self) {
        let now = Instant::now();

        self.consoles.retain(|addr, guard| {
            let console = guard.lock().unwrap();

            if console.last_active.duration_since(now)
                > Duration::from_secs(self.config.timeout_secs)
            {
                info!("Dropped console {addr} due to timeout");
                false
            } else {
                true
            }
        });

        self.remotes.retain(|addr, remote| {
            let last_active = { remote.lock().unwrap().last_active };
            let nick = { remote.lock().unwrap().mask.clone() };
            let channel_id = { remote.lock().unwrap().channel_id };

            if now.duration_since(last_active) > Duration::from_secs(self.config.timeout_secs) {
                if let Some(channel) = self.channels.get_mut(&channel_id) {
                    info!(
                        "{addr} is dropped due to timeout of {} seconds",
                        self.config.timeout_secs
                    );

                    if let Some(nick) = nick {
                        info!("Broadcasting leave of {nick}");
                        let mut packet = vec![0x0b];
                        packet.extend_from_slice(nick.as_bytes());

                        for peer in &channel.remotes {
                            let peer_addr = { peer.lock().unwrap().addr };

                            if let Err(e) = self.socket.send_to(&packet, peer_addr) {
                                warn!("Failed to send leave packet to {}: {:?}", peer_addr, e);
                            }
                        }
                    }
                    channel.remove_remote(addr);
                } // if this is false, the remote is channel-less which i don't know how that would even happen
                false // remote hasn't updated in the past N seconds, needs to be kicked
            } else {
                true // remote can stay alive
            }
        });
    }

    pub fn run(&mut self) {
        let mut buf = [0u8; 2048];
        let mut next_tick = Instant::now();

        let throttle = self.config.throttle_millis;
        let tick_period = 1000 / self.config.tickrate as u64; // in ms
        info!(
            "Tick period is {}ms ({} tps) with {}ms throttles",
            tick_period, self.config.tickrate, throttle
        );
        info!(
            "Sample rate is {} ({} samples per tick per audio channel)",
            self.config.sample_rate,
            self.config.get_framesize()
        );

        if self.config.should_compress {
            info!(
                "Audio compression is enabled with threshold {} and ratio {}",
                self.config.compress_threshold, self.config.compress_ratio
            )
        } else {
            info!("Audio compression is disabled");
        }

        if self.config.should_normalize {
            info!("Audio normalization is enabled");
        } else {
            info!("Audio normalization is disabled");
        }

        if !self.config.should_compress
            && !self.config.should_normalize
            && self.config.clipping == Clipping::Hard
        {
            warn!(
                "This setting is not recommended (No compression, no normalization, with hard clipping)"
            );
        }

        match self.config.clipping {
            Clipping::Soft => info!("Samples are set to be soft-clipped"),
            Clipping::Hard => info!("Samples are set to be hard-clipped"),
        }

        loop {
            loop {
                match self.socket.recv_from(&mut buf) {
                    Ok((size, addr)) => {
                        self.handle_packet(addr, &buf[..size]);
                    }
                    Err(ref e) if e.0.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        self.handle_bad(e.1);
                        break;
                    }
                }
            }

            if Instant::now() >= next_tick {
                self.process_audio_tick();
                self.cleanup();
                next_tick += Duration::from_millis(tick_period);
            }

            std::thread::sleep(Duration::from_millis(throttle));
        }
    }
}
