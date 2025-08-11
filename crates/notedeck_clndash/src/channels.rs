use crate::event::LoadingState;
use crate::ui;
use egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct ListPeerChannel {
    pub short_channel_id: String,
    pub our_reserve_msat: i64,
    pub to_us_msat: i64,
    pub total_msat: i64,
    pub their_reserve_msat: i64,
}

pub struct Channel {
    pub to_us: i64,
    pub to_them: i64,
    pub original: ListPeerChannel,
}

pub struct Channels {
    pub max_total_msat: i64,
    pub avail_in: i64,
    pub avail_out: i64,
    pub channels: Vec<Channel>,
}

pub fn channels_ui(ui: &mut egui::Ui, channels: &LoadingState<Channels, lnsocket::Error>) {
    match channels {
        LoadingState::Loaded(channels) => {
            if channels.channels.is_empty() {
                ui.label("no channels yet...");
                return;
            }

            for channel in &channels.channels {
                channel_ui(ui, channel, channels.max_total_msat);
            }

            ui.label(format!(
                "available out {}",
                ui::human_sat(channels.avail_out)
            ));
            ui.label(format!("available in {}", ui::human_sat(channels.avail_in)));
        }
        LoadingState::Failed(err) => {
            ui.label(format!("error fetching channels: {err}"));
        }
        LoadingState::Loading => {
            ui.label("fetching channels...");
        }
    }
}

pub fn channel_ui(ui: &mut egui::Ui, c: &Channel, max_total_msat: i64) {
    // ---------- numbers ----------
    let short_channel_id = &c.original.short_channel_id;

    let cap_ratio = (c.original.total_msat as f32 / max_total_msat.max(1) as f32).clamp(0.0, 1.0);
    // Feel free to switch to log scaling if you have whales:
    //let cap_ratio = ((c.original.total_msat as f32 + 1.0).log10() / (max_total_msat as f32 + 1.0).log10()).clamp(0.0, 1.0);

    // ---------- colors & style ----------
    let out_color = Color32::from_rgb(84, 69, 201); // blue
    let in_color = Color32::from_rgb(158, 56, 180); // purple

    // Thickness scales with capacity, but keeps a nice minimum
    let thickness = 10.0 + cap_ratio * 22.0; // 10 → 32 px
    let row_h = thickness + 14.0;

    // ---------- layout ----------
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    let bar_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - thickness * 0.5),
        egui::pos2(rect.right(), rect.center().y + thickness * 0.5),
    );
    let corner_radius = (thickness * 0.5) as u8;
    let out_radius = egui::CornerRadius {
        ne: 0,
        nw: corner_radius,
        sw: corner_radius,
        se: 0,
    };
    let in_radius = egui::CornerRadius {
        ne: corner_radius,
        nw: 0,
        sw: 0,
        se: corner_radius,
    };
    /*
    painter.rect_filled(bar_rect, rounding, track_color);
    painter.rect_stroke(bar_rect, rounding, track_stroke, egui::StrokeKind::Middle);
    */

    // Split widths
    let usable = (c.to_us + c.to_them).max(1) as f32;
    let out_w = (bar_rect.width() * (c.to_us as f32 / usable)).round();
    let split_x = bar_rect.left() + out_w;

    // Outbound fill (left)
    let out_rect = egui::Rect::from_min_max(bar_rect.min, egui::pos2(split_x, bar_rect.max.y));
    if out_rect.width() > 0.5 {
        painter.rect_filled(out_rect, out_radius, out_color);
    }

    // Inbound fill (right)
    let in_rect = egui::Rect::from_min_max(egui::pos2(split_x, bar_rect.min.y), bar_rect.max);
    if in_rect.width() > 0.5 {
        painter.rect_filled(in_rect, in_radius, in_color);
    }

    // Tooltip
    response.on_hover_text_at_pointer(format!(
        "Channel ID {short_channel_id}\nOutbound (ours): {} sats\nInbound (theirs): {} sats\nCapacity: {} sats",
        ui::human_sat(c.to_us),
        ui::human_sat(c.to_them),
        ui::human_sat(c.original.total_msat),
    ));
}
