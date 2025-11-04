use crate::channels::Channels;
use crate::event::LoadingState;
use crate::ui;

#[derive(Clone, Default)]
pub struct Summary {
    pub total_msat: i64,
    pub avail_out_msat: i64,
    pub avail_in_msat: i64,
    pub channel_count: usize,
    pub largest_msat: i64,
    pub outbound_pct: f32, // fraction of total capacity
}

pub fn compute_summary(ch: &Channels) -> Summary {
    let total_msat: i64 = ch.channels.iter().map(|c| c.original.total_msat).sum();
    let largest_msat: i64 = ch
        .channels
        .iter()
        .map(|c| c.original.total_msat)
        .max()
        .unwrap_or(0);
    let outbound_pct = if total_msat > 0 {
        ch.avail_out as f32 / total_msat as f32
    } else {
        0.0
    };

    Summary {
        total_msat,
        avail_out_msat: ch.avail_out,
        avail_in_msat: ch.avail_in,
        channel_count: ch.channels.len(),
        largest_msat,
        outbound_pct,
    }
}

pub fn summary_ui(
    ui: &mut egui::Ui,
    last_summary: Option<&Summary>,
    summary: &LoadingState<Summary, lnsocket::Error>,
) {
    match summary {
        LoadingState::Loading => {
            ui.label("loading summary");
        }
        LoadingState::Failed(err) => {
            ui.label(format!("Failed to get summary: {err}"));
        }
        LoadingState::Loaded(summary) => {
            summary_cards_ui(ui, summary, last_summary);
            ui.add_space(8.0);
        }
    }
}

pub fn summary_cards_ui(ui: &mut egui::Ui, s: &Summary, prev: Option<&Summary>) {
    let old = prev.cloned().unwrap_or_default();
    let items: [(&str, String, Option<String>); 6] = [
        (
            "Total capacity",
            ui::human_sat(s.total_msat),
            prev.map(|_| ui::delta_str(s.total_msat, old.total_msat)),
        ),
        (
            "Avail out",
            ui::human_sat(s.avail_out_msat),
            prev.map(|_| ui::delta_str(s.avail_out_msat, old.avail_out_msat)),
        ),
        (
            "Avail in",
            ui::human_sat(s.avail_in_msat),
            prev.map(|_| ui::delta_str(s.avail_in_msat, old.avail_in_msat)),
        ),
        ("# Channels", s.channel_count.to_string(), None),
        ("Largest", ui::human_sat(s.largest_msat), None),
        (
            "Outbound %",
            format!("{:.0}%", s.outbound_pct * 100.0),
            None,
        ),
    ];

    // --- responsive columns ---
    let min_card = 160.0;
    let cols = ((ui.available_width() / min_card).floor() as usize).max(1);

    egui::Grid::new("summary_grid")
        .num_columns(cols)
        .min_col_width(min_card)
        .spacing(egui::vec2(8.0, 8.0))
        .show(ui, |ui| {
            let items_len = items.len();
            for (i, (t, v, d)) in items.into_iter().enumerate() {
                card_cell(ui, t, v, d, min_card);

                // End the row when we filled a row worth of cells
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }

            // If the last row wasn't full, close it anyway
            if !items_len.is_multiple_of(cols) {
                ui.end_row();
            }
        });
}

fn card_cell(ui: &mut egui::Ui, title: &str, value: String, delta: Option<String>, min_card: f32) {
    let weak = ui.visuals().weak_text_color();
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(10))
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .show(ui, |ui| {
            ui.set_min_width(min_card);
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(title).small().color(weak))
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(value).strong().size(18.0))
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
                if let Some(d) = delta {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(d).small().color(weak))
                            .wrap_mode(egui::TextWrapMode::Wrap),
                    );
                }
            });
            ui.set_min_height(20.0);
        });
}
