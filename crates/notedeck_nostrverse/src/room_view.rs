//! Room 3D rendering and editing UI for nostrverse via renderbud

use egui::{Color32, Pos2, Rect, Response, Sense, Ui};
use glam::Vec3;

use super::convert;
use super::room_state::{
    DragState, NostrverseAction, NostrverseState, ObjectLocation, RoomObject, RoomShape,
};

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

    let mut action: Option<NostrverseAction> = None;

    // Update renderer target size and handle input
    {
        let mut r = renderer.renderer.lock().unwrap();
        r.set_target_size((rect.width() as u32, rect.height() as u32));

        if state.edit_mode {
            // --- Edit mode: click-to-select, drag-to-move objects ---

            // Drag start: pick to decide object-drag vs camera
            if response.drag_started()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let vp = pos - rect.min.to_vec2();
                if let Some(scene_id) = r.pick(vp.x, vp.y)
                    && let Some(obj) = state
                        .objects
                        .iter()
                        .find(|o| o.scene_object_id == Some(scene_id))
                {
                    let can_drag = obj.location.is_none()
                        || matches!(obj.location, Some(ObjectLocation::Floor));
                    if can_drag {
                        let plane_y = obj.position.y;
                        let hit = r
                            .unproject_to_plane(vp.x, vp.y, plane_y)
                            .unwrap_or(obj.position);
                        state.drag_state = Some(DragState {
                            object_id: obj.id.clone(),
                            grab_offset: obj.position - hit,
                            plane_y,
                        });
                        action = Some(NostrverseAction::SelectObject(Some(obj.id.clone())));
                    }
                }
            }

            // Dragging: move object or control camera
            if response.dragged() {
                if let Some(ref drag) = state.drag_state {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let vp = pos - rect.min.to_vec2();
                        if let Some(hit) = r.unproject_to_plane(vp.x, vp.y, drag.plane_y) {
                            let new_pos = hit + drag.grab_offset;
                            action = Some(NostrverseAction::MoveObject {
                                id: drag.object_id.clone(),
                                position: new_pos,
                            });
                        }
                    }
                    ui.ctx().request_repaint();
                } else {
                    let delta = response.drag_delta();
                    r.on_mouse_drag(delta.x, delta.y);
                }
            }

            // Drag end: clear state
            if response.drag_stopped() {
                state.drag_state = None;
            }

            // Click (no drag): select/deselect
            if response.clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let vp = pos - rect.min.to_vec2();
                if let Some(scene_id) = r.pick(vp.x, vp.y) {
                    if let Some(obj) = state
                        .objects
                        .iter()
                        .find(|o| o.scene_object_id == Some(scene_id))
                    {
                        action = Some(NostrverseAction::SelectObject(Some(obj.id.clone())));
                    }
                } else {
                    action = Some(NostrverseAction::SelectObject(None));
                }
            }
        } else {
            // --- View mode: camera only ---
            if response.dragged() {
                let delta = response.drag_delta();
                r.on_mouse_drag(delta.x, delta.y);
            }
        }

        // Scroll: always routes to camera (zoom/speed)
        if response.hover_pos().is_some() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                r.on_scroll(scroll * 0.01);
            }
        }

        // WASD + QE movement: always available
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

    NostrverseResponse { response, action }
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
                            .selectable_value(&mut room.shape, RoomShape::Rectangle, "Rectangle")
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
            let selected = if is_selected {
                None
            } else {
                Some(state.objects[i].id.clone())
            };
            action = Some(NostrverseAction::SelectObject(selected));
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
    if let Some(selected_id) = state.selected_object.as_ref()
        && let Some(obj) = state.objects.iter_mut().find(|o| &o.id == selected_id)
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

        // Edit offset (relative to location base) or absolute position
        let base = obj.location_base.unwrap_or(Vec3::ZERO);
        let offset = obj.position - base;
        let mut ox = offset.x;
        let mut oy = offset.y;
        let mut oz = offset.z;
        let has_location = obj.location.is_some();
        let pos_label = if has_location { "Offset:" } else { "Pos:" };
        let pos_changed = ui
            .horizontal(|ui| {
                ui.label(pos_label);
                let x = ui
                    .add(egui::DragValue::new(&mut ox).speed(0.1).prefix("x:"))
                    .changed();
                let y = ui
                    .add(egui::DragValue::new(&mut oy).speed(0.1).prefix("y:"))
                    .changed();
                let z = ui
                    .add(egui::DragValue::new(&mut oz).speed(0.1).prefix("z:"))
                    .changed();
                x || y || z
            })
            .inner;
        obj.position = base + Vec3::new(ox, oy, oz);

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
            action = Some(NostrverseAction::RemoveObject(selected_id.to_owned()));
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

    // --- Scene body (syntax-highlighted, read-only) ---
    // Only re-serialize when not actively dragging an object
    if state.drag_state.is_none()
        && let Some(room) = &state.room
    {
        let space = convert::build_space(room, &state.objects);
        state.cached_scene_text = protoverse::serialize(&space);
    }

    ui.add_space(12.0);
    ui.strong("Scene");
    ui.separator();
    if !state.cached_scene_text.is_empty() {
        let layout_job = highlight_sexp(&state.cached_scene_text, ui);
        let code_bg = if ui.visuals().dark_mode {
            Color32::from_rgb(0x1E, 0x1C, 0x19)
        } else {
            Color32::from_rgb(0xF5, 0xF0, 0xEB)
        };
        egui::Frame::default()
            .fill(code_bg)
            .inner_margin(6.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.add(egui::Label::new(layout_job).wrap());
            });
    }

    action
}

