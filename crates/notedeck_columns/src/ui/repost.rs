use std::f32::consts::PI;

use egui::{
    epaint::PathShape, pos2, vec2, CornerRadius, Layout, Margin, Pos2, RichText, Sense, Shape,
    Stroke,
};
use egui_extras::StripBuilder;
use enostr::NoteId;
use notedeck::{fonts::get_font_size, NotedeckTextStyle};
use notedeck_ui::app_images;

use crate::repost::RepostAction;

pub struct RepostDecisionView<'a> {
    noteid: &'a NoteId,
}

impl<'a> RepostDecisionView<'a> {
    pub fn new(noteid: &'a NoteId) -> Self {
        Self { noteid }
    }

    pub fn show(&self, ui: &mut egui::Ui) -> Option<RepostAction> {
        let mut action = None;
        egui::Frame::new()
            .inner_margin(Margin::symmetric(48, 24))
            .show(ui, |ui| {
                StripBuilder::new(ui)
                    .sizes(egui_extras::Size::exact(48.0), 2)
                    .size(egui_extras::Size::exact(80.0))
                    .vertical(|mut strip| {
                        strip.cell(|ui| {
                            if ui
                                .with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                                    let r = ui.add(
                                        app_images::repost_image(ui.visuals().dark_mode)
                                            .max_height(24.0)
                                            .sense(Sense::click()),
                                    );
                                    r.union(ui.add(repost_item_text("Repost")))
                                })
                                .inner
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                action = Some(RepostAction::Kind06Repost(*self.noteid))
                            }
                        });

                        strip.cell(|ui| {
                            if ui
                                .with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                                    let r = ui
                                        .add(quote_icon())
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    r.union(ui.add(repost_item_text("Quote")))
                                })
                                .inner
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                action = Some(RepostAction::Quote(*self.noteid))
                            }
                        });

                        strip.cell(|ui| {
                            ui.add_space(16.0);
                            let resp = ui.allocate_response(
                                vec2(ui.available_width(), 48.0),
                                Sense::click(),
                            );

                            let color = if resp.hovered() {
                                ui.visuals().widgets.hovered.bg_fill
                            } else {
                                ui.visuals().text_color()
                            };

                            let painter = ui.painter_at(resp.rect);
                            ui.painter().rect_stroke(
                                resp.rect,
                                CornerRadius::same(32),
                                egui::Stroke::new(1.5, color),
                                egui::StrokeKind::Inside,
                            );

                            let galley = painter.layout_no_wrap(
                                "Cancel".to_owned(),
                                NotedeckTextStyle::Heading3.get_font_id(ui.ctx()),
                                ui.visuals().text_color(),
                            );

                            painter.galley(
                                galley_top_left_from_center(&galley, resp.rect.center()),
                                galley,
                                ui.visuals().text_color(),
                            );

                            if resp.clicked() {
                                action = Some(RepostAction::Cancel);
                            }
                        });
                    });
            });

        action
    }
}

fn galley_top_left_from_center(galley: &std::sync::Arc<egui::Galley>, center: Pos2) -> Pos2 {
    let mut top_left = center;
    top_left.x -= galley.rect.width() / 2.0;
    top_left.y -= galley.rect.height() / 2.0;

    top_left
}

fn repost_item_text(text: &str) -> impl egui::Widget + use<'_> {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.add(egui::Label::new(
            RichText::new(text).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3)),
        ))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

pub fn quote_icon() -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let h = 32.0; // scaling constant
        let color = ui.visuals().strong_text_color();
        let r = h * 0.12; // dot radius
        let arc_r = r * 2.0; // larger so it protrudes above
        let sw = h * 0.06; // stroke width
        let stroke = Stroke::new(sw, color);

        // Same horizontal layout as before (in "local" coords using height scale)
        let cx1_raw = h * 0.34;
        let cx2_raw = h * 0.72;
        let gap = cx2_raw - cx1_raw;

        // Bounds including stroke (only the left side needs +sw/2 for arcs)
        let left_bound = (cx1_raw - r) - sw * 0.5;
        let right_bound = (cx2_raw + r).max(cx2_raw + (arc_r - r)); // safe if arc_r > 2r
        let width = right_bound - left_bound;

        // Vertical bounds: arc top needs sw/2; total content height = arc_r + r + sw/2
        let content_h = arc_r + r + sw * 0.5;
        let v_margin = (h - content_h) * 0.5;

        let resp = ui.allocate_response(vec2(width, h), egui::Sense::click());
        let rect = resp.rect;
        let origin = rect.min;

        // Place centers with bounds aligned to rect
        let dx = origin.x - left_bound;
        let cy = origin.y + v_margin + arc_r + sw * 0.5;

        let c1 = pos2(dx + cx1_raw, cy);
        let c2 = pos2(dx + cx1_raw + gap, cy);

        // arc centers (unchanged)
        let a1 = pos2(c1.x + (arc_r - r) + 0.8, c1.y);
        let a2 = pos2(c2.x + (arc_r - r) + 0.8, c2.y);

        // Draw arcs FIRST (upper-left quadrant [π, 3π/2])
        let arc = |ac: egui::Pos2| {
            let steps = 16;
            (0..=steps)
                .map(|i| {
                    let t = i as f32 / steps as f32;
                    let ang = PI + t * (PI * 0.5);
                    pos2(ac.x + arc_r * ang.cos(), ac.y + arc_r * ang.sin())
                })
                .collect::<Vec<_>>()
        };
        let painter = ui.painter_at(rect);
        painter.add(Shape::Path(PathShape::line(arc(a1), stroke)));
        painter.add(Shape::Path(PathShape::line(arc(a2), stroke)));

        // Dots LAST to hide the junction
        painter.circle_filled(c1, r, color);
        painter.circle_filled(c2, r, color);

        resp
    }
}
