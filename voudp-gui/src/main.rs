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
use voudp::client::{self, ClientState, GlobalListState, Message};

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
}

#[derive(Default)]
enum ShowMode {
    #[default]
    DontShow,
    ShowError,
    ShowMaskScreen,
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
                                        RichText::new(format!("📢 Channel #{}", channel.channel_id))
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

                                        // Unmasked users count
                                        if channel.unmasked_count > 0 {
                                            ui.separator();
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new("👻").small());
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} unmasked users",
                                                        channel.unmasked_count
                                                    ))
                                                    .color(Color32::GRAY),
                                                );
                                            });
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
                                        RichText::new(format!("Updating every second"))
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
                        ui.horizontal(|ui| {
                            self.talking_indicator(ui);

                            let available_width = ui.available_width() - 115.0;
                            let text_edit = egui::TextEdit::singleline(&mut self.input)
                                .hint_text("type your message...")
                                .text_color(Color32::from_rgb(255, 215, 0));

                            let response = ui.add_sized([available_width, 24.0], text_edit);

                            ui.add_sized([60.0, 24.0], egui::Button::new("Send"))
                                .clicked()
                                .then(|| self.send_message());

                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                self.send_message();
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
        let is_talking = self
            .client
            .clone()
            .unwrap()
            .lock()
            .unwrap()
            .talking
            .load(std::sync::atomic::Ordering::Relaxed);

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

    fn join_channel(&self, id: u32) {
        if let Some(client) = &self.client {
            if let Err(e) = client.lock().unwrap().join(id) {
                eprintln!(
                    "we faced an error when trying to join channel {}: {}",
                    id, e
                );
            }
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

        let client = match &self.client {
            Some(client) => client.lock().unwrap(),
            None => return,
        };

        client.send(&msg);

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
