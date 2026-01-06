use anyhow::Result;
use chrono::{DateTime, Local};
use core::f32;
use eframe::{NativeOptions, egui};
use egui::{Color32, Id, RichText};
use log::info;
use std::{
    sync::{Arc, Mutex, RwLock, mpsc::TryRecvError},
    thread::{self, JoinHandle},
    time::Instant,
};
use voudp::{
    client::{self, ClientState, GlobalListState, Message},
    util::{SecureUdpSocket, ServerCommand},
};

fn main() -> Result<()> {
    // Initialize logging
    pretty_env_logger::init_timed();

    // Run the egui application
    let options = NativeOptions {
        // drag_and_drop_support: true,
        // initial_window_size: Some(egui::vec2(400.0, 300.0)),
        ..Default::default()
    };

    eframe::run_native(
        "VoUDP GUI Client",
        options,
        Box::new(|_cc| Box::new(GuiClientApp::default())),
    )
    .unwrap();

    Ok(())
}

type LogVec = Arc<RwLock<Vec<(String, egui::Color32, DateTime<Local>)>>>;

struct GuiClientApp {
    global_list: GlobalListState,
    command_list: Vec<ServerCommand>,
    socket: Option<SecureUdpSocket>,
    current_channel_id: u32,
    address: String,
    chan_id_text: String,
    phrase: String,
    is_connected: bool,
    muted: bool,
    deafened: bool,
    show_help: bool,
    client: Option<Arc<Mutex<ClientState>>>,
    client_thread: Option<JoinHandle<()>>,
    error: ErrorWindow,
    input: String,
    nick: String,
    nicked: bool,
    logs: LogVec,
    show_command_suggestions: bool,
    selected_suggestion: usize,
    filter_text: String,
}

#[derive(Default)]
enum ShowMode {
    #[default]
    DontShow,
    ShowError,
    ShowMaskScreen,
}

enum CommandAction {
    UseCommand(String),
    ShowNickWarning,
}

#[derive(Default)]
struct ErrorWindow {
    show: ShowMode,
    message: String,
}

