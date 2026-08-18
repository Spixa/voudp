use egui::{Color32, Pos2, Vec2};

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
    text_color: egui::Color32,
    username: Option<&str>, // optional for context menu
    input: &mut String,
) -> egui::Response {
    let bubble_color = if text_color == egui::Color32::WHITE {
        egui::Color32::from_rgb(0, 122, 255) // blue for self
    } else {
        egui::Color32::from_rgb(52, 199, 89) // gray for others
    };

    let frame = egui::Frame::none()
        .fill(bubble_color)
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::vec2(12.0, 8.0));

    let inner = frame.show(ui, |ui| {
        ui.set_max_width(300.0);
        ui.style_mut().wrap = Some(true);
        ui.label(egui::RichText::new(msg).color(text_color).size(14.0));
        ui.style_mut().wrap = None;
    });

    let response = inner.response;

    // Attach context menu to the bubble area
    response.context_menu(|ui| {
        if ui.button("↩ Reply").clicked() {
            *input = format!("> {} \n\n", msg);
            ui.close_menu();
        }
        if ui.button("📋 Copy message").clicked() {
            ui.output_mut(|o| o.copied_text = msg.to_string());
            ui.close_menu();
        }
        if let Some(name) = username
            && ui.button("👤 Copy username").clicked()
        {
            ui.output_mut(|o| o.copied_text = name.to_string());
            ui.close_menu();
        }

        if ui.button("🔊 Adjust volume").clicked() {
            *input = format!("/volume {} ", username.unwrap_or("unknown"));
            ui.close_menu();
        }
    });

    response
}

pub fn badge(ui: &mut egui::Ui, text: impl Into<String>, color: egui::Color32) {
    let text = egui::RichText::new(text.into())
        .color(color)
        .small()
        .strong();

    egui::Frame::none()
        .fill(ui.ctx().style().visuals.faint_bg_color)
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(text);
        });
}

pub fn connection_activity_wifi(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let arc_count = 3;
    let segments = 90;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());

    let painter = ui.painter_at(rect);
    let center = rect.center();

    let time = ui.input(|i| i.time);
    let active_arcs = ((time * 0.4) % 1.0 * arc_count as f64).floor() as usize;

    // ===== SIZING =====
    let dot_radius = size * 0.06;
    let arc_thickness = size * 0.075;
    let arc_gap = size * 0.06;

    let first_arc_radius = dot_radius + arc_gap + arc_thickness * 0.5;

    let vertical_shift = size * 0.12;
    let origin = center + egui::vec2(0.0, vertical_shift);

    // ===== DRAW ARCS (ONLY ACTIVE ONES) =====
    for i in 0..=active_arcs.min(arc_count - 1) {
        let radius = first_arc_radius + i as f32 * (arc_thickness + arc_gap);

        let stroke = egui::Stroke::new(arc_thickness, color);
        let mut points = Vec::with_capacity(segments);

        let start = std::f32::consts::PI * 1.15;
        let end = std::f32::consts::PI * 1.85;

        for s in 0..segments {
            let t = s as f32 / (segments - 1) as f32;
            let a = start + t * (end - start);

            points.push(origin + egui::vec2(radius * a.cos(), radius * a.sin()));
        }

        painter.add(egui::Shape::line(points, stroke));
    }

    // ===== BASE DOT (ALWAYS VISIBLE) =====
    painter.circle_filled(origin, dot_radius, color);
}

pub struct Particle {
    pub pos: Pos2,
    pub vel: Vec2,
    pub color: Color32,
    pub rotation: f32,
    pub angular_vel: f32,
}

pub fn update_confetti(ui: &mut egui::Ui, particles: &mut Vec<Particle>) {
    let dt = ui.input(|i| i.stable_dt);
    let painter = ui.painter();

    particles.retain_mut(|p| {
        p.vel.y += 500.0 * dt; // gravity
        p.pos += p.vel * dt;
        p.rotation += p.angular_vel * dt;

        // draw the confetti (a small rotated rect)
        let rect = egui::Rect::from_center_size(p.pos, egui::vec2(5.0, 5.0));
        let mesh = egui::Shape::rect_filled(rect, 1.0, p.color); // Simplified
        painter.add(mesh);

        // keep particle if it's still on screen
        p.pos.y < ui.ctx().screen_rect().height()
    });

    // request a repaint to keep the animation smooth
    ui.ctx().request_repaint();
}

fn _name_color(_: &str) -> egui::Color32 {
    Color32::YELLOW
}
