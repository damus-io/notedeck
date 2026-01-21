use egui::{Color32, Pos2, Response, Sense, Stroke, Ui, Vec2};

#[derive(Clone, Copy)]
pub struct SparkStyle {
    pub stroke: Stroke,
    pub fill_alpha: u8,
    pub rounding: f32,
}

impl Default for SparkStyle {
    fn default() -> Self {
        Self {
            stroke: Stroke::new(1.5, Color32::WHITE),
            fill_alpha: 40,
            rounding: 3.0,
        }
    }
}

/// values are samples over time (left=oldest, right=newest)
pub fn sparkline(
    ui: &mut Ui,
    size: Vec2,
    values: &[f32],
    color: Color32,
    style: SparkStyle,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);

    // background
    //painter.rect_filled(rect, style.rounding, ui.visuals().widgets.inactive.bg_fill);

    if values.len() < 2 {
        return resp;
    }

    let mut min_v = f32::INFINITY;
    let mut max_v = f32::NEG_INFINITY;
    for &v in values {
        let v = v.max(0.0);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    // avoid div by zero, also allow flat lines
    let span = (max_v - min_v).max(1.0);

    let n = values.len();
    let dx = rect.width() / (n.saturating_sub(1) as f32).max(1.0);

    let mut pts: Vec<Pos2> = Vec::with_capacity(n);
    for (i, &v) in values.iter().enumerate() {
        let t = (v.max(0.0) - min_v) / span; // 0..1
        let x = rect.left() + (i as f32) * dx;
        let y = rect.bottom() - t * rect.height();
        pts.push(Pos2::new(x, y));
    }

    // fill under curve
    /*
    let mut fill_pts = Vec::with_capacity(pts.len() + 2);
    fill_pts.extend_from_slice(&pts);
    fill_pts.push(Pos2::new(rect.right(), rect.bottom()));
    fill_pts.push(Pos2::new(rect.left(), rect.bottom()));

    let mut fill_color = color;
    fill_color = Color32::from_rgba_premultiplied(
        fill_color.r(),
        fill_color.g(),
        fill_color.b(),
        style.fill_alpha,
    );

    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: fill_pts,
        closed: true,
        fill: fill_color,
        stroke: Stroke::NONE.into(),
    }));
    */

    // line
    painter.add(egui::Shape::line(
        pts,
        Stroke::new(style.stroke.width, color),
    ));

    resp
}
