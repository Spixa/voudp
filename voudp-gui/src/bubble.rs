use chrono::Local;
use egui::Color32;

pub fn parse_chat_message(msg: &str) -> Option<(String, String, String)> {
    if !msg.starts_with("[#") {
        return None;
    }
    let after_bracket = msg.strip_prefix("[#")?;
    let (channel, rest) = after_bracket.split_once("] ")?;
    let (name, content) = rest.split_once(": ")?;
    Some((channel.to_string(), name.to_string(), content.to_string()))
}

pub fn parse_system_message(msg: &str) -> Option<(String, String)> {
    if !msg.starts_with('[') {
        return None;
    }
    let after_bracket = msg.strip_prefix('[')?;
    let (src, rest) = after_bracket.split_once("] ")?;
    Some((src.to_string(), rest.to_string()))
}

// For regular messages without name parsing (fallback)
pub fn bubble_ui(
    ui: &mut egui::Ui,
    msg: &str,
    time: &chrono::DateTime<Local>,
    text_color: egui::Color32,
) {
    let bubble_color = if text_color == egui::Color32::WHITE {
        egui::Color32::from_rgb(0, 120, 215) // blue for self
    } else {
        egui::Color32::from_rgb(240, 240, 240) // gray for others
    };

    egui::Frame::none()
        .fill(bubble_color)
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::vec2(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_max_width(300.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(msg).color(text_color).size(14.0));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("{}", time.format("%H:%M")))
                        .color(if text_color == egui::Color32::WHITE {
                            egui::Color32::from_rgb(200, 220, 255)
                        } else {
                            egui::Color32::from_rgb(120, 120, 120)
                        })
                        .size(11.0),
                );
            });
        });
}

fn name_color(_: &str) -> egui::Color32 {
    Color32::YELLOW
}
// For parsed messages with name + content
pub fn bubble_ui_with_name(
    ui: &mut egui::Ui,
    display_name: &str,
    content: &str,
    time: &chrono::DateTime<Local>,
    bubble_color: egui::Color32,
    text_color: egui::Color32,
) {
    egui::Frame::none()
        .fill(bubble_color)
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::vec2(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_max_width(300.0);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(display_name)
                        .color(name_color(display_name))
                        .size(13.0)
                        .strong(),
                );

                // Message + timestamp on same line
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(content).color(text_color).size(14.0));
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("{}", time.format("%H:%M")))
                            .color(if text_color == egui::Color32::WHITE {
                                egui::Color32::from_rgb(200, 220, 255)
                            } else {
                                egui::Color32::from_rgb(120, 120, 120)
                            })
                            .size(11.0),
                    );
                });
            });
        });
}
