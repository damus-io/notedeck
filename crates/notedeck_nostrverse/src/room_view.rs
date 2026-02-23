//! Room 3D rendering and editing UI for nostrverse via renderbud

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui};
use glam::Vec3;

use super::room_state::{NostrverseAction, NostrverseState, RoomObject, RoomShape};

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

    // Update renderer target size and handle input
    {
        let mut r = renderer.renderer.lock().unwrap();
        r.set_target_size((rect.width() as u32, rect.height() as u32));

        // Handle mouse drag for camera look
        if response.dragged() {
            let delta = response.drag_delta();
            r.on_mouse_drag(delta.x, delta.y);
        }

        // Handle scroll for speed adjustment
        if response.hover_pos().is_some() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                r.on_scroll(scroll * 0.01);
            }
        }

        // WASD + QE movement
        let dt = ui.input(|i| i.stable_dt);
        let mut forward = 0.0_f32;
        let mut right = 0.0_f32;
        let mut up = 0.0_f32;

        ui.input(|i| {
            if i.key_down(egui::Key::W) {
                forward -= 1.0;
            }
            if i.key_down(egui::Key::S) {
                forward += 1.0;
            }
            if i.key_down(egui::Key::D) {
                right += 1.0;
            }
            if i.key_down(egui::Key::A) {
                right -= 1.0;
            }
            if i.key_down(egui::Key::E) || i.key_down(egui::Key::Space) {
                up += 1.0;
            }
            if i.key_down(egui::Key::Q) {
                up -= 1.0;
            }
        });

        if forward != 0.0 || right != 0.0 || up != 0.0 {
            r.process_movement(forward, right, up, dt);
            ui.ctx().request_repaint();
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

/// Render the side panel with room editing, object list, and object inspector.
pub fn render_editing_panel(ui: &mut Ui, state: &mut NostrverseState) -> Option<NostrverseAction> {
    let mut action = None;

    let panel = egui::Frame::default()
        .fill(Color32::from_rgba_unmultiplied(30, 35, 45, 240))
        .inner_margin(12.0)
        .outer_margin(8.0)
        .corner_radius(8.0)
        .stroke(Stroke::new(1.0, Color32::from_rgb(80, 90, 110)));

    panel.show(ui, |ui| {
        ui.set_min_width(220.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            // --- Room Properties ---
            if let Some(room) = &mut state.room {
                ui.strong("Room");
                ui.separator();

                let name_changed = ui
                    .horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut room.name).changed()
                    })
                    .inner;

                let mut width = room.width;
                let mut height = room.height;
                let mut depth = room.depth;

                let dims_changed = ui
                    .horizontal(|ui| {
                        ui.label("W:");
                        let w = ui
                            .add(
                                egui::DragValue::new(&mut width)
                                    .speed(0.5)
                                    .range(1.0..=200.0),
                            )
                            .changed();
                        ui.label("H:");
                        let h = ui
                            .add(
                                egui::DragValue::new(&mut height)
                                    .speed(0.5)
                                    .range(1.0..=200.0),
                            )
                            .changed();
                        ui.label("D:");
                        let d = ui
                            .add(
                                egui::DragValue::new(&mut depth)
                                    .speed(0.5)
                                    .range(1.0..=200.0),
                            )
                            .changed();
                        w || h || d
                    })
                    .inner;

                room.width = width;
                room.height = height;
                room.depth = depth;

                let shape_changed = ui
                    .horizontal(|ui| {
                        ui.label("Shape:");
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("room_shape")
                            .selected_text(match room.shape {
                                RoomShape::Rectangle => "Rectangle",
                                RoomShape::Circle => "Circle",
                                RoomShape::Custom => "Custom",
                            })
                            .show_ui(ui, |ui| {
                                changed |= ui
                                    .selectable_value(
                                        &mut room.shape,
                                        RoomShape::Rectangle,
                                        "Rectangle",
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(&mut room.shape, RoomShape::Circle, "Circle")
                                    .changed();
                            });
                        changed
                    })
                    .inner;

                if name_changed || dims_changed || shape_changed {
                    state.dirty = true;
                }

                ui.add_space(8.0);
            }

            // --- Object List ---
            ui.strong("Objects");
            ui.separator();

            let num_objects = state.objects.len();
            for i in 0..num_objects {
                let is_selected = state
                    .selected_object
                    .as_ref()
                    .map(|s| s == &state.objects[i].id)
                    .unwrap_or(false);

                let label = format!("{} ({})", state.objects[i].name, state.objects[i].id);
                if ui.selectable_label(is_selected, label).clicked() {
                    let id = state.objects[i].id.clone();
                    state.selected_object = if is_selected { None } else { Some(id) };
                }
            }

            // Add object button
            ui.add_space(4.0);
            if ui.button("+ Add Object").clicked() {
                let new_id = format!("obj-{}", state.objects.len() + 1);
                let obj = RoomObject::new(new_id.clone(), "New Object".to_string(), Vec3::ZERO);
                action = Some(NostrverseAction::AddObject(obj));
            }

            ui.add_space(12.0);

            // --- Object Inspector ---
            if let Some(selected_id) = state.selected_object.clone()
                && let Some(obj) = state.objects.iter_mut().find(|o| o.id == selected_id)
            {
                ui.strong("Inspector");
                ui.separator();

                ui.small(format!("ID: {}", obj.id));
                ui.add_space(4.0);

                // Editable name
                let name_changed = ui
                    .horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut obj.name).changed()
                    })
                    .inner;

                // Editable position
                let mut px = obj.position.x;
                let mut py = obj.position.y;
                let mut pz = obj.position.z;
                let pos_changed = ui
                    .horizontal(|ui| {
                        ui.label("Pos:");
                        let x = ui
                            .add(egui::DragValue::new(&mut px).speed(0.1).prefix("x:"))
                            .changed();
                        let y = ui
                            .add(egui::DragValue::new(&mut py).speed(0.1).prefix("y:"))
                            .changed();
                        let z = ui
                            .add(egui::DragValue::new(&mut pz).speed(0.1).prefix("z:"))
                            .changed();
                        x || y || z
                    })
                    .inner;
                obj.position = Vec3::new(px, py, pz);

                // Editable scale (uniform)
                let mut sx = obj.scale.x;
                let mut sy = obj.scale.y;
                let mut sz = obj.scale.z;
                let scale_changed = ui
                    .horizontal(|ui| {
                        ui.label("Scale:");
                        let x = ui
                            .add(
                                egui::DragValue::new(&mut sx)
                                    .speed(0.05)
                                    .prefix("x:")
                                    .range(0.01..=100.0),
                            )
                            .changed();
                        let y = ui
                            .add(
                                egui::DragValue::new(&mut sy)
                                    .speed(0.05)
                                    .prefix("y:")
                                    .range(0.01..=100.0),
                            )
                            .changed();
                        let z = ui
                            .add(
                                egui::DragValue::new(&mut sz)
                                    .speed(0.05)
                                    .prefix("z:")
                                    .range(0.01..=100.0),
                            )
                            .changed();
                        x || y || z
                    })
                    .inner;
                obj.scale = Vec3::new(sx, sy, sz);

                // Model URL (read-only for now)
                if let Some(url) = &obj.model_url {
                    ui.add_space(4.0);
                    ui.small(format!("Model: {}", url));
                }

                if name_changed || pos_changed || scale_changed {
                    state.dirty = true;
                }

                ui.add_space(8.0);
                if ui.button("Delete Object").clicked() {
                    action = Some(NostrverseAction::RemoveObject(selected_id));
                }
            }

            // --- Save button ---
            ui.add_space(12.0);
            ui.separator();
            let save_label = if state.dirty { "Save *" } else { "Save" };
            if ui
                .add_enabled(state.dirty, egui::Button::new(save_label))
                .clicked()
            {
                action = Some(NostrverseAction::SaveRoom);
            }
        });
    });

    action
}