// --- S-expression syntax highlighting ---

#[derive(Clone, Copy)]
enum SexpToken {
    Paren,
    Keyword,
    Symbol,
    String,
    Number,
    Whitespace,
}

/// Tokenize S-expression text for highlighting, preserving all characters.
fn tokenize_sexp(input: &str) -> Vec<(SexpToken, &str)> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'(' | b')' => {
                tokens.push((SexpToken::Paren, &input[i..i + 1]));
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // closing quote
                }
                tokens.push((SexpToken::String, &input[start..i]));
            }
            c if c.is_ascii_whitespace() => {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                tokens.push((SexpToken::Whitespace, &input[start..i]));
            }
            c if c.is_ascii_digit()
                || (c == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) =>
            {
                while i < bytes.len()
                    && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'-')
                {
                    i += 1;
                }
                tokens.push((SexpToken::Number, &input[start..i]));
            }
            c if c.is_ascii_alphabetic() || c == b'-' || c == b'_' => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
                {
                    i += 1;
                }
                let word = &input[start..i];
                let kind = if is_sexp_keyword(word) {
                    SexpToken::Keyword
                } else {
                    SexpToken::Symbol
                };
                tokens.push((kind, word));
            }
            _ => {
                tokens.push((SexpToken::Symbol, &input[i..i + 1]));
                i += 1;
            }
        }
    }
    tokens
}

fn is_sexp_keyword(word: &str) -> bool {
    matches!(
        word,
        "room"
            | "group"
            | "table"
            | "chair"
            | "door"
            | "light"
            | "prop"
            | "name"
            | "id"
            | "shape"
            | "width"
            | "height"
            | "depth"
            | "position"
            | "location"
            | "model-url"
            | "material"
            | "condition"
            | "state"
            | "type"
    )
}

/// Build a syntax-highlighted LayoutJob from S-expression text.
fn highlight_sexp(code: &str, ui: &Ui) -> egui::text::LayoutJob {
    let font_id = ui
        .style()
        .override_font_id
        .clone()
        .unwrap_or_else(|| egui::TextStyle::Monospace.resolve(ui.style()));

    let dark = ui.visuals().dark_mode;

    let paren_color = if dark {
        Color32::from_rgb(0xA0, 0x96, 0x88)
    } else {
        Color32::from_rgb(0x6E, 0x64, 0x56)
    };
    let keyword_color = if dark {
        Color32::from_rgb(0xD4, 0xA5, 0x74)
    } else {
        Color32::from_rgb(0x9A, 0x60, 0x2A)
    };
    let symbol_color = if dark {
        Color32::from_rgb(0xD5, 0xCE, 0xC4)
    } else {
        Color32::from_rgb(0x3A, 0x35, 0x2E)
    };
    let string_color = if dark {
        Color32::from_rgb(0xC6, 0xB4, 0x6A)
    } else {
        Color32::from_rgb(0x6B, 0x5C, 0x1A)
    };
    let number_color = if dark {
        Color32::from_rgb(0xC4, 0x8A, 0x6A)
    } else {
        Color32::from_rgb(0x8B, 0x4C, 0x30)
    };

    let mut job = egui::text::LayoutJob::default();
    for (token, text) in tokenize_sexp(code) {
        let color = match token {
            SexpToken::Paren => paren_color,
            SexpToken::Keyword => keyword_color,
            SexpToken::Symbol => symbol_color,
            SexpToken::String => string_color,
            SexpToken::Number => number_color,
            SexpToken::Whitespace => Color32::TRANSPARENT,
        };
        job.append(text, 0.0, egui::TextFormat::simple(font_id.clone(), color));
    }
    job
}
