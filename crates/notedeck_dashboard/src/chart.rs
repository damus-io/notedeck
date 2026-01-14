use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

pub fn palette(i: usize) -> Color32 {
    const P: [Color32; 10] = [
        Color32::from_rgb(231, 76, 60),
        Color32::from_rgb(52, 152, 219),
        Color32::from_rgb(46, 204, 113),
        Color32::from_rgb(155, 89, 182),
        Color32::from_rgb(241, 196, 15),
        Color32::from_rgb(230, 126, 34),
        Color32::from_rgb(26, 188, 156),
        Color32::from_rgb(149, 165, 166),
        Color32::from_rgb(52, 73, 94),
        Color32::from_rgb(233, 150, 122),
    ];
    P[i % P.len()]
}

// ----------------------
// Bar chart (unchanged)
// ----------------------

#[derive(Debug, Clone)]
pub struct Bar {
    pub label: String,
    pub value: f32,
    pub color: Color32,
}

#[derive(Clone, Copy)]
pub struct BarChartStyle {
    pub row_height: f32,
    pub gap: f32,
    pub rounding: f32,
    pub show_values: bool,
    pub value_precision: usize,
}

impl Default for BarChartStyle {
    fn default() -> Self {
        Self {
            row_height: 18.0,
            gap: 6.0,
            rounding: 3.0,
            show_values: true,
            value_precision: 0,
        }
    }
}

/// Draws a horizontal bar chart. Returns the combined response so you can check hover/click if desired.
pub fn horizontal_bar_chart(
    ui: &mut Ui,
    title: Option<&str>,
    bars: &[Bar],
    style: BarChartStyle,
) -> Response {
    if let Some(t) = title {
        ui.label(t);
    }

    if bars.is_empty() {
        return ui.label("No data");
    }

    let max_v = bars
        .iter()
        .map(|b| b.value.max(0.0))
        .fold(0.0_f32, f32::max);

    if max_v <= 0.0 {
        return ui.label("No data");
    }

    // Layout: label column + bar column
    let label_col_w = ui
        .fonts(|f| {
            bars.iter()
                .map(|b| {
                    f.layout_no_wrap(
                        b.label.to_owned(),
                        FontId::proportional(14.0),
                        ui.visuals().text_color(),
                    )
                    .size()
                    .x
                })
                .fold(0.0, f32::max)
        })
        .ceil()
        + 10.0;

    let avail_w = ui.available_width().max(50.0);
    let bar_col_w = (avail_w - label_col_w).max(50.0);

    let total_h =
        bars.len() as f32 * style.row_height + (bars.len().saturating_sub(1) as f32) * style.gap;
    let (outer_rect, outer_resp) =
        ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());
    let painter = ui.painter_at(outer_rect);

    // Optional: faint background
    painter.rect_filled(outer_rect, 6.0, ui.visuals().faint_bg_color);

    let mut y = outer_rect.top();

    for b in bars {
        let row_rect = Rect::from_min_size(
            Pos2::new(outer_rect.left(), y),
            Vec2::new(avail_w, style.row_height),
        );
        let row_resp = ui.interact(
            row_rect,
            ui.id().with(&b.label).with(y as i64),
            Sense::hover(),
        );

        // Label (left)
        let label_pos = Pos2::new(row_rect.left() + 6.0, row_rect.center().y);
        painter.text(
            label_pos,
            Align2::LEFT_CENTER,
            &b.label,
            FontId::proportional(14.0),
            ui.visuals().text_color(),
        );

        // Bar background track (right)
        let track_rect = Rect::from_min_max(
            Pos2::new(row_rect.left() + label_col_w, row_rect.top() + 2.0),
            Pos2::new(
                row_rect.left() + label_col_w + bar_col_w,
                row_rect.bottom() - 2.0,
            ),
        );
        painter.rect_filled(
            track_rect,
            style.rounding,
            ui.visuals().widgets.inactive.bg_fill,
        );
        painter.rect_stroke(
            track_rect,
            style.rounding,
            Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
            StrokeKind::Middle,
        );

        // Filled portion
        let frac = (b.value.max(0.0) / max_v).clamp(0.0, 1.0);
        let fill_w = track_rect.width() * frac;
        if fill_w > 0.0 {
            let fill_rect = Rect::from_min_max(
                track_rect.min,
                Pos2::new(track_rect.min.x + fill_w, track_rect.max.y),
            );
            painter.rect_filled(fill_rect, style.rounding, b.color);
        }

        // Value label (right-aligned at end of track)
        if style.show_values {
            let txt = if style.value_precision == 0 {
                format!("{:.0}", b.value)
            } else {
                format!("{:.*}", style.value_precision, b.value)
            };
            painter.text(
                Pos2::new(track_rect.right() - 6.0, row_rect.center().y),
                Align2::RIGHT_CENTER,
                txt,
                FontId::proportional(13.0),
                ui.visuals().text_color(),
            );
        }

        // Tooltip on hover
        if row_resp.hovered() {
            let sum = bars.iter().map(|x| x.value.max(0.0)).sum::<f32>().max(1.0);
            let pct = (b.value / sum) * 100.0;
            row_resp.on_hover_text(format!("{}: {:.0} ({:.1}%)", b.label, b.value, pct));
        }

        y += style.row_height + style.gap;
    }

    outer_resp
}
