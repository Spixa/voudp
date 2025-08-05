use anyhow::Result;
use eframe::{NativeOptions, egui};
use egui::{Color32, RichText};
use log::info;
use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
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
}

#[derive(Default)]
struct ErrorWindow {
    show: bool,
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
            client: None,
            client_thread: None,
            error: Default::default(),
        }
    }
}

impl eframe::App for GuiClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.error.show {
                egui::Window::new(egui::RichText::new("Connection Error").color(Color32::WHITE))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, -50.0])
                    .show(ctx, |ui| {
                        ui.label(egui::RichText::new(&self.error.message).color(Color32::RED));
                        ui.separator();

                        ui.horizontal(|ui| {
                            if ui
                                .button(egui::RichText::new("Go back").color(Color32::LIGHT_GRAY))
                                .clicked()
                            {
                                self.error.show = false;
                            }
                        });
                    });
            }

            let available = ui.available_size();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(available.y * 0.2);

                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading(RichText::new("VoUDP GUI Client"));
                        ui.add_space(10.0);

                        if !self.is_connected {
                            ui.vertical_centered(|ui| {
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
                            });
                        }

                        ui.add_space(10.0);

                        if !self.is_connected {
                            if ui.button("🔗 Connect").clicked() {
                                let chan_id = match self.chan_id_text.parse::<u32>() {
                                    Ok(num) => num,
                                    Err(_) => {
                                        self.error.show = true;
                                        self.error.message = "Bad channel ID".into();
                                        return;
                                    }
                                };

                                match ClientState::new(&self.address, chan_id) {
                                    Ok(state) => {
                                        info!("Connected to server at {}", self.address);
                                        let arc_state = Arc::new(Mutex::new(state));
                                        let thread_state = arc_state.clone();

                                        let handle = std::thread::spawn(move || {
                                            let _ =
                                                thread_state.lock().unwrap().run(client::Mode::Gui);
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
                        } else {
                            if ui.button("❌ Disconnect").clicked() {
                                if let Some(client) = &self.client {
                                    client.lock().unwrap().disconnect();
                                }

                                if let Some(handle) = self.client_thread.take() {
                                    handle.join().ok();
                                }
                                self.is_connected = false;
                                self.client = None;
                            }

                            ui.add_space(5.0);

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
                            }

                            if ui
                                .button(if self.deafened {
                                    "🧏 Undeafen"
                                } else {
                                    "🔊 Deafen"
                                })
                                .clicked()
                            {
                                self.deafened = !self.deafened;
                                if let Some(client) = &self.client {
                                    client.lock().unwrap().set_deafened(self.deafened);
                                }
                            }
                        }

                        ui.add_space(10.0);

                        if ui.button("❓ Toggle Help").clicked() {
                            self.show_help = !self.show_help;
                        }

                        if self.show_help {
                            ui.separator();
                            ui.label("📝 Commands:");
                            ui.label("- Mute/Unmute: Toggle your microphone");
                            ui.label("- Deafen/Undeafen: Toggle your output");
                            ui.label("- Disconnect: Leave the server");
                        }
                    });
                });
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}
