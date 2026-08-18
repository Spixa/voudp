mod bubble;

use anyhow::Result;
use chrono::{DateTime, Local};
use core::f32;
use eframe::{NativeOptions, egui};
use egui::{Color32, Id, RichText};
use rand::Rng;

use std::{
    fs::File,
    io::{self, Write},
    sync::{Arc, Mutex, RwLock, atomic::Ordering, mpsc::TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use voudp::{
    client::{self, ClientState, GlobalListState, Message},
    protocol::ClientPacketType,
    socket::SecureUdpSocket,
    util::{CommandResult, ServerCommand},
};

use crate::bubble::{
    badge, bubble_ui, connection_activity_wifi, parse_chat_message, parse_system_message,
};

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
    phrase: String,
    is_connected: bool,
    muted: bool,
    deafened: bool,
    client: Option<Arc<Mutex<ClientState>>>,
    client_thread: Option<JoinHandle<()>>,
    error: ErrorWindow,
    input: String,
    username: String,
    password: String,
    info_text: String,
    logs: LogVec,
    show_command_suggestions: bool,
    selected_suggestion: usize,
    filter_text: String,
    ping: u16,
    confetti: Vec<crate::bubble::Particle>,
    spawn_confetti: bool,
    confetti_start: Instant,
}

#[derive(Default, PartialEq, Eq)]
enum ShowMode {
    #[default]
    DontShow,
    ShowError,
}

enum CommandAction {
    UseCommand(String),
}

#[derive(Default)]
struct ErrorWindow {
    show: ShowMode,
    message: String,
}

impl Default for GuiClientApp {
    fn default() -> Self {
        use std::fs;

        const DEFAULT_ADDR: &str = "127.0.0.1:37549";

        let (address, phrase, username) = if let Ok(data) = fs::read_to_string(".voudp") {
            let tokens: Vec<&str> = data.split_whitespace().collect();

            if tokens.len() >= 3 {
                (
                    tokens[0].to_string(),
                    tokens[1].to_string(),
                    tokens.get(3).map_or_else(String::new, |&s| s.to_string()),
                )
            } else {
                (DEFAULT_ADDR.to_string(), String::new(), String::new())
            }
        } else {
            (DEFAULT_ADDR.to_string(), String::new(), String::new())
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
            phrase,
            is_connected: false,
            muted: false,
            deafened: false,
            client: None,
            client_thread: None,
            error: Default::default(),
            logs: Default::default(),
            input: Default::default(),
            username,
            password: Default::default(),
            info_text: String::from("To register, right click the Connect button"),
            show_command_suggestions: false,
            selected_suggestion: 0,
            filter_text: String::new(),
            ping: u16::MAX,
            confetti: vec![],
            spawn_confetti: false,
            confetti_start: Instant::now(),
        }
    }
}
impl eframe::App for GuiClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        catppuccin_egui::set_theme(ctx, catppuccin_egui::MACCHIATO);

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
                                    {}
                                    self.error.show = ShowMode::DontShow;
                                }
                            },
                        );
                    });
            }

            // ShowMode::ShowMaskScreen => {
            //     egui::Window::new("Nickname Required")
            //         .collapsible(false)
            //         .resizable(false)
            //         .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            //         .frame(
            //             egui::Frame::none()
            //                 .fill(ctx.style().visuals.window_fill())
            //                 .stroke(ctx.style().visuals.window_stroke())
            //                 .rounding(12.0)
            //                 .inner_margin(egui::Margin::symmetric(18.0, 16.0)),
            //         )
            //         .show(ctx, |ui| {
            //             ui.vertical_centered(|ui| {
            //                 ui.heading(
            //                     egui::RichText::new("Choose a nickname")
            //                         .color(egui::Color32::YELLOW),
            //                 );
            //             });

            //             ui.add_space(10.0);
            //             ui.separator();
            //             ui.add_space(12.0);

            //             ui.label(
            //                 egui::RichText::new("🔌 Enter nickname").color(egui::Color32::GRAY),
            //             );

            //             let edit = ui.add(
            //                 egui::TextEdit::singleline(&mut self.username)
            //                     .hint_text("Nickname")
            //                     .desired_width(ui.available_width()),
            //             );

            //             ui.memory_mut(|mem| mem.request_focus(edit.id));

            //             let enter_pressed =
            //                 edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            //             ui.add_space(16.0);

            //             ui.with_layout(
            //                 egui::Layout::top_down_justified(egui::Align::Center),
            //                 |ui| {
            //                     let use_nick = ui.add_enabled(
            //                         !self.nick.is_empty(),
            //                         egui::Button::new(
            //                             egui::RichText::new("Use nickname")
            //                                 .strong()
            //                                 .color(egui::Color32::BLACK),
            //                         )
            //                         .fill(egui::Color32::LIGHT_GREEN)
            //                         .min_size(egui::vec2(ui.available_width(), 34.0)),
            //                     );

            //                     if (use_nick.clicked() || enter_pressed) && !self.nick.is_empty() {
            //                         self.error.show = ShowMode::DontShow;
            //                         self.nicked = true;
            //                         self.set_nick();

            //                         if self.input.starts_with("/") {
            //                             self.execute_command();
            //                         } else {
            //                             self.send_message();
            //                         }
            //                     }
            //                 },
            //             );

            //             ui.add_space(8.0);

            //             ui.with_layout(
            //                 egui::Layout::top_down_justified(egui::Align::Center),
            //                 |ui| {
            //                     let skip = ui.add_sized(
            //                         [ui.available_width(), 28.0],
            //                         egui::Button::new("Continue without nickname")
            //                             .fill(egui::Color32::from_gray(60)),
            //                     );

            //                     if skip.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
            //                     {
            //                         self.error.show = ShowMode::DontShow;
            //                         self.nick.clear();
            //                         self.input.clear();
            //                     }
            //                 },
            //             );
            //         });
            // }
            ShowMode::DontShow => {}
        }

        if !self.is_connected {
            {
                let mut delete = false;
                if let Some(client) = &self.client {
                    let c = client.lock().unwrap();

                    match c.register_state {
                        client::RegisterState::RegisterError => {
                            self.info_text = format!(
                                "Could not register {}! Either the username is taken, or registration is closed.",
                                self.username
                            );
                            self.error.message = "Failed to register, maybe username is taken, or registration is closed.".into();
                            delete = true;
                        }
                        client::RegisterState::RegisterSuccess => {
                            self.info_text = format!("Successfully registered {}!", self.username);
                            delete = true;
                        }
                        client::RegisterState::HasntBegun => {
                            self.info_text = "To register, right click the Connect button 1".into();
                        }
                        client::RegisterState::Registering => {
                            self.info_text = "Handshake in process..".into();
                        }
                        client::RegisterState::TimedOut => {
                            self.info_text = format!(
                                "Handshake with {} timed out. Maybe server is unreachable, or PSK is incorrect",
                                self.address
                            );
                        }
                    }
                }

                if delete {
                    self.client = None;
                    self.client_thread = None;
                }
            }

            egui::CentralPanel::default().show(ctx, |ui| {
                let available = ui.available_size();
                ui.vertical_centered(|ui| {
                    ui.add_space(available.y * 0.15); // top padding

                    // ===== Main card =====
                    egui::Frame::none()
                        .fill(ctx.style().visuals.extreme_bg_color) // dark card
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(60))) // subtle border
                        .rounding(10.0)
                        .inner_margin(egui::Margin::symmetric(20.0, 20.0))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.heading(RichText::new("VoUDP").size(24.0).strong());
                                ui.add_space(15.0);

                                // ----- Server Address -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔌").size(18.0));
                                    ui.add_space(4.0);
                                    let text_edit = egui::TextEdit::singleline(&mut self.address)
                                        .hint_text("server address (ip:port)")
                                        .desired_width(220.0)
                                        .frame(false);
                                    egui::Frame::none()
                                        .fill(ctx.style().visuals.code_bg_color)
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });
                                ui.add_space(8.0);

                                // ----- Server Password (phrase) -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔑").size(18.0));
                                    ui.add_space(4.0);
                                    let text_edit = egui::TextEdit::singleline(&mut self.phrase)
                                        .hint_text("pre-shared key")
                                        .password(true)
                                        .desired_width(220.0)
                                        .frame(false);
                                    egui::Frame::none()
                                        .fill(ctx.style().visuals.code_bg_color)
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });
                                ui.add_space(12.0);

                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Authentication (v0.5+)")
                                        .size(14.0)
                                        .weak()
                                        .color(Color32::from_gray(180)),
                                );
                                ui.add_space(6.0);

                                // ----- Username -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("👤").size(18.0));
                                    ui.add_space(4.0);
                                    let text_edit = egui::TextEdit::singleline(&mut self.username)
                                        .hint_text("username")
                                        .desired_width(220.0)
                                        .frame(false);
                                    egui::Frame::none()
                                        .fill(ctx.style().visuals.code_bg_color)
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });
                                ui.add_space(8.0);

                                // ----- Password -----
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("🔒").size(18.0));
                                    ui.add_space(4.0);
                                    let text_edit = egui::TextEdit::singleline(&mut self.password)
                                        .hint_text("password")
                                        .password(true)
                                        .desired_width(220.0)
                                        .frame(false);
                                    egui::Frame::none()
                                        .fill(ctx.style().visuals.code_bg_color)
                                        .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.add(text_edit);
                                        });
                                });
                                ui.add_space(15.0);

                                ui.add_space(4.0);
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Min),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(self.info_text.clone())
                                                    .size(12.0)
                                                    .weak()
                                                    .color(Color32::LIGHT_GREEN),
                                            )
                                            .wrap(false), // prevents wrapping and keeps height minimal
                                        );
                                    },
                                );
                                ui.add_space(6.0);

                                // ----- Connect Button -----
                                let connect_size = [150.0, 32.0];
                                let connect_color = Color32::from_rgb(60, 120, 240); // clean blue

                                let btn = ui.add_sized(
                                    connect_size,
                                    egui::Button::new(
                                        RichText::new("Connect").strong().color(Color32::WHITE),
                                    )
                                    .fill(connect_color)
                                    .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                                    .rounding(6.0),
                                );
                                btn.context_menu(|ui| {
                                    if ui.button("Register instead").clicked()
                                        && !self.username.is_empty()
                                    {
                                        match ClientState::new(
                                            &self.address,
                                            &self.phrase.clone().into_bytes(),
                                            self.username.clone(),
                                            self.password.clone(),
                                        ) {
                                            Ok(state) => {
                                                self.socket = Some(state.socket.clone());
                                                let arc_state = Arc::new(Mutex::new(state));
                                                let thread_state = arc_state.clone();
                                                let username = self.username.clone();
                                                let password = self.password.clone();

                                                let handle = std::thread::spawn(move || {
                                                    thread_state
                                                        .lock()
                                                        .unwrap()
                                                        .register(&username, &password);
                                                });

                                                self.client_thread = Some(handle);
                                                self.client = Some(arc_state);
                                            }
                                            Err(e) => {
                                                self.error.show = ShowMode::ShowError;
                                                self.error.message = format!(
                                                    "Failed to connect to the server: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                });

                                if btn.clicked() {
                                    // Try to connect
                                    match ClientState::new(
                                        &self.address,
                                        &self.phrase.clone().into_bytes(),
                                        self.username.clone(),
                                        self.password.clone(),
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

                                    // save credentials to .voudp (now with all 4 fields)
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
                                        // Write address, phrase, username, and password
                                        let _ = writeln!(
                                            file,
                                            "{} {} {}",
                                            self.address, self.phrase, self.username
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
                    let footer_height = 64.0; // just enough for emojis
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

                    ui.horizontal(|ui| {
                        if !self.muted {
                            connection_activity_wifi(ui, 18.0, Color32::LIGHT_GREEN);

                            let idevice_name = if let Some(client) = &self.client {
                                client.lock().unwrap().devices.lock().unwrap().input.clone()
                            } else {
                                String::new()
                            };

                            ui.label(format!("Streaming audio from {idevice_name}..."));
                        } else if !self.deafened {
                            ui.label("Audio stream paused");
                        } else {
                            ui.label(RichText::new("⚡").color(Color32::YELLOW).size(14.0));
                            ui.label("Low bandwidth mode");
                        }
                    });

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
                if !self.confetti.is_empty() {
                    crate::bubble::update_confetti(ui, &mut self.confetti);
                }

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

                    // ----- Renick ----- [REMOVE]
                    // if ui
                    //     .add_sized(
                    //         [button_width, button_height],
                    //         egui::Button::new(RichText::new("Renick").strong())
                    //             .fill(Color32::from_rgb(80, 120, 180))
                    //             .stroke(egui::Stroke::new(1.0, Color32::BLACK))
                    //             .rounding(6.0),
                    //     )
                    //     .clicked()
                    // {
                    //     self.error.show = ShowMode::ShowMaskScreen;
                    // }

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
                                || *color == Color32::LIGHT_RED
                                || *color == Color32::RED
                                || *color == Color32::from_rgb(255, 128, 255);

                            if is_system {
                                ui.vertical_centered(|ui| {
                                    let frame = egui::Frame::default()
                                        .fill(ui.ctx().style().visuals.extreme_bg_color) // dark
                                        .rounding(ui.ctx().style().visuals.window_rounding)
                                        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                                        .outer_margin(egui::Margin::symmetric(0.0, 4.0));

                                    frame.show(ui, |ui| {
                                        ui.style_mut().wrap = Some(true);
                                        ui.spacing_mut().item_spacing.y = 2.0;

                                        if let Some((src, content)) = parse_system_message(msg) {
                                            ui.horizontal(|ui| {
                                                let title_badge = egui::Frame::default()
                                                    .fill(ui.ctx().style().visuals.code_bg_color)
                                                    .rounding(
                                                        ui.ctx().style().visuals.menu_rounding,
                                                    )
                                                    .inner_margin(egui::Margin::symmetric(
                                                        8.0, 2.0,
                                                    ));

                                                title_badge.show(ui, |ui| {
                                                    ui.add_space(4.0);
                                                    ui.label(
                                                        egui::RichText::new(src)
                                                            .color(*color)
                                                            .size(13.0)
                                                            .strong(),
                                                    );
                                                });
                                            });
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new(content)
                                                    .color(*color)
                                                    .size(12.0),
                                            );
                                        } else {
                                            // Fallback: single line system message
                                            ui.label(
                                                egui::RichText::new(msg)
                                                    .color(*color)
                                                    .size(12.0)
                                                    .italics()
                                                    .strong(),
                                            );
                                        }
                                        ui.style_mut().wrap = None;
                                    });
                                });
                                continue;
                            }

                            if let Some((_, name, content)) = parse_chat_message(msg) {
                                // Colors
                                let (_, text_color) = if is_self {
                                    (Color32::from_rgb(0, 120, 215), Color32::WHITE)
                                } else {
                                    (Color32::from_rgb(240, 240, 240), Color32::BLACK)
                                };

                                let time_str = time.format("%H:%M").to_string();
                                let full_time = time.format("%Y-%m-%d %H:%M:%S").to_string();

                                if is_self {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::TOP),
                                        |ui| {
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!("{} ", name))
                                                        .color(Color32::LIGHT_YELLOW)
                                                        .size(13.0),
                                                );
                                                ui.add_space(5.0);
                                                ui.label(
                                                    egui::RichText::new(&time_str)
                                                        .color(Color32::from_rgb(180, 180, 180))
                                                        .size(11.0),
                                                )
                                                .on_hover_text(full_time);
                                            });
                                            ui.add_space(4.0);
                                        },
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(&time_str)
                                                .color(Color32::from_rgb(150, 150, 150))
                                                .size(11.0),
                                        )
                                        .on_hover_text(full_time);

                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(format!("{} ", name))
                                                .color(Color32::WHITE)
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
                                            bubble_ui(
                                                ui,
                                                &content,
                                                text_color,
                                                Some(&name),
                                                &mut self.input,
                                            );
                                        },
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        bubble_ui(
                                            ui,
                                            &content,
                                            text_color,
                                            Some(&name),
                                            &mut self.input,
                                        );
                                    });
                                }

                                ui.add_space(2.0);
                            } else {
                                // received invalid message
                            }
                        }
                    });

                egui::TopBottomPanel::bottom("input_panel")
                    .show_separator_line(true)
                    .show_inside(ui, |ui| {
                        ui.add_space(2.0);

                        // Assign a persistent ID so we can reliably check focus
                        let input_id = ui.make_persistent_id("chat_input");
                        let is_focused = ui.memory(|mem| mem.has_focus(input_id));

                        let mut send_triggered = false;

                        // --- Intercept Enter key BEFORE the TextEdit sees it ---
                        if is_focused {
                            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let shift = ui.input(|i| i.modifiers.shift);
                            if enter && !shift {
                                send_triggered = true;
                                // Consume the key so TextEdit doesn't add a newline
                                ui.input_mut(|i| {
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                                });
                            }
                        }

                        ui.horizontal(|ui| {
                            let available_width = ui.available_width() - 80.0;

                            // --- Multiline text edit with persistent ID ---
                            let text_edit = egui::TextEdit::singleline(&mut self.input)
                                .id(input_id) // Important: links focus check above
                                .hint_text("type your message/command...")
                                .text_color(Color32::from_rgb(255, 215, 0))
                                .desired_rows(2) // Set initial height; grows with content
                                .desired_width(available_width);
                            let mut response = None;
                            ui.vertical(|ui| {
                                response = Some(ui.add(text_edit));
                            });
                            let response = response.unwrap();

                            // Store response for command suggestions if needed
                            ui.memory_mut(|mem| mem.data.insert_temp(input_id, response.clone()));

                            // --- Command suggestion handling (unchanged) ---
                            if self.show_command_suggestions && !self.command_list.is_empty() {
                                let handled = self.handle_command_nav(ui.ctx(), response.id);
                                if !handled {
                                    self.show_command_suggestions_ui(ui, input_id);
                                }
                            }

                            // --- Send button ---
                            let send_button_size = [70.0, 28.0];
                            let send_color = ctx.style().visuals.code_bg_color;

                            if ui
                                .add_sized(
                                    send_button_size,
                                    egui::Button::new(
                                        RichText::new("Send").strong().color(Color32::WHITE),
                                    )
                                    .fill(send_color)
                                    .stroke(ctx.style().visuals.window_stroke)
                                    .rounding(6.0),
                                )
                                .clicked()
                                || send_triggered
                            // Also send if we intercepted Enter
                            {
                                if self.input.starts_with('/') {
                                    self.execute_command();
                                } else {
                                    self.send_message();
                                }
                                // After sending, refocus the input field
                                ui.memory_mut(|mem| mem.request_focus(input_id));
                            }

                            // --- Tab completion (unchanged) ---
                            if is_focused && ui.input(|i| i.key_pressed(egui::Key::Tab)) {
                                self.tab_complete();
                                ui.memory_mut(|mem| mem.request_focus(input_id));
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

            if client.glitter.load(Ordering::Relaxed) {
                client.glitter.store(false, Ordering::Relaxed);
                self.confetti_start = Instant::now();
                self.spawn_confetti = true;
            }

            if self.spawn_confetti
                && Instant::now().duration_since(self.confetti_start) <= Duration::from_secs(15)
            {
                self.spawn_confetti(ctx);
            } else {
                self.spawn_confetti = false;
            }

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
                                .rfind(|channel| channel.channel_id == id)
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
                    Message::Command(result) => {
                        type Cr = CommandResult;
                        match result {
                            Cr::Success(content) => {
                                self.logs.write().unwrap().push((
                                    format!("[Command Success] {content}"),
                                    Color32::LIGHT_GREEN,
                                    time,
                                ));
                            }
                            Cr::Error(content) => {
                                self.logs.write().unwrap().push((
                                    format!("[Command Fail] {content}"),
                                    Color32::LIGHT_RED,
                                    time,
                                ));
                            }
                            Cr::Silent => {}
                        }
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
    fn spawn_confetti(&mut self, ctx: &egui::Context) {
        use crate::bubble::Particle;
        let screen_rect = ctx.screen_rect();

        let mut rng = rand::rng();
        for _ in 0..4 {
            self.confetti.push(Particle {
                pos: egui::pos2(
                    rng.random_range(screen_rect.min.x..screen_rect.max.x),
                    screen_rect.min.y - 10.0,
                ),
                vel: egui::vec2(
                    rng.random_range(-50.0..50.0),
                    rng.random_range(100.0..300.0),
                ),
                color: egui::Color32::from_rgb(rng.random(), rng.random(), rng.random()),
                rotation: rng.random_range(0.0..std::f32::consts::TAU),
                angular_vel: rng.random_range(-5.0..5.0),
            });
        }
    }

    fn disconnect(&mut self) {
        if let Some(client) = &self.client {
            client.lock().unwrap().disconnect();
        }

        if let Some(handle) = self.client_thread.take() {
            handle.join().ok();
        }
        self.is_connected = false;
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
            let packet = vec![ClientPacketType::List as u8]; // Request global list
            client.lock().unwrap().send(&packet);
        }
    }

    fn request_command_list(&self) {
        if let Some(client) = &self.client {
            let packet = vec![ClientPacketType::SyncCommands as u8]; // Request global list
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

        let filter_text = self.filter_text.clone();

        // get filtered commands
        let mut filtered_commands: Vec<&ServerCommand> = self
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

        // sort: exact matches first, then by length
        filtered_commands.sort_by(|a, b| {
            let a_exact = a.name[1..].to_lowercase() == filter_text.to_lowercase();
            let b_exact = b.name[1..].to_lowercase() == filter_text.to_lowercase();

            match (a_exact, b_exact) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.len().cmp(&b.name.len()),
            }
        });

        let filtered_count = filtered_commands.len();
        if filtered_count == 0 {
            return false;
        }

        let mut handled = false;

        if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.selected_suggestion = (self.selected_suggestion + 1) % filtered_count;
            handled = true;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            if self.selected_suggestion == 0 {
                self.selected_suggestion = filtered_count - 1;
            } else {
                self.selected_suggestion -= 1;
            }
            handled = true;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            let exact_match = filtered_commands
                .iter()
                .find(|cmd| cmd.name[1..].to_lowercase() == filter_text.to_lowercase());

            let command = exact_match.or_else(|| filtered_commands.get(self.selected_suggestion));

            if let Some(command) = command {
                let _requires_auth_warning = command.requires_auth;

                let should_execute = self.input.trim() == command.name
                    || (self.input.len() > command.name.len()
                        && self.input.starts_with(&command.name)
                        && self.input.chars().nth(command.name.len()) == Some(' '));

                if should_execute {
                    self.execute_command();
                } else {
                    self.input = format!("{} ", command.name);
                }

                self.show_command_suggestions = false;
                ctx.memory_mut(|mem| mem.request_focus(input_id));
            }

            handled = true;
        }

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
        let input_rect = input_response
            .map(|r| r.rect)
            .unwrap_or_else(|| ui.min_rect());

        let max_visible = 8;
        let visible_count = filtered_commands.len().min(max_visible);
        let suggestion_height = (visible_count as f32 * 28.0).min(200.0);

        let popup_id = egui::Id::new("command_suggestions_popup");

        let mut action_to_take: Option<CommandAction> = None;

        // Anchor the popup's bottom-left corner above the input's top-left corner.
        // Increase the negative Y offset to raise the popup higher.
        let area = egui::Area::new(popup_id)
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(10.0, -45.0))
            .fixed_pos(input_rect.left_top());

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

                                let row_response = ui
                                    .horizontal(|ui| {
                                        let name_display = command.name.clone();

                                        let name_color = if is_selected {
                                            Color32::WHITE
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
                                    action_to_take =
                                        Some(CommandAction::UseCommand(command.name.clone()));
                                }

                                if row_response.hovered() {
                                    let mut tooltip = command.description.clone();
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

        let mut msg = vec![ClientPacketType::Cmd as u8];
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

        let mut msg = vec![ClientPacketType::Chat as u8];
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
}
