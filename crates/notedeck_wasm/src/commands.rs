use std::collections::HashMap;

#[derive(Clone)]
pub enum UiCommand {
    Label(String),
    Heading(String),
    Button(String),
    AddSpace(f32),
    DrawRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
    },
    DrawCircle {
        cx: f32,
        cy: f32,
        r: f32,
        color: u32,
    },
    DrawLine {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: u32,
    },
    DrawText {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        color: u32,
    },
}

fn color_from_u32(c: u32) -> egui::Color32 {
    let r = ((c >> 24) & 0xFF) as u8;
    let g = ((c >> 16) & 0xFF) as u8;
    let b = ((c >> 8) & 0xFF) as u8;
    let a = (c & 0xFF) as u8;
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Render buffered commands into egui, returning button click events.
/// Keys are `button_key(text, occurrence)`.
pub fn render_commands(commands: &[UiCommand], ui: &mut egui::Ui) -> HashMap<String, bool> {
    let mut events = HashMap::new();
    let mut button_occ: HashMap<&str, u32> = HashMap::new();
    let origin = ui.min_rect().left_top();
    let painter = ui.painter().clone();

    for cmd in commands {
        match cmd {
            UiCommand::Label(text) => {
                ui.label(text.as_str());
            }
            UiCommand::Heading(text) => {
                ui.heading(text.as_str());
            }
            UiCommand::Button(text) => {
                let occ = button_occ.entry(text.as_str()).or_insert(0);
                let key = button_key(text, *occ);
                *occ += 1;
                let clicked = ui.button(text.as_str()).clicked();
                events.insert(key, clicked);
            }
            UiCommand::AddSpace(px) => {
                ui.add_space(*px);
            }
            UiCommand::DrawRect { x, y, w, h, color } => {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(origin.x + x, origin.y + y),
                    egui::vec2(*w, *h),
                );
                painter.rect_filled(rect, 0.0, color_from_u32(*color));
            }
            UiCommand::DrawCircle { cx, cy, r, color } => {
                let center = egui::pos2(origin.x + cx, origin.y + cy);
                painter.circle_filled(center, *r, color_from_u32(*color));
            }
            UiCommand::DrawLine {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => {
                let p1 = egui::pos2(origin.x + x1, origin.y + y1);
                let p2 = egui::pos2(origin.x + x2, origin.y + y2);
                painter.line_segment([p1, p2], egui::Stroke::new(*width, color_from_u32(*color)));
            }
            UiCommand::DrawText {
                x,
                y,
                text,
                size,
                color,
            } => {
                let pos = egui::pos2(origin.x + x, origin.y + y);
                painter.text(
                    pos,
                    egui::Align2::LEFT_TOP,
                    text,
                    egui::FontId::proportional(*size),
                    color_from_u32(*color),
                );
            }
        }
    }

    events
}

pub fn button_key(text: &str, occurrence: u32) -> String {
    if occurrence == 0 {
        text.to_string()
    } else {
        format!("{}#{}", text, occurrence)
    }
}
