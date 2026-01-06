use std::collections::HashMap;

use crate::util::{CommandCategory, ServerCommand};

#[derive(Default)]
pub struct CommandSystem {
    commands: HashMap<String, ServerCommand>,
    command_aliases: HashMap<String, String>,
}

impl CommandSystem {
    pub fn new() -> Self {
        let mut system = Self {
            commands: HashMap::new(),
            command_aliases: HashMap::new(),
        };

        system.register_default_commands();
        system
    }

    fn register_default_commands(&mut self) {
        // User commands
        self.register_command(ServerCommand {
            name: "/nick".to_string(),
            description: "Change your nickname".to_string(),
            usage: "/nick <name>".to_string(),
            category: CommandCategory::User,
            aliases: vec!["/name".to_string(), "/username".to_string()],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/whoami".to_string(),
            description: "Show your current nickname and channel".to_string(),
            usage: "/whoami".to_string(),
            category: CommandCategory::User,
            aliases: vec![],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/join".to_string(),
            description: "Switch to another channel".to_string(),
            usage: "/join <channel_id>".to_string(),
            category: CommandCategory::Channel,
            aliases: vec!["/j".to_string(), "/switch".to_string()],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/list".to_string(),
            description: "List all channels and users".to_string(),
            usage: "/list".to_string(),
            category: CommandCategory::Channel,
            aliases: vec!["/channels".to_string(), "/ls".to_string()],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/mute".to_string(),
            description: "Toggle your microphone mute".to_string(),
            usage: "/mute".to_string(),
            category: CommandCategory::Audio,
            aliases: vec![],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/deafen".to_string(),
            description: "Toggle your speaker deafen".to_string(),
            usage: "/deafen".to_string(),
            category: CommandCategory::Audio,
            aliases: vec![],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/me".to_string(),
            description: "Perform an action".to_string(),
            usage: "/me <action>".to_string(),
            category: CommandCategory::Chat,
            aliases: vec![],
            requires_auth: true,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/whisper".to_string(),
            description: "Send a private message".to_string(),
            usage: "/whisper <user> <message>".to_string(),
            category: CommandCategory::Chat,
            aliases: vec!["/w".to_string(), "/msg".to_string(), "/tell".to_string()],
            requires_auth: true,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/kick".to_string(),
            description: "Kick a user from the server".to_string(),
            usage: "/kick <user> [reason]".to_string(),
            category: CommandCategory::Admin,
            aliases: vec![],
            requires_auth: true,
            admin_only: true,
        });

        self.register_command(ServerCommand {
            name: "/ban".to_string(),
            description: "Ban a user from the server".to_string(),
            usage: "/ban <user> [reason]".to_string(),
            category: CommandCategory::Admin,
            aliases: vec![],
            requires_auth: true,
            admin_only: true,
        });

        self.register_command(ServerCommand {
            name: "/help".to_string(),
            description: "Show help for commands".to_string(),
            usage: "/help [command]".to_string(),
            category: CommandCategory::Utility,
            aliases: vec!["/?".to_string(), "/commands".to_string()],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/ping".to_string(),
            description: "Check server latency".to_string(),
            usage: "/ping".to_string(),
            category: CommandCategory::Utility,
            aliases: vec![],
            requires_auth: false,
            admin_only: false,
        });

        self.register_command(ServerCommand {
            name: "/serverinfo".to_string(),
            description: "Show server information".to_string(),
            usage: "/serverinfo".to_string(),
            category: CommandCategory::Utility,
            aliases: vec!["/info".to_string(), "/status".to_string()],
            requires_auth: false,
            admin_only: false,
        });
    }

    fn register_command(&mut self, command: ServerCommand) {
        self.commands.insert(command.name.clone(), command.clone());

        // Register aliases
        for alias in &command.aliases {
            self.command_aliases
                .insert(alias.clone(), command.name.clone());
        }
    }

    pub fn get_command(&self, name: &str) -> Option<&ServerCommand> {
        // Check if it's an alias
        let actual_name = self
            .command_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);

        self.commands.get(actual_name)
    }

    pub fn get_all_commands(&self) -> Vec<&ServerCommand> {
        self.commands.values().collect()
    }

    pub fn get_commands_for_user(&self, is_admin: bool) -> Vec<&ServerCommand> {
        self.commands
            .values()
            .filter(|cmd| !cmd.admin_only || is_admin)
            .collect()
    }

    pub fn parse_command(&self, input: &str) -> Option<(&ServerCommand, Vec<String>)> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command_name = parts[0];
        let arguments = parts[1..].iter().map(|s| s.to_string()).collect();

        self.get_command(command_name).map(|cmd| (cmd, arguments))
    }
}
