mod bubble;

use anyhow::Result;
use chrono::{DateTime, Local};
use core::f32;
use eframe::{NativeOptions, egui};
use egui::{Color32, Id, RichText, Stroke};

use std::{
    fs::File,
    io::{self, Read, Write},
    sync::{Arc, Mutex, RwLock, atomic::Ordering, mpsc::TryRecvError},
    thread::{self, JoinHandle},
    time::Instant,
};

use voudp::{
    client::{self, ClientState, GlobalListState, Message},
    socket::SecureUdpSocket,
    util::ServerCommand,
};

use crate::bubble::{badge, bubble_ui, parse_chat_message, parse_system_message};

fn main() -> Result<()> {
    pretty_env_logger::init_timed();

    let options = NativeOptions {
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

type LogVec = Arc<RwLock<Vec<(String, Color32, DateTime<Local>)>>>;

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
    ping: u16,
}

#[derive(Default, PartialEq, Eq)]
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
        let (address, phrase, chan_id_text) = if let Ok(mut file) = File::open(".voudp") {
            let mut data = String::new();
            file.read_to_string(&mut data).ok();

            if !data.is_empty() {
                let split = data.split_whitespace().collect::<Vec<&str>>();

                if split.len() >= 3 {
                    (split[0].into(), split[1].into(), split[2].into())
                } else {
                    (
                        "127.0.0.1:37549".to_string(),
                        "".to_string(),
                        "1".to_string(),
                    )
                }
            } else {
                (
                    "127.0.0.1:37549".to_string(),
                    "".to_string(),
                    "1".to_string(),
                )
            }
        } else {
            (
                "127.0.0.1:37549".to_string(),
                "".to_string(),
                "1".to_string(),
            )
        };

        Self {
            address,
            current_channel_id: 0,
            global_list: GlobalListState {
                channels: vec![],
                last_updated: Instant::now(),
                current_channel: 0,
            },
            command_list: vec![],
            socket: None,
            chan_id_text,
            phrase,
            is_connected: false,
            muted: false,
            deafened: false,
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
            ping: u16::MAX,
        }
    }
}
impl eframe::App for GuiClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.error.show {
            ShowMode::ShowError => {
                egui::Window::new("Connection Error")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
                    .frame(
                        egui::Frame::none()
                            .fill(ctx.style().visuals.window_fill())
                            .stroke(ctx.style().visuals.window_stroke())
                            .rounding(12.0)
                            .inner_margin(egui::Margin::symmetric(18.0, 16.0)),
                    )
                    .show(ctx, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading(
                                egui::RichText::new("Connection Error")
                                    .color(egui::Color32::LIGHT_RED),
                            );
                        });

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        ui.label(
                            egui::RichText::new(&self.error.message)
                                .size(14.0)
                                .color(egui::Color32::RED),
                        );

