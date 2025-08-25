use anyhow::Result;
use chrono::{DateTime, Local};
use core::f32;
use eframe::{NativeOptions, egui};
use egui::{Color32, RichText};
use log::info;
use std::{
    sync::{Arc, Mutex, RwLock, mpsc::TryRecvError},
    thread::{self, JoinHandle},
};
use voudp::client::{self, ClientState};

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
    address: String,
    chan_id_text: String,
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
    unmasked_count: u32,
    masked_users: Vec<(String, bool, bool)>,
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
            chan_id_text: "1".to_string(),
            is_connected: false,
            muted: false,
            deafened: false,
            show_help: false,
            nicked: false,
            client: None,
            client_thread: None,
            error: Default::default(),
            logs: Default::default(),
            unmasked_count: 0,
            masked_users: Vec::new(),
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
                        ui.label("🔌 Channel ID:");
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

                            match ClientState::new(&self.address, chan_id) {
                                Ok(state) => {
                                    info!("Connected to server at {}", self.address);

                                    self.write_log(
                                        format!(
                                            "Connected to {} in channel {}",
                                            self.address, chan_id
                                        ),
                                        Color32::YELLOW,
                                    );
                                    self.write_log(
                                        "Hope you enjoy your stay".into(),
                                        Color32::GREEN,
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
                                    eprintln!("Failed to connect: {:?}", e);
                                }
                            }
                        }
                    });
                });
            });
        } else {
            // Connected UI
            egui::SidePanel::right("user_list_panel")
                .resizable(true)
                .default_width(180.0)
                .show(ctx, |ui| {
                    ui.heading("📜 Users");
                    ui.label(format!("Unmasked: {}", self.unmasked_count));
                    ui.label(format!("Masked: {}", self.masked_users.len()));
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if self.masked_users.is_empty() {
                            ui.label("No masked users connected.");
                        } else {
                            for (name, muted, deafened) in &self.masked_users {
                                ui.horizontal(|ui| {
                                    // Connection dot
                                    ui.label(
                                        RichText::new("●")
                                            .color(Color32::from_rgb(0, 200, 0))
                                            .monospace(),
                                    );

                                    // Name
                                    ui.label(RichText::new(name).strong());

                                    // Status icons / text
                                    if *muted {
                                        ui.label(RichText::new("🔇").size(14.0)); // muted icon
                                    }
                                    if *deafened {
                                        ui.label(RichText::new("🙉").size(14.0)); // deafened icon
                                    }
                                });
                            }
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
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            let text_edit = egui::TextEdit::singleline(&mut self.input)
                                .hint_text("type your message...")
                                .text_color(Color32::from_rgb(255, 215, 0));

                            let response =
                                ui.add_sized([ui.available_width() - 130.0, 24.0], text_edit);

                            // width is fixed
                            ui.add_sized([60.0, 24.0], egui::Button::new("Send"))
                                .clicked()
                                .then(|| self.send_message());

                            // regain focus when we send
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

        // === Update user list ===
        {
            let Some(client) = self.client.clone() else {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            };
            let client = client.lock().unwrap();
            let list = client.list.lock().unwrap();
            self.unmasked_count = list.unmasked;
            self.masked_users = list.masked.clone();
        }

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
                Ok((name, msg, time)) => {
                    self.logs.write().unwrap().push((
                        format!("{name}: {msg}"),
                        Color32::WHITE,
                        time,
                    ));
                }
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

impl GuiClientApp {
    fn write_log(&mut self, log: String, color: Color32) {
        self.logs.write().unwrap().push((log, color, Local::now()));
    }

    fn send_message(&mut self) {
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
        self.input.clear();
    }
}
