//! Room 3D rendering for nostrverse via renderbud

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui};

use super::room_state::{NostrverseAction, NostrverseState};

/// Response from rendering the nostrverse view
pub struct NostrverseResponse {
    pub response: Response,
    pub action: Option<NostrverseAction>,
}

/// Render the nostrverse room view with 3D scene
pub fn show_room_view(
    ui: &mut Ui,
    state: &mut NostrverseState,
    renderer: &renderbud::egui::EguiRenderer,
) -> NostrverseResponse {
    let available_size = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(available_size, Sense::click_and_drag());

    // Update renderer target size
    {
        let mut r = renderer.renderer.lock().unwrap();
        r.set_target_size((rect.width() as u32, rect.height() as u32));

        // Handle mouse drag for camera orbit
        if response.dragged() {
            let delta = response.drag_delta();
            r.on_mouse_drag(delta.x, delta.y);
        }

        // Handle scroll for zoom
        if response.hover_pos().is_some() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                r.on_scroll(scroll * 0.01);
            }
        }
    }

    // Register the 3D scene paint callback
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        renderbud::egui::SceneRender,
    ));

    // Draw 2D overlays on top of the 3D scene
    let painter = ui.painter_at(rect);
    draw_info_overlay(&painter, state, rect);

    NostrverseResponse {
        response,
        action: None,
    }
}

fn draw_info_overlay(painter: &egui::Painter, state: &NostrverseState, rect: Rect) {
    let room_name = state
        .room
        .as_ref()
        .map(|r| r.name.as_str())
        .unwrap_or("Loading...");

    let info_text = format!("{} | Objects: {}", room_name, state.objects.len());

    // Background for readability
    let text_pos = Pos2::new(rect.left() + 10.0, rect.top() + 10.0);
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, rect.top() + 4.0),
            egui::vec2(200.0, 24.0),
        ),
        4.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 160),
    );

    painter.text(
        text_pos,
        egui::Align2::LEFT_TOP,
        info_text,
        egui::FontId::proportional(14.0),
        Color32::from_rgba_unmultiplied(200, 200, 210, 220),
    );
}

/// Render the object inspection panel (side panel when object is selected)
pub fn render_inspection_panel(
    ui: &mut Ui,
    state: &mut NostrverseState,
) -> Option<NostrverseAction> {
    let selected_id = state.selected_object.as_ref()?;
    let obj = state.objects.iter().find(|o| &o.id == selected_id)?;

    let mut action = None;

    egui::Frame::default()
        .fill(Color32::from_rgba_unmultiplied(30, 35, 45, 240))
        .inner_margin(12.0)
        .outer_margin(8.0)
        .corner_radius(8.0)
        .stroke(Stroke::new(1.0, Color32::from_rgb(80, 90, 110)))
        .show(ui, |ui| {
            ui.set_min_width(180.0);

            ui.horizontal(|ui| {
                ui.strong("Object Inspector");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("X").clicked() {
                        action = Some(NostrverseAction::SelectObject(None));
                    }
                });
            });

            ui.separator();

            ui.label(format!("Name: {}", obj.name));
            ui.label(format!(
                "Position: ({:.1}, {:.1}, {:.1})",
                obj.position.x, obj.position.y, obj.position.z
            ));
            ui.label(format!(
                "Scale: ({:.1}, {:.1}, {:.1})",
                obj.scale.x, obj.scale.y, obj.scale.z
            ));

            if let Some(url) = &obj.model_url {
                ui.separator();
                ui.small(format!("Model: {}", url));
            }

            ui.separator();

            let id_display = if obj.id.len() > 16 {
                format!("{}...", &obj.id[..16])
            } else {
                obj.id.clone()
            };
            ui.small(format!("ID: {}", id_display));
        });

    action
}