                        ui.add_space(14.0);

                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::Center),
                            |ui| {
                                let back = ui.add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Button::new(egui::RichText::new("Go back").strong()),
                                );

                                if back.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    self.error.show = ShowMode::DontShow;
                                }
                            },
                        );
                    });
            }

            ShowMode::ShowMaskScreen => {
                egui::Window::new("Nickname Required")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
                    .frame(
                        egui::Frame::none()
                            .fill(ctx.style().visuals.window_fill())
                            .stroke(ctx.style().visuals.window_stroke())
                            .rounding(12.0)
                            .inner_margin(egui::Margin::symmetric(18.0, 16.0)),
                    )
                    .show(ctx, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading(
                                egui::RichText::new("Choose a nickname")
                                    .color(egui::Color32::YELLOW),
                            );
                        });

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(12.0);

                        ui.label(
                            egui::RichText::new("🔌 Enter nickname").color(egui::Color32::GRAY),
                        );

                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.nick)
                                .hint_text("Nickname")
                                .desired_width(ui.available_width()),
                        );

                        ui.memory_mut(|mem| mem.request_focus(edit.id));

                        let enter_pressed =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                        ui.add_space(16.0);

                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::Center),
                            |ui| {
                                let use_nick = ui.add_enabled(
                                    !self.nick.is_empty(),
                                    egui::Button::new(
                                        egui::RichText::new("Use nickname")
                                            .strong()
                                            .color(egui::Color32::BLACK),
                                    )
                                    .fill(egui::Color32::LIGHT_GREEN)
                                    .min_size(egui::vec2(ui.available_width(), 34.0)),
                                );

                                if (use_nick.clicked() || enter_pressed) && !self.nick.is_empty() {
                                    self.error.show = ShowMode::DontShow;
                                    self.nicked = true;
                                    self.set_nick();
                                    self.send_message();
                                }
                            },
                        );

                        ui.add_space(8.0);

                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::Center),
                            |ui| {
                                let skip = ui.add_sized(
                                    [ui.available_width(), 28.0],
                                    egui::Button::new("Continue without nickname")
                                        .fill(egui::Color32::from_gray(60)),
                                );

                                if skip.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    self.error.show = ShowMode::DontShow;
                                    self.nick.clear();
                                    self.input.clear();
                                }
                            },
                        );
                    });
            }

            ShowMode::DontShow => {}
        }

        if !self.is_connected {
            egui::CentralPanel::default().show(ctx, |ui| {
                let available = ui.available_size();
                ui.vertical_centered(|ui| {
                    ui.add_space(available.y * 0.15); // top padding

                    // ===== Main card =====
                    egui::Frame::none()
                        .fill(Color32::from_rgb(40, 45, 50)) // dark card
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(60))) // subtle border
                        .rounding(10.0)
                        .inner_margin(egui::Margin::symmetric(20.0, 20.0))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.heading(RichText::new("VoUDP GUI Client").size(24.0).strong());
                                ui.add_space(15.0);

                                // ----- Server Address -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔌").size(18.0));
                                    ui.add_space(4.0);

                                    let text_edit = egui::TextEdit::singleline(&mut self.address)
                                        .hint_text("server address (ip:port)")
                                        .desired_width(220.0)
                                        .frame(false); // disable default ugly frame

                                    egui::Frame::none()
                                        .fill(Color32::from_gray(30))
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });

                                ui.add_space(8.0);

                                // ----- Server Password -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔑").size(18.0));
                                    ui.add_space(4.0);

                                    let text_edit = egui::TextEdit::singleline(&mut self.phrase)
                                        .hint_text("usually 'voudp'")
                                        .password(true)
                                        .desired_width(220.0)
                                        .frame(false);

                                    egui::Frame::none()
                                        .fill(Color32::from_gray(30))
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });

                                ui.add_space(8.0);

                                // ----- Channel ID -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔗").size(18.0));
                                    ui.add_space(4.0);

                                    let text_edit =
                                        egui::TextEdit::singleline(&mut self.chan_id_text)
                                            .hint_text("ID")
                                            .char_limit(2)
                                            .desired_width(60.0)
                                            .frame(false);

                                    egui::Frame::none()
                                        .fill(Color32::from_gray(30))
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });

                                ui.add_space(15.0);

                                // ----- Connect Button -----
                                let connect_size = [150.0, 32.0];
                                let connect_color = Color32::from_rgb(60, 120, 240); // clean blue
                                if ui
                                    .add_sized(
                                        connect_size,
                                        egui::Button::new(
                                            RichText::new("Connect").strong().color(Color32::WHITE),
                                        )
                                        .fill(connect_color)
                                        .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                                        .rounding(6.0),
                                    )
                                    .clicked()
                                {
                                    // ----- Connection logic -----
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
                                            self.socket = Some(state.socket.clone());
                                            let arc_state = Arc::new(Mutex::new(state));
                                            let thread_state = arc_state.clone();
                                            let handle = std::thread::spawn(move || {
                                                let _ = thread_state
                                                    .lock()
                                                    .unwrap()
                                                    .run(client::Mode::Gui);
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

                                    self.request_global_list();

                                    let file = match File::create_new(".voudp") {
                                        Ok(file) => Some(file),
                                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                                            File::options()
                                                .write(true)
                                                .truncate(true)
                                                .open(".voudp")
                                                .ok()
                                        }
                                        Err(_) => None,
                                    };

                                    if let Some(mut file) = file {
                                        let _ = writeln!(
                                            file,
                                            "{} {} {}",
                                            self.address, self.phrase, self.chan_id_text
                                        );

                                        let _ = file.flush();
                                    }
                                }
                            });
                        });

                    ui.add_space(available.y * 0.15); // bottom padding
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
                .default_width(280.0)
                .min_width(220.0)
                .max_width(420.0)
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;

                    // ===== Header =====
                    ui.heading("Channels");
                    ui.add_space(4.0);

                    let total_users = self
                        .global_list
                        .channels
                        .iter()
                        .map(|c| c.unmasked_count as usize + c.masked_users.len())
                        .sum::<usize>();
                    let total_channels = self.global_list.channels.len();

                    // ===== Stats =====
                    egui::Frame::group(ui.style())
                        .rounding(8.0)
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("Users").small().color(Color32::GRAY));
                                    ui.label(
                                        RichText::new(total_users.to_string()).strong().size(16.0),
                                    );
                                });
                                ui.separator();
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new("Channels").small().color(Color32::GRAY),
                                    );
                                    ui.label(
                                        RichText::new(total_channels.to_string())
                                            .strong()
                                            .size(16.0),
                                    );
                                });
                            });
                        });

                    ui.add_space(6.0);

                    // ===== Scrollable channel list =====
                    let footer_height = 48.0; // just enough for emojis
                    let max_scroll_height = (ui.available_height() - footer_height).max(0.0);

                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .max_height(max_scroll_height)
                        .show(ui, |ui| {
                            if self.global_list.channels.is_empty() {
                                ui.add_space(20.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new("No active channels")
                                            .italics()
                                            .color(Color32::GRAY),
                                    );
                                });
                                ui.add_space(20.0);
                            }

                            for channel in &self.global_list.channels {
                                let is_current = channel.channel_id == self.current_channel_id;
                                let total_in_channel =
                                    channel.unmasked_count as usize + channel.masked_users.len();
                                let bg = if is_current {
                                    Color32::from_rgb(30, 45, 35)
                                } else {
                                    ui.style().visuals.extreme_bg_color
                                };

                                let response = egui::Frame::none()
                                    .fill(bg)
                                    .rounding(10.0)
                                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                    .show(ui, |ui| {
                                        // ----- Header -----
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(format!("#{}", channel.name))
                                                    .strong()
                                                    .size(15.0)
                                                    .monospace()
                                                    .color(if is_current {
                                                        Color32::LIGHT_GREEN
                                                    } else {
                                                        Color32::WHITE
                                                    }),
                                            );

                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    badge(
                                                        ui,
                                                        format!("{total_in_channel} users"),
                                                        Color32::GRAY,
                                                    );
                                                    if channel.unmasked_count > 0 {
                                                        badge(
                                                            ui,
                                                            format!(
                                                                "{} unmasked",
                                                                channel.unmasked_count
                                                            ),
                                                            Color32::YELLOW,
                                                        );
                                                    }
                                                },
                                            );
                                        });

                                        ui.add_space(4.0);
                                        ui.separator();
                                        ui.add_space(4.0);

                                        // ----- Users -----
                                        if channel.masked_users.is_empty() {
                                            ui.label(
                                                RichText::new("No masked users")
                                                    .small()
                                                    .color(Color32::GRAY),
                                            );
                                        } else {
                                            for (name, muted, deafened) in &channel.masked_users {
                                                ui.horizontal(|ui| {
                                                    let status_color = match (*muted, *deafened) {
                                                        (true, true) => Color32::RED,
                                                        (true, false) => {
                                                            Color32::from_rgb(100, 150, 255)
                                                        }
                                                        (false, true) => Color32::YELLOW,
                                                        (false, false) => Color32::GREEN,
                                                    };
                                                    ui.label(
                                                        RichText::new("•")
                                                            .size(15.0)
                                                            .color(status_color),
                                                    );
                                                    ui.label(
                                                        RichText::new(name)
                                                            .strong()
                                                            .color(Color32::GRAY),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            if *deafened {
                                                                badge(
                                                                    ui,
                                                                    "deafened",
                                                                    Color32::YELLOW,
                                                                );
                                                            }
                                                            if *muted {
                                                                badge(
                                                                    ui,
                                                                    "muted",
                                                                    Color32::from_rgb(
                                                                        120, 160, 255,
                                                                    ),
                                                                );
                                                            }
                                                        },
                                                    );
                                                });
                                            }
                                        }
                                    })
                                    .response;

                                // Make the entire card clickable
                                if !is_current && response.clicked() {
                                    self.join_channel(channel.channel_id);
                                }

                                // Context menu
                                response.context_menu(|ui| {
                                    if !is_current && ui.button("Join channel").clicked() {
                                        self.join_channel(channel.channel_id);
                                        ui.close_menu();
                                    }
                                    if ui.button("Copy channel name").clicked() {
                                        ui.output_mut(|o| o.copied_text = channel.name.clone());
                                        ui.close_menu();
                                    }
                                });

                                ui.add_space(4.0);
                            }
                        });

                    // ===== Footer (Ping + text buttons) =====
                    ui.add_space(2.0);
                    ui.separator();
                    ui.add_space(2.0);

                    ui.horizontal(|ui| {
                        // ----- Ping -----
                        if self.ping != u16::MAX {
                            let color = match self.ping {
                                p if p < 125 => Color32::LIGHT_GREEN,
                                p if p < 250 => Color32::YELLOW,
                                _ => Color32::RED,
                            };
                            ui.label(RichText::new("📡").size(18.0).color(color));
                            ui.label(RichText::new("Ping: ").size(14.0).color(Color32::WHITE));
                            ui.label(
                                RichText::new(format!("{} ms", self.ping))
                                    .size(14.0)
                                    .color(color),
                            );
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn_size = [60.0, 25.0]; // slightly smaller buttons

                            // Deafen button
                            let deaf_color = if self.deafened {
                                Color32::from_rgb(60, 120, 240)
                            } else {
                                ui.visuals().widgets.inactive.bg_fill
                            };
                            if ui
                                .add_sized(
                                    btn_size,
                                    egui::Button::new(RichText::new("Deafen").strong())
                                        .fill(deaf_color)
                                        .rounding(6.0),
                                )
                                .clicked()
                            {
                                self.deafened = !self.deafened;
                                if let Some(client) = &self.client {
                                    client.lock().unwrap().set_deafened(self.deafened);
                                }
                                if self.deafened {
                                    self.write_log("[Speaker] deafened".into(), Color32::RED);
                                } else {
                                    self.write_log(
                                        "[Speaker] undeafened".into(),
                                        Color32::LIGHT_GREEN,
                                    );
                                }
                            }

                            ui.add_space(2.0); // small gap between buttons

                            // Mute button
                            let mute_color = if self.muted {
                                Color32::from_rgb(60, 120, 240)
                            } else {
                                ui.visuals().widgets.inactive.bg_fill
                            };
                            if ui
                                .add_sized(
                                    btn_size,
                                    egui::Button::new(RichText::new("Mute").strong())
                                        .fill(mute_color)
                                        .rounding(6.0),
                                )
                                .clicked()
                            {
                                self.muted = !self.muted;
                                if let Some(client) = &self.client {
                                    client.lock().unwrap().set_muted(self.muted);
                                }
                                if self.muted {
                                    self.write_log("[Microphone] muted".into(), Color32::RED);
                                } else {
                                    self.write_log(
                                        "[Microphone] unmuted".into(),
                                        Color32::LIGHT_GREEN,
                                    );
                                }
                            }
                            ui.add_space(2.0);
                            self.talking_indicator(ui);
                        });
                    });
                });

            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let button_height = 32.0;
                    let button_width = 100.0; // fixed width for uniformity
                    let spacing = 6.0;

                    ui.spacing_mut().item_spacing.x = spacing;

                    // ----- Disconnect -----
                    if ui
                        .add_sized(
                            [button_width, button_height],
                            egui::Button::new(RichText::new("❌ Disconnect").strong())
                                .fill(Color32::from_rgb(180, 60, 60))
                                .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                                .rounding(6.0),
                        )
                        .clicked()
                    {
                        self.disconnect();
                        self.write_log(
                            format!(
                                "Sent EOF to {}. It is now handling our departure",
                                self.address
                            ),
                            Color32::YELLOW,
                        );
                    }

                    // ----- Renick -----
                    if ui
                        .add_sized(
                            [button_width, button_height],
                            egui::Button::new(RichText::new("Renick").strong())
                                .fill(Color32::from_rgb(80, 120, 180))
                                .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                                .rounding(6.0),
                        )
                        .clicked()
                    {
                        self.error.show = ShowMode::ShowMaskScreen;
                    }

                    // ----- Clear Logs -----
                    if ui
                        .add_sized(
                            [button_width, button_height],
                            egui::Button::new(RichText::new("Clear Logs").strong())
                                .fill(Color32::from_rgb(100, 140, 100))
                                .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                                .rounding(6.0),
                        )
                        .clicked()
                    {
                        self.logs.write().unwrap().clear();
                        self.write_log("Cleared logs".into(), Color32::LIGHT_GREEN);
                    }
                });

                ui.separator();

                let available_width = ui.available_width();
                let available_height = ui.available_height();

                ui.set_width(available_width);
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false; 2])
                    .max_width(available_width)
                    .max_height(available_height - 50.0)
                    .show(ui, |ui| {
                        // Remove default padding
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);

                        let logs = self.logs.read().unwrap();

                        for (msg, color, time) in logs.iter() {
                            let is_self = *color == Color32::LIGHT_BLUE || *color == Color32::BLUE;
                            let is_system = *color == Color32::GRAY
                                || *color == Color32::YELLOW
                                || *color == Color32::LIGHT_GREEN
                                || *color == Color32::RED;

                            if is_system {
                                if let Some((src, content)) = parse_system_message(msg) {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(4.0);

                                        ui.label(
                                            egui::RichText::new(src)
                                                .color(*color)
                                                .size(14.0)
                                                .strong()
                                                .monospace(),
                                        );

                                        ui.label(
                                            egui::RichText::new(content)
                                                .color(*color)
                                                .size(12.0)
                                                .italics(),
                                        );

                                        ui.add_space(4.0);
                                    });
                                } else {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(2.0);
                                        ui.label(
                                            egui::RichText::new(msg)
                                                .color(*color)
                                                .size(12.0)
                                                .italics()
                                                .strong(),
                                        );
                                        ui.add_space(2.0);
                                    });
                                }
                                continue;
                            }

                            // Try to parse as chat message
                            if let Some((_, name, content)) = parse_chat_message(msg) {
                                // Colors
                                let (_, text_color) = if is_self {
                                    (Color32::from_rgb(0, 120, 215), Color32::WHITE)
                                } else {
                                    (Color32::from_rgb(240, 240, 240), Color32::BLACK)
                                };

                                let channel_label = format!("{} ", name);
                                if is_self {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::TOP),
                                        |ui| {
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new(channel_label)
                                                    .color(Color32::LIGHT_YELLOW)
                                                    .size(13.0),
                                            );
                                            ui.add_space(4.0);
                                        },
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(channel_label)
                                                .color(Color32::from_rgb(150, 150, 150))
                                                .size(13.0),
                                        );
                                        ui.add_space(4.0);
                                    });
                                }

                                // Bubble with name and message
                                if is_self {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::TOP),
                                        |ui| {
                                            bubble_ui(ui, &content, time, text_color);
                                        },
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        bubble_ui(ui, &content, time, text_color);
                                    });
                                }

                                ui.add_space(2.0);
                            } else {
                                // Fallback: display raw message in bubble
                                let text_color = if is_self {
                                    Color32::WHITE
                                } else {
                                    Color32::BLACK
                                };

                                if is_self {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::TOP),
                                        |ui| {
                                            bubble_ui(ui, msg, time, text_color);
                                        },
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        bubble_ui(ui, msg, time, text_color);
                                    });
                                }

                                ui.add_space(2.0);
                            }
                        }
                    });

                egui::TopBottomPanel::bottom("input_panel")
                    .show_separator_line(true)
                    .show_inside(ui, |ui| {
                        let input_id = ui.make_persistent_id("chat_input");

                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let available_width = ui.available_width() - 80.0;
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

                            let send_button_size = [70.0, 28.0]; // slightly smaller than input

                            let send_color = { Color32::from_gray(70) };

                            if ui
                                .add_sized(
                                    send_button_size,
                                    egui::Button::new(
                                        RichText::new("Send").strong().color(Color32::WHITE),
                                    )
                                    .fill(send_color)
                                    .stroke(Stroke::new(1.0, Color32::BLACK))
                                    .rounding(6.0),
                                )
                                .clicked()
                            {
                                if self.input.starts_with('/') {
                                    self.execute_command();
                                } else {
                                    self.send_message();
                                }
                            }

                            // if self.error.show.ne(&ShowMode::DontShow) {
                            //     return;
                            // }

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
                    Message::Renick(old, new) => {
                        self.logs.write().unwrap().push((
                            format!("{old} is now known as {new}"),
                            Color32::YELLOW,
                            time,
                        ));
                    }
                    Message::ChatMessage(name, content, is_self) => {
                        let channel = {
                            let id = self.current_channel_id;

                            self.global_list
                                .channels
                                .iter()
                                .filter(|channel| channel.channel_id == id)
                                .next_back()
                                .map(|info| info.name.clone())
                                .unwrap_or(String::from("unknown"))
                        };

                        self.logs.write().unwrap().push((
                            format!("[#{channel}] {name}: {content}"),
                            if is_self {
                                Color32::LIGHT_BLUE
                            } else {
                                Color32::WHITE
                            },
                            time,
                        ));
                    }
                    Message::Broadcast(src, content) => {
                        self.logs.write().unwrap().push((
                            format!("[{src}] {content}"),
                            Color32::LIGHT_GREEN,
                            time,
                        ));
                    }
                    Message::Kick(msg) => {
                        drop(client);
                        self.disconnect();

                        self.error.message = msg;
                        self.error.show = ShowMode::ShowError;
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
    fn disconnect(&mut self) {
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
    }
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
        self.request_global_list();
    }

    fn update_global_list(&mut self) {
        if let Some(client) = &self.client {
            let client = client.lock().unwrap();
            let list_state = client.list.lock().unwrap();
            let ping = client.ping.load(Ordering::Relaxed);

            self.global_list.channels = list_state.channels.clone();
            self.global_list.last_updated = Instant::now();
            self.global_list.current_channel = list_state.current_channel;
            self.current_channel_id = list_state.current_channel;
            self.ping = ping;
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
