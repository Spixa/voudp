use rand::seq::IndexedRandom;
use std::collections::HashMap;

use crate::{
    server::{Channel, ServerState},
    socket::SecureUdpSocket,
    util::{CommandCategory, CommandContext, CommandResult, ServerCommand},
};

pub type CommandFn = Box<
    dyn Fn(&CommandContext, &mut std::collections::HashMap<u32, Channel>) -> CommandResult
        + Send
        + Sync,
>;

#[derive(Default)]
pub struct CommandSystem {
    commands: HashMap<String, (ServerCommand, CommandFn)>,
    command_aliases: HashMap<String, String>,
}

impl CommandSystem {
    pub fn new(socket: &SecureUdpSocket) -> Self {
        let mut system = Self {
            commands: HashMap::new(),
            command_aliases: HashMap::new(),
        };

        system.register_default_commands(socket);
        system
    }

    fn register_default_commands(&mut self, socket: &SecureUdpSocket) {
        self.register_command(
            ServerCommand {
                name: "/test".to_string(),
                description: "Test command".to_string(),
                usage: "/test <args>".to_string(),
                category: CommandCategory::Fun,
                aliases: vec!["/".to_string()],
                requires_auth: true,
                admin_only: false,
            },
            |ctx, _| {
                let mask = ctx.sender_mask.clone().unwrap();

                if mask.eq("spixa") {
                    CommandResult::Success("Good name".into())
                } else {
                    CommandResult::Error("Bad name".into())
                }
            },
        );

        let socket = socket.clone();

        let titles: Vec<String> = vec![
            "Incase you didn't know".into(),
            "Did you know that".into(),
            "You might benefit from knowing this".into(),
            "Just so you know".into(),
            "For your information".into(),
            "Have you heard that".into(),
            "Just so you know".into(),
        ];

        self.register_command(
            ServerCommand {
                name: "/me".to_string(),
                description: "Status command".to_string(),
                usage: "/me <activity>".to_string(),
                category: CommandCategory::User,
                aliases: vec!["/".to_string()],
                requires_auth: true,
                admin_only: false,
            },
            move |ctx, chans| {
                if ctx.arguments.is_empty() {
                    return CommandResult::Silent;
                }

                let mask = ctx.sender_mask.clone().unwrap();
                let activity = ctx.arguments.join(" ");

                let mut rng = rand::rng();
                let title = titles.choose(&mut rng).cloned();

                ServerState::broadcast_channel(
                    socket.clone(),
                    chans,
                    ctx.channel_id,
                    title.unwrap(),
                    format!("* {mask} {activity}"),
                );

                CommandResult::Silent
            },
        );

        self.register_command(
            ServerCommand {
                name: "/whoami".to_string(),
                description: "Show your current nickname and channel".to_string(),
                usage: "/whoami".to_string(),
                category: CommandCategory::User,
                aliases: vec![],
                requires_auth: true,
                admin_only: false,
            },
            |ctx, chans| {
                if let Some(channel) = chans.get(&ctx.channel_id) {
                    return CommandResult::Success(format!(
                        "You are @{} in {}",
                        ctx.sender_mask.clone().unwrap(),
                        if let Some(name) = &channel.name {
                            format!("#{name}")
                        } else {
                            format!("unnamed channel with id {}", ctx.channel_id)
                        }
                    ));
                }
                CommandResult::Silent
            },
        );

        self.register_command(
            ServerCommand {
                name: "/volume".to_string(),
                description: "Adjust volume of another user for yourself".to_string(),
                usage: "/volume <user> [0.0-2.0]".to_string(),
                category: CommandCategory::Audio,
                aliases: vec!["/vol".to_string(), "/gain".to_string()],
                requires_auth: false,
                admin_only: false,
            },
            move |ctx, chans| {
                if ctx.arguments.is_empty() {
                    return CommandResult::Error(
                        "usage: /volume <user> [volume (0.0-2.0)]".to_string(),
                    );
                }

                let target_user = &ctx.arguments[0];
                let new_vol = ctx.arguments.get(1).and_then(|v| v.parse::<f32>().ok());

                if let Some(channel) = chans.get(&ctx.channel_id) {
                    if let Some(requesting_remote) = channel
                        .remotes
                        .iter()
                        .find(|r| r.lock().unwrap().addr == ctx.sender_addr)
                    {
                        let mut remote = requesting_remote.lock().unwrap();

                        if let Some(vol) = new_vol {
                            let final_vol = vol.clamp(0.0, 2.0);
                            remote.set_user_volume(target_user, final_vol);
                            CommandResult::Success(format!(
                                "volume for '{}' set to {:.2} ({:.0}%)",
                                target_user,
                                final_vol,
                                final_vol * 100.0
                            ))
                        } else {
                            let current = remote.get_user_volume(target_user);
                            CommandResult::Success(format!(
                                "volume for '{}' is currently {} ({:.0}%)",
                                target_user,
                                current,
                                current * 100.0
                            ))
                        }
                    } else {
                        CommandResult::Error("You're not in a channel?".into())
                    }
                } else {
                    CommandResult::Error("Channel not found".into())
                }
            },
        );

        self.register_command(
            ServerCommand {
                name: "/volumes".to_string(),
                description: "List your volume adjustments".to_string(),
                usage: "/volumes".to_string(),
                category: CommandCategory::Audio,
                aliases: vec!["/vols".to_string()],
                requires_auth: true,
                admin_only: false,
            },
            move |ctx, chans| {
                if let Some(channel) = chans.get(&ctx.channel_id) {
                    if let Some(remote) = channel
                        .remotes
                        .iter()
                        .find(|r| r.lock().unwrap().addr == ctx.sender_addr)
                    {
                        let remote = remote.lock().unwrap();
                        if remote.volume_settings.is_empty() {
                            return CommandResult::Success("No custom volume settings".into());
                        }

                        let settings: Vec<String> = remote
                            .volume_settings
                            .iter()
                            .map(|(user, vol)| format!("{}: {:.1}", user, vol))
                            .collect();

                        CommandResult::Success(format!(
                            "Your volume settings:\n{}",
                            settings.join("\n")
                        ))
                    } else {
                        CommandResult::Error("Not found".into())
                    }
                } else {
                    CommandResult::Error("Channel not found".into())
                }
            },
        );

        //     self.register_command(ServerCommand {
        //         name: "/join".to_string(),
        //         description: "Switch to another channel".to_string(),
        //         usage: "/join <channel_id>".to_string(),
        //         category: CommandCategory::Channel,
        //         aliases: vec!["/j".to_string(), "/switch".to_string()],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/list".to_string(),
        //         description: "List all channels and users".to_string(),
        //         usage: "/list".to_string(),
        //         category: CommandCategory::Channel,
        //         aliases: vec!["/channels".to_string(), "/ls".to_string()],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/mute".to_string(),
        //         description: "Toggle your microphone mute".to_string(),
        //         usage: "/mute".to_string(),
        //         category: CommandCategory::Audio,
        //         aliases: vec![],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/deafen".to_string(),
        //         description: "Toggle your speaker deafen".to_string(),
        //         usage: "/deafen".to_string(),
        //         category: CommandCategory::Audio,
        //         aliases: vec![],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/me".to_string(),
        //         description: "Perform an action".to_string(),
        //         usage: "/me <action>".to_string(),
        //         category: CommandCategory::Chat,
        //         aliases: vec![],
        //         requires_auth: true,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/whisper".to_string(),
        //         description: "Send a private message".to_string(),
        //         usage: "/whisper <user> <message>".to_string(),
        //         category: CommandCategory::Chat,
        //         aliases: vec!["/w".to_string(), "/msg".to_string(), "/tell".to_string()],
        //         requires_auth: true,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/kick".to_string(),
        //         description: "Kick a user from the server".to_string(),
        //         usage: "/kick <user> [reason]".to_string(),
        //         category: CommandCategory::Admin,
        //         aliases: vec![],
        //         requires_auth: true,
        //         admin_only: true,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/ban".to_string(),
        //         description: "Ban a user from the server".to_string(),
        //         usage: "/ban <user> [reason]".to_string(),
        //         category: CommandCategory::Admin,
        //         aliases: vec![],
        //         requires_auth: true,
        //         admin_only: true,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/help".to_string(),
        //         description: "Show help for commands".to_string(),
        //         usage: "/help [command]".to_string(),
        //         category: CommandCategory::Utility,
        //         aliases: vec!["/?".to_string(), "/commands".to_string()],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/ping".to_string(),
        //         description: "Check server latency".to_string(),
        //         usage: "/ping".to_string(),
        //         category: CommandCategory::Utility,
        //         aliases: vec![],
        //         requires_auth: false,
        //         admin_only: false,
        //     });

        //     self.register_command(ServerCommand {
        //         name: "/serverinfo".to_string(),
        //         description: "Show server information".to_string(),
        //         usage: "/serverinfo".to_string(),
        //         category: CommandCategory::Utility,
        //         aliases: vec!["/info".to_string(), "/status".to_string()],
        //         requires_auth: false,
        //         admin_only: false,
        //     });
    }

