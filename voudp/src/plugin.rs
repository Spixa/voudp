use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};

use log::{error, info};
use mlua::{Lua, RegistryKey, UserData, UserDataMethods};

pub enum PluginAction {
    Reply {
        to: String,
        msg: String,
    },
    Broadcast {
        msg: String,
    },
    Kick {
        user: String,
        reason: Option<String>,
    },
}

#[derive(Debug)]
pub struct PluginMetadata {
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
}

pub struct JoinContext {
    _username: String,
}

impl UserData for JoinContext {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("broadcast", |_, _, msg: String| {
            println!("[broadcast] {}", msg);
            Ok(())
        });
    }
}

pub struct MessageContext {
    pub username: String,
    pub message: String,
    cancelled: Arc<AtomicBool>,
    tx: Sender<PluginAction>,
}

impl UserData for MessageContext {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("get_message", |_, ctx, ()| Ok(ctx.message.clone()));
        methods.add_method("get_username", |_, ctx, ()| Ok(ctx.username.clone()));

        methods.add_method("reply", |_, ctx, msg: String| {
            // info!("relying");
            ctx.tx
                .send(PluginAction::Reply {
                    to: ctx.username.clone(),
                    msg,
                })
                .ok();
            Ok(())
        });

        methods.add_method("kick", |_, ctx, reason: String| {
            ctx.tx
                .send(PluginAction::Kick {
                    user: ctx.username.clone(),
                    reason: Some(reason),
                })
                .ok();
            Ok(())
        });

        methods.add_method("cancel", |_, ctx, ()| {
            ctx.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        });

        methods.add_method("broadcast", |_, _, _: String| {
            // unimplemeted!();
            Ok(())
        });
    }
}

pub struct LeaveContext {
    pub username: String,
}

impl UserData for LeaveContext {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("broadcast", |_, _, msg: String| {
            println!("[broadcast] {}", msg);
            Ok(())
        });
    }
}

pub struct Plugin {
    pub metadata: PluginMetadata,
    pub lua: Lua,
    pub on_join: Option<RegistryKey>,
    pub on_message: Option<RegistryKey>,
    pub on_leave: Option<RegistryKey>,
}

impl Plugin {
    pub fn load(path: &Path) -> mlua::Result<Self> {
        let lua = Lua::new();

        let code = std::fs::read_to_string(path)?;
        lua.load(&code).exec()?;

        // Everything that borrows `lua` lives in this block
        let (metadata, on_join, on_message, on_leave) = {
            let globals = lua.globals();

            // --- metadata ---
            let plugin_table: mlua::Table = globals.get("plugin")?;

            let metadata = PluginMetadata {
                name: plugin_table.get("name")?,
                version: plugin_table.get("version").ok(),
                author: plugin_table.get("author").ok(),
                description: plugin_table.get("description").ok(),
            };

            // --- callbacks ---
            let on_join = globals
                .get::<_, mlua::Function>("on_join")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;

            let on_message = globals
                .get::<_, mlua::Function>("on_message")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;

            let on_leave = globals
                .get::<_, mlua::Function>("on_leave")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;

            (metadata, on_join, on_message, on_leave)
        };

        Ok(Self {
            metadata,
            lua,
            on_join,
            on_message,
            on_leave,
        })
    }
}

pub struct PluginManager {
    plugins: Vec<Plugin>,
    sender: Sender<PluginAction>,
}

impl PluginManager {
    pub fn new(sender: Sender<PluginAction>) -> Self {
        Self {
            plugins: Vec::new(),
            sender,
        }
    }

    pub fn load_plugin(&mut self, path: &Path) {
        match Plugin::load(path) {
            Ok(plugin) => {
                info!(
                    "Loaded plugin: {} {} {} {}",
                    plugin.metadata.name,
                    if let Some(ref version) = plugin.metadata.version {
                        format!("v{}", version)
                    } else {
                        "".into()
                    },
                    if let Some(ref author) = plugin.metadata.author {
                        format!("written by {}", author)
                    } else {
                        "".into()
                    },
                    if let Some(ref desc) = plugin.metadata.description {
                        format!("\n\tDescription: {desc}")
                    } else {
                        "".into()
                    }
                );
                self.plugins.push(plugin);
            }
            Err(e) => {
                error!("Failed to load plugin {:?}: {}", path, e);
            }
        }
    }

    pub fn dispatch_join(&self, username: &str) {
        for plugin in &self.plugins {
            if let Some(key) = &plugin.on_join {
                let func: mlua::Function = match plugin.lua.registry_value(key) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("{}: {}", plugin.metadata.name, e);
                        continue;
                    }
                };

                let ctx = JoinContext {
                    _username: username.to_string(),
                };

                if let Err(e) = func.call::<_, ()>(ctx) {
                    eprintln!("{} on_join error: {}", plugin.metadata.name, e);
                }
            }
        }
    }

    pub fn dispatch_message(&self, username: &str, message: &str) -> bool {
        // return type means if it is cancelled
        let cancelled = Arc::new(AtomicBool::new(false)); // message isnt cancelled by default

        for plugin in &self.plugins {
            if let Some(key) = &plugin.on_message {
                let func: mlua::Function = match plugin.lua.registry_value(key) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("{}: {}", plugin.metadata.name, e);
                        continue;
                    }
                };

                let ctx = MessageContext {
                    username: username.to_string(),
                    message: message.to_string(),
                    cancelled: cancelled.clone(),
                    tx: self.sender.clone(),
                };

                if let Err(e) = func.call::<_, ()>(ctx) {
                    eprintln!("{} on_message error: {}", plugin.metadata.name, e);
                }

                if cancelled.load(Ordering::SeqCst) {
                    return false;
                }
            }
        }

        true
    }

    pub fn dispatch_leave(&self, username: &str) {
        for plugin in &self.plugins {
            if let Some(key) = &plugin.on_leave {
                let func: mlua::Function = match plugin.lua.registry_value(key) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("{}: {}", plugin.metadata.name, e);
                        continue;
                    }
                };

                let ctx = LeaveContext {
                    username: username.to_string(),
                };

                if let Err(e) = func.call::<_, ()>(ctx) {
                    eprintln!("{} on_leave error: {}", plugin.metadata.name, e);
                }
            }
        }
    }
}