impl Default for GuiClientApp {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:37549".to_string(),
            current_channel_id: 0,
            global_list: GlobalListState {
                channels: vec![],
                last_updated: Instant::now(),
                current_channel: 0,
            },
            command_list: vec![],
            socket: None,
            chan_id_text: "1".to_string(),
            phrase: "".to_string(),
            is_connected: false,
            muted: false,
            deafened: false,
            show_help: false,
            nicked: false,
            client: None,
            client_thread: None,
            error: Default::default(),
            logs: Default::default(),
            input: Default::default(),
            nick: Default::default(),
            show_command_suggestions: false,
            selected_suggestion: 0,
            filter_text: String::new(),
        }
    }
}
impl eframe::App for GuiClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Connection error popup
        match self.error.show {
            ShowMode::ShowError => {
                egui::Window::new(RichText::new("Connection Error").color(Color32::WHITE))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, -50.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new(&self.error.message).color(Color32::RED));
                        ui.separator();
                        if ui
                            .button(RichText::new("Go back").color(Color32::LIGHT_GRAY))
                            .clicked()
                        {
                            self.error.show = ShowMode::DontShow;
                        }
                    });
            }
            ShowMode::ShowMaskScreen => {
                egui::Window::new(RichText::new("You are not nicked!").color(Color32::YELLOW))
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("🔌 Enter nickname:");
                        let textedit = ui
                            .add(egui::TextEdit::singleline(&mut self.nick).hint_text("Nickname"));

                        ui.memory_mut(|mem| mem.request_focus(textedit.id));
                        if ui
                            .button(RichText::new("Use nickname").color(Color32::LIGHT_GREEN))
                            .clicked()
                        {
                            self.error.show = ShowMode::DontShow;
                            self.nicked = true;
                            self.set_nick();
                            self.send_message();
                        }

                        if ui
                            .button(RichText::new("Don't nick").color(Color32::LIGHT_RED))
                            .clicked()
                        {
                            self.error.show = ShowMode::DontShow;
                            self.input = String::new();
                            self.nick = String::new();
                        }
                    });
            }
            _ => {}
        }

        if !self.is_connected {
            egui::CentralPanel::default().show(ctx, |ui| {
                let available = ui.available_size();
                ui.add_space(available.y * 0.2);

                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading(RichText::new("VoUDP GUI Client"));
                        ui.add_space(10.0);

                        ui.label("🔌 Server Address:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.address)
                                .hint_text("server address (ip:port)"),
                        );

                        ui.label("🔑 Server password:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.phrase)
                                .hint_text("usually \'voudp\'")
                                .password(true),
                        );

                        ui.label("🔗 Channel ID:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.chan_id_text)
                                .hint_text("ID")
                                .char_limit(2)
                                .desired_width(20.0),
                        );

                        ui.add_space(10.0);

                        if ui.button("🔗 Connect").clicked() {
                            let chan_id = match self.chan_id_text.parse::<u32>() {
                                Ok(num) => num,
                                Err(_) => {
                                    self.error.show = ShowMode::ShowError;
                                    self.error.message = "Bad channel ID".into();
                                    return;
                                }
                            };

                            match ClientState::new(
                                &self.address,
                                chan_id,
                                &self.phrase.clone().into_bytes(),
                            ) {
                                Ok(state) => {
                                    info!("Connected to server at {}", self.address);

                                    self.socket = Some(state.socket.clone());

                                    self.write_log(
                                        format!(
                                            "Connected to {} in channel #{}",
                                            self.address, chan_id
                                        ),
                                        Color32::KHAKI,
                                    );

                                    self.write_log(
                                        "You need to mask to appear for others".into(),
                                        Color32::DARK_GREEN,
                                    );

                                    let arc_state = Arc::new(Mutex::new(state));
                                    let thread_state = arc_state.clone();

                                    let handle = std::thread::spawn(move || {
                                        let _ = thread_state.lock().unwrap().run(client::Mode::Gui);
                                    });

                                    self.client_thread = Some(handle);
                                    self.client = Some(arc_state);
                                    self.is_connected = true;
                                }
                                Err(e) => {
                                    self.error.show = ShowMode::ShowError;
                                    self.error.message =
                                        format!("Failed to connect to the server: {}", e);
                                }
                            }
                        }
                    });
                });
            });
        } else {
            self.update_global_list();
            self.update_command_list();

            if self.input.starts_with('/') && self.command_list.is_empty() {
                self.request_command_list();
            }

            let typed_cmd = self
                .input
                .strip_prefix('/')
                .map(|s| s.split_whitespace().next().unwrap_or(""))
                .unwrap_or("");

            self.filter_text = typed_cmd.to_string();
            self.show_command_suggestions = self.input.starts_with('/') && !self.input.is_empty();

            egui::SidePanel::right("global_list_panel")
                .resizable(true)
                .default_width(250.0)
                .min_width(200.0)
                .max_width(400.0)
                .show(ctx, |ui| {
                    ui.heading("🌐 Server List");

                    // Server summary at the top
                    let total_users = self
                        .global_list
                        .channels
                        .iter()
                        .map(|c| c.unmasked_count as usize + c.masked_users.len())
                        .sum::<usize>();
                    let total_channels = self.global_list.channels.len();

                    ui.horizontal(|ui| {
                        ui.label(RichText::new("📊").size(18.0));
                        ui.label(format!("{} users", total_users));
                        ui.label("•");
                        ui.label(format!("{} channels", total_channels));
                    });

                    ui.separator();

                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            if self.global_list.channels.is_empty() {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(20.0);
                                    ui.label(
                                        RichText::new("No active channels")
                                            .color(Color32::GRAY)
                                            .italics(),
                                    );
                                    ui.add_space(20.0);
                                });
                            } else {
                                for channel in &self.global_list.channels {
                                    let is_current_channel =
                                        channel.channel_id == self.current_channel_id;

                                    let header = if is_current_channel {
                                        RichText::new(format!(
                                            "📢 Channel #{} (connected: stereo)",
                                            channel.channel_id
                                        ))
                                        .color(Color32::LIGHT_GREEN)
                                        .strong()
                                    } else {
                                        RichText::new(format!("🔈 Channel #{}", channel.channel_id))
                                            .color(Color32::LIGHT_BLUE)
                                    };

                                    let total_in_channel = channel.unmasked_count as usize
                                        + channel.masked_users.len();

                                    let response = ui.collapsing(header, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("👤").small());
                                            ui.label(format!("{} users", total_in_channel));
                                            if channel.unmasked_count > 0 {
                                                ui.label(RichText::new("•").color(Color32::GRAY));
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} unmasked",
                                                        channel.unmasked_count
                                                    ))
                                                    .color(Color32::YELLOW),
                                                );
                                            }
                                        });

                                        ui.separator();

                                        if channel.masked_users.is_empty() {
                                            ui.label(
                                                RichText::new("No masked users")
                                                    .color(Color32::GRAY)
                                                    .small(),
                                            );
                                        } else {
                                            for (name, muted, deafened) in &channel.masked_users {
                                                ui.horizontal(|ui| {
                                                    // Status indicator
                                                    let status_color = match (*muted, *deafened) {
                                                        (true, true) => Color32::RED,
                                                        (true, false) => Color32::BLUE,
                                                        (false, true) => Color32::YELLOW,
                                                        (false, false) => Color32::GREEN,
                                                    };
                                                    ui.label(
                                                        RichText::new("•").color(status_color),
                                                    );

                                                    // Name
                                                    ui.label(RichText::new(name));

                                                    // Status icons
                                                    if *muted {
                                                        ui.label(RichText::new("🔇").small());
                                                    }
                                                    if *deafened {
                                                        ui.label(RichText::new("🙉").small());
                                                    }
                                                });
                                            }
                                        }

                                        if !is_current_channel {
                                            ui.button("Join").clicked().then(|| {
                                                self.join_channel(channel.channel_id);
                                            });
                                        }
                                    });

                                    if is_current_channel {
                                        ui.painter().rect_stroke(
                                            response.header_response.rect.expand(2.0),
                                            4.0,
                                            egui::Stroke::new(1.0, Color32::LIGHT_GREEN),
                                        );
                                    }
                                }
                            }

                            ui.add_space(10.0);

                            // Refresh button
                            if ui.button("🔄 Refresh List").clicked() {
                                self.request_global_list();
                            }

                            // Last updated time
                            if !self.global_list.channels.is_empty() {
                                ui.add_space(5.0);
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🕐").small());
                                    ui.label(
                                        RichText::new("Updating every second".to_string())
                                            .color(Color32::GRAY)
                                            .small(),
                                    );
                                });
                            }
                        });
                });

            egui::CentralPanel::default().show(ctx, |ui| {
                // Action buttons row
                ui.horizontal(|ui| {
                    if ui.button("❌ Disconnect").clicked() {
                        if let Some(client) = &self.client {
                            client.lock().unwrap().disconnect();
                        }

                        if let Some(handle) = self.client_thread.take() {
                            handle.join().ok();
                        }
                        self.is_connected = false;
                        self.nicked = false;
                        self.nick = String::new();
                        self.client = None;
                        self.write_log("Goodbye!".into(), Color32::GREEN);
                        self.write_log(
                            format!(
                                "Sent EOF to {}. It is now handling our departure",
                                self.address
                            ),
                            Color32::YELLOW,
                        );
                    }

                    if ui
                        .button(if self.muted {
                            "🔈 Unmute"
                        } else {
                            "🔇 Mute"
                        })
                        .clicked()
                    {
                        self.muted = !self.muted;
                        if let Some(client) = &self.client {
                            client.lock().unwrap().set_muted(self.muted);
                        }
                        if self.muted {
                            self.write_log("Microphone muted".into(), Color32::RED);
                        } else {
                            self.write_log("Microphone unmuted".into(), Color32::LIGHT_GREEN);
                        }
                    }

                    if ui
                        .button(if self.deafened {
                            "🔈 Undeafen"
                        } else {
                            "🔇 Deafen"
                        })
                        .clicked()
                    {
                        self.deafened = !self.deafened;
                        if let Some(client) = &self.client {
                            client.lock().unwrap().set_deafened(self.deafened);
                        }
                        if self.deafened {
                            self.write_log("Speaker deafened".into(), Color32::RED);
                        } else {
                            self.write_log("Speaker undeafened".into(), Color32::LIGHT_GREEN);
                        }
                    }

                    if ui.button("❓ Help").clicked() {
                        self.show_help = !self.show_help;
                    }

                    if ui.button("🧹 Clear Logs").clicked() {
                        self.logs.write().unwrap().clear();
                        self.write_log("Cleared logs".into(), Color32::WHITE);
                    }
                });

                ui.separator();

                if self.show_help {
                    ui.collapsing("📝 Commands", |ui| {
                        ui.label("- Mute/Unmute: Toggle your microphone");
                        ui.label("- Deafen/Undeafen: Toggle your output");
                        ui.label("- Disconnect: Leave the server");
                    });
                    ui.separator();
                }

                // Logs area
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false; 2])
                    .max_width(f32::INFINITY)
                    .max_height(ui.available_height() - 50.0)
                    .show(ui, |ui| {
                        for (msg, color, time) in self.logs.read().unwrap().iter() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}  ", time.format("%H:%M:%S")))
                                        .color(egui::Color32::GRAY)
                                        .monospace(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(msg)
                                            .text_style(egui::TextStyle::Monospace)
                                            .color(*color),
                                    )
                                    .wrap(true),
                                );
                            });
                        }
                    });

                egui::TopBottomPanel::bottom("input_panel")
                    .show_separator_line(true)
                    .show_inside(ui, |ui| {
                        let input_id = ui.make_persistent_id("chat_input");

                        ui.horizontal(|ui| {
                            self.talking_indicator(ui);

                            let available_width = ui.available_width() - 115.0;
                            let text_edit = egui::TextEdit::singleline(&mut self.input)
                                .hint_text("type your message/command...")
                                .text_color(Color32::from_rgb(255, 215, 0));

                            let response = ui.add_sized([available_width, 24.0], text_edit);

                            ui.memory_mut(|mem| {
                                mem.data.insert_temp(response.id, response.clone())
                            });

                            if self.show_command_suggestions && !self.command_list.is_empty() {
                                let handled = self.handle_command_nav(ui.ctx(), response.id);

                                if !handled {
                                    self.show_command_suggestions_ui(ui, input_id);
                                }
                            }

                            ui.add_sized([60.0, 24.0], egui::Button::new("Send"))
                                .clicked()
                                .then(|| {
                                    if self.input.starts_with('/') {
                                        self.execute_command();
                                    } else {
                                        self.send_message();
                                    }
                                });

                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                if self.input.starts_with('/') {
                                    self.execute_command();
                                } else {
                                    self.send_message();
                                }
                                ui.memory_mut(|mem| mem.request_focus(response.id));
                            }

                            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Tab))
                            {
                                self.tab_complete();
                                ui.memory_mut(|mem| mem.request_focus(response.id));
                            }
                        });
                    });
            });
        }

        // TODO: merge this with the upper block
        // === Update chat logs ===
        {
            let Some(client) = self.client.clone() else {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            };
            let client = client.lock().unwrap();
            let Some(ref rx) = client.rx else {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            };
            match rx.try_recv() {
                Ok((msg, time)) => match msg {
                    Message::JoinMessage(name) => {
                        self.logs.write().unwrap().push((
                            format!("{name} joined the channel"),
                            Color32::YELLOW,
                            time,
                        ));
                    }
                    Message::LeaveMessage(name) => {
                        self.logs.write().unwrap().push((
                            format!("{name} left the channel"),
                            Color32::YELLOW,
                            time,
                        ));
                    }
                    Message::ChatMessage(name, content) => {
                        let channel = self.current_channel_id;
                        self.logs.write().unwrap().push((
                            format!("[#{channel}] {name}: {content}"),
                            Color32::WHITE,
                            time,
                        ));
                    }
                    Message::Broadcast(alias, content) => {
                        self.logs.write().unwrap().push((
                            format!("[{alias}] {content}"),
                            Color32::LIGHT_GREEN,
                            time,
                        ));
                    }
                },
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

impl GuiClientApp {
    fn talking_indicator(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let is_talking = self.client.clone();

        let is_talking = match is_talking {
            Some(a) => a
                .lock()
                .unwrap()
                .talking
                .load(std::sync::atomic::Ordering::Relaxed),
            None => false,
        };

        let response = ui.add(egui::Label::new(""));

        if is_talking {
            let time = ui.input(|i| i.time);
            let pulse = 0.5 + 0.5 * (time * 3.0).sin();

            let center = response.rect.center();
            ui.painter().circle_filled(
                center,
                6.0,
                Color32::from_rgba_premultiplied(0, 255, 0, (220.0 * pulse) as u8),
            );

            if response.hovered() {
                egui::show_tooltip_at_pointer(ui.ctx(), Id::new("talking_tooltip"), |ui| {
                    ui.label("Voice activity detected")
                });
            }
        }

        response
    }

    fn write_log(&mut self, log: String, color: Color32) {
        self.logs.write().unwrap().push((log, color, Local::now()));
    }

    fn request_global_list(&self) {
        if let Some(client) = &self.client {
            let packet = vec![0x05]; // Request global list
            client.lock().unwrap().send(&packet);
        }
    }

    fn request_command_list(&self) {
        if let Some(client) = &self.client {
            let packet = vec![0x0c]; // Request global list
            client.lock().unwrap().send(&packet);
        }
    }

    fn join_channel(&self, id: u32) {
        if let Some(client) = &self.client
            && let Err(e) = client.lock().unwrap().join(id)
        {
            eprintln!(
                "we faced an error when trying to join channel {}: {}",
                id, e
            );
        }
    }

    fn update_global_list(&mut self) {
        if let Some(client) = &self.client {
            let client = client.lock().unwrap();
            let list_state = client.list.lock().unwrap();

            self.global_list.channels = list_state.channels.clone();
            self.global_list.last_updated = Instant::now();
            self.global_list.current_channel = list_state.current_channel;
            self.current_channel_id = list_state.current_channel;
        }
    }

    fn update_command_list(&mut self) {
        if let Some(client) = &self.client {
            let client = client.lock().unwrap();
            let list_state = client.cmd_list.lock().unwrap();
            self.command_list = list_state.to_vec();
        }
    }

    fn handle_command_nav(&mut self, ctx: &egui::Context, input_id: egui::Id) -> bool {
        if !self.show_command_suggestions || self.command_list.is_empty() {
            return false;
        }

        // Store filter text and selection before any borrowing
        let filter_text = self.filter_text.clone();
        let current_selection = self.selected_suggestion;

        // Calculate filtered count WITHOUT borrowing self
        let filtered_count = self
            .command_list
            .iter()
            .filter(|cmd| {
                let name_match = cmd.name[1..]
                    .to_lowercase()
                    .starts_with(&filter_text.to_lowercase());
                let alias_match = cmd.aliases.iter().any(|alias| {
                    alias[1..]
                        .to_lowercase()
                        .starts_with(&filter_text.to_lowercase())
                });
                name_match || alias_match
            })
            .count();

        if filtered_count == 0 {
            return false;
        }

        let mut handled = false;

        // Handle arrow down
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.selected_suggestion = (current_selection + 1) % filtered_count;
            handled = true;
        }

        // Handle arrow up
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            if current_selection == 0 {
                self.selected_suggestion = filtered_count - 1;
            } else {
                self.selected_suggestion = current_selection - 1;
            }
            handled = true;
        }

        // Handle Enter key - need to get the actual command now
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && filtered_count > 0 {
            // Get filtered commands AFTER we've updated selected_suggestion if needed
            let filtered_commands: Vec<&ServerCommand> = self
                .command_list
                .iter()
                .filter(|cmd| {
                    let name_match = cmd.name[1..]
                        .to_lowercase()
                        .starts_with(&filter_text.to_lowercase());
                    let alias_match = cmd.aliases.iter().any(|alias| {
                        alias[1..]
                            .to_lowercase()
                            .starts_with(&filter_text.to_lowercase())
                    });
                    name_match || alias_match
                })
                .collect();

            // Use the current selection (which might have been updated by arrow keys)
            let selection_to_use = if handled
                && (ctx.input(|i| i.key_pressed(egui::Key::ArrowDown))
                    || ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)))
            {
                self.selected_suggestion
            } else {
                current_selection
            };

            if let Some(command) = filtered_commands.get(selection_to_use) {
                let requires_auth_warning = command.requires_auth && !self.nicked;

                if requires_auth_warning {
                    self.error.show = ShowMode::ShowMaskScreen;
                    self.error.message = "You need to set a nickname first!".to_string();
                } else {
                    self.input = format!("{} ", command.name);
                }

                self.show_command_suggestions = false;
                ctx.memory_mut(|mem| mem.request_focus(input_id));
            }
            handled = true;
        }

        // Escape key closes suggestions
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_command_suggestions = false;
            ctx.memory_mut(|mem| mem.request_focus(input_id));
            handled = true;
        }

        handled
    }

    fn get_filtered_commands(&self) -> Vec<&ServerCommand> {
        if self.filter_text.is_empty() {
            return self.command_list.iter().collect();
        }

        self.command_list
            .iter()
            .filter(|cmd| {
                let name_match = cmd.name[1..]
                    .to_lowercase()
                    .starts_with(&self.filter_text.to_lowercase());
                let alias_match = cmd.aliases.iter().any(|alias| {
                    alias[1..]
                        .to_lowercase()
                        .starts_with(&self.filter_text.to_lowercase())
                });
                name_match || alias_match
            })
            .collect()
    }

    fn show_command_suggestions_ui(&mut self, ui: &mut egui::Ui, input_id: egui::Id) {
        let filtered_commands = self.get_filtered_commands();

        if filtered_commands.is_empty() {
            return;
        }

        let input_response = ui.memory(|mem| mem.data.get_temp::<egui::Response>(input_id));
        let input_rect = input_response.map(|r| r.rect).unwrap_or_else(|| {
            // fallback: use a default position
            ui.min_rect()
        });

        let max_visible = 8;
        let visible_count = filtered_commands.len().min(max_visible);
        let suggestion_height = (visible_count as f32 * 28.0).min(200.0);

        let popup_pos = egui::pos2(input_rect.min.x, input_rect.min.y - suggestion_height - 5.0);

        let popup_id = egui::Id::new("command_suggestions_popup");

        let mut action_to_take: Option<CommandAction> = None;

        let area = egui::Area::new(popup_id)
            .order(egui::Order::Tooltip)
            .fixed_pos(popup_pos);

        area.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(5.0)
                .show(ui, |ui| {
                    ui.set_width(350.0);
                    ui.set_max_height(suggestion_height);

                    egui::ScrollArea::vertical()
                        .max_height(suggestion_height)
                        .show(ui, |ui| {
                            for (i, command) in filtered_commands.iter().enumerate() {
                                let is_selected = i == self.selected_suggestion;
                                let requires_auth_warning = command.requires_auth && !self.nicked;

                                let row_response = ui
                                    .horizontal(|ui| {
                                        let name_display = if requires_auth_warning {
                                            format!("⚠ {}", command.name)
                                        } else {
                                            command.name.clone()
                                        };

                                        let name_color = if is_selected {
                                            Color32::WHITE
                                        } else if requires_auth_warning {
                                            Color32::YELLOW
                                        } else {
                                            Color32::LIGHT_BLUE
                                        };

                                        ui.label(RichText::new(&name_display).color(name_color));

                                        ui.add_space(ui.available_width() - 150.0);

                                        let desc = if command.description.len() > 30 {
                                            format!("{}...", &command.description[..27])
                                        } else {
                                            command.description.clone()
                                        };

                                        ui.label(RichText::new(desc).color(Color32::GRAY).small());
                                    })
                                    .response;

                                if is_selected {
                                    ui.painter().rect_filled(
                                        row_response.rect,
                                        2.0,
                                        Color32::from_rgba_unmultiplied(65, 105, 225, 50),
                                    );

                                    ui.scroll_to_rect(row_response.rect, Some(egui::Align::Center));
                                }

                                if row_response.clicked() {
                                    if requires_auth_warning {
                                        action_to_take = Some(CommandAction::ShowNickWarning);
                                    } else {
                                        action_to_take =
                                            Some(CommandAction::UseCommand(command.name.clone()));
                                    }
                                }

                                if row_response.hovered() {
                                    let mut tooltip = command.description.clone();
                                    if requires_auth_warning {
                                        tooltip =
                                            format!("{}\n\n⚠ Requires nickname first!", tooltip);
                                    }
                                    if command.admin_only {
                                        tooltip = format!("{}\n\n🛡️ Admin only", tooltip);
                                    }

                                    egui::show_tooltip_at_pointer(
                                        ui.ctx(),
                                        Id::new("cmd_tt"),
                                        |ui| {
                                            ui.label(tooltip);
                                        },
                                    );
                                }
                            }
                        });
                });
        });

        match action_to_take {
            Some(CommandAction::UseCommand(cmd_name)) => {
                self.input = format!("{} ", cmd_name);
                self.show_command_suggestions = false;
                ui.ctx().memory_mut(|mem| mem.request_focus(input_id));
            }
            Some(CommandAction::ShowNickWarning) => {
                self.error.show = ShowMode::ShowMaskScreen;
                self.error.message = "You need to set a nickname first!".to_string();
                self.show_command_suggestions = false;
            }
            None => {}
        }
    }

    fn tab_complete(&mut self) {
        let filtered_commands = self.get_filtered_commands();

        if filtered_commands.is_empty() {
            return;
        }

        if filtered_commands.len() == 1 {
            let command = filtered_commands[0];
            self.input = format!("{} ", command.name);
            self.show_command_suggestions = false;
            return;
        }

        let common_prefix = self.find_common_prefix(&filtered_commands);
        if !common_prefix.is_empty() && common_prefix != self.filter_text {
            self.input = format!("/{}", common_prefix);
            self.filter_text = common_prefix;
        }
    }

    fn find_common_prefix(&self, commands: &[&ServerCommand]) -> String {
        if commands.is_empty() {
            return String::new();
        }

        let names: Vec<&str> = commands.iter().map(|cmd| &cmd.name[1..]).collect();

        let first = names[0];
        let mut prefix = String::new();

        for (i, ch) in first.char_indices() {
            for name in names.iter().skip(1) {
                if i >= name.len() || name.chars().nth(i) != Some(ch) {
                    return prefix;
                }
            }
            prefix.push(ch);
        }

        prefix
    }

    fn execute_command(&mut self) {
        if self.input.is_empty() || !self.input.starts_with('/') {
            return;
        }

        self.show_command_suggestions = false;
        self.selected_suggestion = 0;

        let mut msg = vec![0x0d];
        msg.extend_from_slice(self.input.as_bytes());

        if let Some(socket) = &self.socket {
            match socket.send(&msg) {
                Ok(_) => {}
                Err(e) => {
                    self.write_log(format!("Failed to send: {}", e), Color32::RED);
                }
            }
        } else {
            self.write_log("Not connected".to_string(), Color32::RED);
        }

        self.input.clear();
    }

    fn send_message(&mut self) {
        if self.input.is_empty() {
            return;
        }

        if !self.nicked {
            self.error.show = ShowMode::ShowMaskScreen;
            return;
        }

        let mut msg = vec![0x06];
        msg.extend_from_slice(self.input.as_bytes());

        if let Some(socket) = &self.socket {
            match socket.send(&msg) {
                Ok(_) => {}
                Err(e) => {
                    self.write_log(format!("Failed to send: {}", e), Color32::RED);
                }
            }
        } else {
            self.write_log("Not connected".to_string(), Color32::RED);
        }

        self.input.clear();
    }

    fn set_nick(&mut self) {
        let mut nick = vec![0x04];
        nick.extend_from_slice(self.nick.as_bytes());

        let client = match &self.client {
            Some(client) => client.lock().unwrap(),
            None => return,
        };

        client.send(&nick);
    }
}