    pub fn register_command<F>(&mut self, command: ServerCommand, f: F)
    where
        F: Fn(&CommandContext, &mut std::collections::HashMap<u32, Channel>) -> CommandResult
            + Send
            + Sync
            + 'static,
    {
        let cmd_name = command.name.clone();

        // Insert main command
        self.commands
            .insert(cmd_name.clone(), (command.clone(), Box::new(f)));

        // Insert aliases pointing to the same function
        for alias in &command.aliases {
            self.command_aliases.insert(alias.clone(), cmd_name.clone());
        }
    }

    pub fn get_command(&self, name: &str) -> Option<(&ServerCommand, &CommandFn)> {
        let actual_name = self
            .command_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);

        self.commands
            .get(actual_name)
            .map(|(cmd, func)| (cmd, func))
    }

    pub fn get_all_commands(&self) -> Vec<&ServerCommand> {
        self.commands.values().map(|a| &a.0).collect()
    }

    pub fn get_commands_for_user(&self, is_admin: bool) -> Vec<&ServerCommand> {
        self.commands
            .values()
            .filter(|cmd| !cmd.0.admin_only || is_admin)
            .map(|a| &a.0)
            .collect()
    }

    pub fn parse_command(&self, input: &str) -> Option<(&ServerCommand, &CommandFn, Vec<String>)> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command_name = parts[0];
        let arguments: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        self.get_command(command_name)
            .map(|(cmd, func)| (cmd, func, arguments))
    }
}
