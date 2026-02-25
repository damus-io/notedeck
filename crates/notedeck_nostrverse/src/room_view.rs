//! Space 3D rendering and editing UI for nostrverse via renderbud

use egui::{Color32, Pos2, Rect, Response, Sense, Ui};
use glam::{Quat, Vec3};

use super::convert;
use super::room_state::{
    DragMode, DragState, NostrverseAction, NostrverseState, ObjectLocation, RoomObject,
};

/// Radians of Y rotation per pixel of horizontal drag
const ROTATE_SENSITIVITY: f32 = 0.01;

/// Response from rendering the nostrverse view
pub struct NostrverseResponse {
    pub response: Response,
    pub action: Option<NostrverseAction>,
}

fn snap_to_grid(pos: Vec3, grid: f32) -> Vec3 {
    Vec3::new(
        (pos.x / grid).round() * grid,
        pos.y,
        (pos.z / grid).round() * grid,
    )
}

/// Result of computing a drag update — fully owned, no borrows into state.
enum DragUpdate {
    Move {
        id: String,
        position: Vec3,
    },
    Breakaway {
        id: String,
        world_pos: Vec3,
        new_grab_offset: Vec3,
        new_plane_y: f32,
    },
    SnapToParent {
        id: String,
        parent_id: String,
        parent_scene_id: renderbud::ObjectId,
        parent_aabb: renderbud::Aabb,
        local_pos: Vec3,
        local_y: f32,
        plane_y: f32,
        new_grab_offset: Vec3,
    },
}

/// During a free drag, check if the world position lands on another object's
/// top surface. Returns snap info if a suitable parent is found.
/// Takes viewport coords to re-unproject onto the new drag plane for a
/// smooth grab-offset transition.
fn find_snap_parent(
    world_pos: Vec3,
    drag_id: &str,
    child_half_h: f32,
    vp_x: f32,
    vp_y: f32,
    objects: &[RoomObject],
    r: &renderbud::Renderer,
) -> Option<DragUpdate> {
    for obj in objects {
        if obj.id == drag_id {
            continue;
        }
        let Some(scene_id) = obj.scene_object_id else {
            continue;
        };
        let Some(model) = obj.model_handle else {
            continue;
        };
        let Some(aabb) = r.model_bounds(model) else {
            continue;
        };
        let Some(parent_world) = r.world_matrix(scene_id) else {
            continue;
        };
        let inv_parent = parent_world.inverse();
        let local_hit = inv_parent.transform_point3(world_pos);

        // Check if XZ is within the parent's AABB
        if aabb.xz_overshoot(local_hit) < 0.01 {
            let local_y = aabb.max.y + child_half_h;
            let local_pos = aabb.clamp_xz(Vec3::new(local_hit.x, local_y, local_hit.z));
            let snapped_world = parent_world.transform_point3(local_pos);
            let plane_y = snapped_world.y;

            // Compute grab offset so the object doesn't jump:
            // re-unproject cursor onto the new (higher) drag plane,
            // then compute offset in parent-local space.
            let grab_offset = if let Some(new_hit) = r.unproject_to_plane(vp_x, vp_y, plane_y) {
                let new_local = inv_parent.transform_point3(new_hit);
                Vec3::new(local_pos.x - new_local.x, 0.0, local_pos.z - new_local.z)
            } else {
                Vec3::ZERO
            };

            return Some(DragUpdate::SnapToParent {
                id: drag_id.to_string(),
                parent_id: obj.id.clone(),
                parent_scene_id: scene_id,
                parent_aabb: aabb,
                local_pos,
                local_y,
                plane_y,
                new_grab_offset: grab_offset,
            });
        }
    }
    None
}

/// Pure computation: given current drag state and pointer, decide what to do.
fn compute_drag_update(
    drag: &DragState,
    vp_x: f32,
    vp_y: f32,
    grid_snap: Option<f32>,
    r: &renderbud::Renderer,
) -> Option<DragUpdate> {
    match &drag.mode {
        DragMode::Free => {
            let hit = r.unproject_to_plane(vp_x, vp_y, drag.plane_y)?;
            let mut new_pos = hit + drag.grab_offset;
            if let Some(grid) = grid_snap {
                new_pos = snap_to_grid(new_pos, grid);
            }
            Some(DragUpdate::Move {
                id: drag.object_id.clone(),
                position: new_pos,
            })
        }
        DragMode::Parented {
            parent_scene_id,
            parent_aabb,
            local_y,
            ..
        } => {
            let hit = r.unproject_to_plane(vp_x, vp_y, drag.plane_y)?;
            let parent_world = r.world_matrix(*parent_scene_id)?;
            let local_hit = parent_world.inverse().transform_point3(hit);
            let mut local_pos = Vec3::new(
                local_hit.x + drag.grab_offset.x,
                *local_y,
                local_hit.z + drag.grab_offset.z,
            );
            if let Some(grid) = grid_snap {
                local_pos = snap_to_grid(local_pos, grid);
            }

            if parent_aabb.xz_overshoot(local_pos) > 1.0 {
                let world_pos = parent_world.transform_point3(local_pos);
                Some(DragUpdate::Breakaway {
                    id: drag.object_id.clone(),
                    world_pos,
                    new_grab_offset: world_pos - hit,
                    new_plane_y: world_pos.y,
                })
            } else {
                Some(DragUpdate::Move {
                    id: drag.object_id.clone(),
                    position: parent_aabb.clamp_xz(local_pos),
                })
            }
        }
    }
}

/// Try to start an object drag. Returns the action (selection) if an object was picked.
fn handle_drag_start(
    state: &mut NostrverseState,
    vp_x: f32,
    vp_y: f32,
    r: &mut renderbud::Renderer,
) -> Option<NostrverseAction> {
    let scene_id = r.pick(vp_x, vp_y)?;
    let obj = state
        .objects
        .iter()
        .find(|o| o.scene_object_id == Some(scene_id))?;

    // Always select on drag start
    r.set_selected(Some(scene_id));
    state.selected_object = Some(obj.id.clone());

    // In rotate mode, mark this as a rotation drag (don't start a position drag)
    let drag_info = if state.rotate_mode {
        state.rotate_drag = true;
        None
    } else {
        compute_initial_drag(obj, state, vp_x, vp_y, r)
    };

    if let Some((mode, grab_offset, plane_y)) = drag_info {
        state.drag_state = Some(DragState {
            object_id: obj.id.clone(),
            grab_offset,
            plane_y,
            mode,
        });
    }
    None
}

/// Compute the initial drag mode and grab offset for an object.
fn compute_initial_drag(
    obj: &RoomObject,
    state: &NostrverseState,
    vp_x: f32,
    vp_y: f32,
    r: &renderbud::Renderer,
) -> Option<(DragMode, Vec3, f32)> {
    match &obj.location {
        Some(ObjectLocation::TopOf(parent_id)) | Some(ObjectLocation::Near(parent_id)) => {
            let parent = state.objects.iter().find(|o| o.id == *parent_id)?;
            let parent_scene_id = parent.scene_object_id?;
            let parent_aabb = r.model_bounds(parent.model_handle?)?;
            let parent_world = r.world_matrix(parent_scene_id)?;

            let child_half_h = obj
                .model_handle
                .and_then(|m| r.model_bounds(m))
                .map(|b| (b.max.y - b.min.y) * 0.5)
                .unwrap_or(0.0);
            let local_y = if matches!(&obj.location, Some(ObjectLocation::TopOf(_))) {
                parent_aabb.max.y + child_half_h
            } else {
                0.0
            };
            let obj_world = parent_world.transform_point3(obj.position);
            let plane_y = obj_world.y;
            let hit = r
                .unproject_to_plane(vp_x, vp_y, plane_y)
                .unwrap_or(obj_world);
            let local_hit = parent_world.inverse().transform_point3(hit);
            let grab_offset = obj.position - local_hit;
            Some((
                DragMode::Parented {
                    parent_id: parent_id.clone(),
                    parent_scene_id,
                    parent_aabb,
                    local_y,
                },
                grab_offset,
                plane_y,
            ))
        }
        None | Some(ObjectLocation::Floor) => {
            let plane_y = obj.position.y;
            let hit = r
                .unproject_to_plane(vp_x, vp_y, plane_y)
                .unwrap_or(obj.position);
            let grab_offset = obj.position - hit;
            Some((DragMode::Free, grab_offset, plane_y))
        }
        _ => None, // Center/Ceiling/Custom: not draggable
    }
}

/// Apply a computed drag update to state and renderer.
fn apply_drag_update(
    update: DragUpdate,
    state: &mut NostrverseState,
    r: &mut renderbud::Renderer,
) -> Option<NostrverseAction> {
    match update {
        DragUpdate::Move { id, position } => Some(NostrverseAction::MoveObject { id, position }),
        DragUpdate::Breakaway {
            id,
            world_pos,
            new_grab_offset,
            new_plane_y,
        } => {
            if let Some(obj) = state.objects.iter_mut().find(|o| o.id == id) {
                if let Some(sid) = obj.scene_object_id {
                    r.set_parent(sid, None);
                }
                obj.position = world_pos;
                obj.location = None;
                obj.location_base = None;
                state.dirty = true;
            }
            state.drag_state = Some(DragState {
                object_id: id,
                grab_offset: new_grab_offset,
                plane_y: new_plane_y,
                mode: DragMode::Free,
            });
            None
        }
        DragUpdate::SnapToParent {
            id,
            parent_id,
            parent_scene_id,
            parent_aabb,
            local_pos,
            local_y,
            plane_y,
            new_grab_offset,
        } => {
            if let Some(obj) = state.objects.iter_mut().find(|o| o.id == id) {
                if let Some(sid) = obj.scene_object_id {
                    r.set_parent(sid, Some(parent_scene_id));
                }
                obj.position = local_pos;
                obj.location = Some(ObjectLocation::TopOf(parent_id.clone()));
                obj.location_base = Some(Vec3::new(0.0, local_y, 0.0));
                state.dirty = true;
            }
            state.drag_state = Some(DragState {
                object_id: id,
                grab_offset: new_grab_offset,
                plane_y,
                mode: DragMode::Parented {
                    parent_id,
                    parent_scene_id,
                    parent_aabb,
                    local_y,
                },
            });
            None
        }
    }
}

/// Handle keyboard shortcuts and WASD movement. Returns an action if triggered.
fn handle_keyboard_input(
    ui: &Ui,
    state: &mut NostrverseState,
    r: &mut renderbud::Renderer,
) -> Option<NostrverseAction> {
    let mut action = None;

    // G key: toggle grid snap
    if ui.input(|i| i.key_pressed(egui::Key::G)) {
        state.grid_snap_enabled = !state.grid_snap_enabled;
    }

    // R key: toggle rotate mode
    if ui.input(|i| i.key_pressed(egui::Key::R)) {
        state.rotate_mode = !state.rotate_mode;
    }

    // Ctrl+D: duplicate selected object
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::D))
        && let Some(id) = state.selected_object.clone()
    {
        action = Some(NostrverseAction::DuplicateObject(id));
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

    action
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
                if let Some(a) = handle_drag_start(state, vp.x, vp.y, &mut r) {
                    action = Some(a);
                }
            }

            // Dragging: rotate or move object, or control camera
            if response.dragged() {
                // Rotation drag: only when drag started on an object in rotate mode
                if state.rotate_drag
                    && let Some(sel_id) = state.selected_object.clone()
                    && let Some(obj) = state.objects.iter().find(|o| o.id == sel_id)
                {
                    let delta_x = response.drag_delta().x;
                    let angle = delta_x * ROTATE_SENSITIVITY;
                    let new_rotation = Quat::from_rotation_y(angle) * obj.rotation;
                    let new_rotation = if state.grid_snap_enabled {
                        let (_, y, _) = new_rotation.to_euler(glam::EulerRot::YXZ);
                        let snap_rad = state.rotation_snap.to_radians();
                        let snapped_y = (y / snap_rad).round() * snap_rad;
                        Quat::from_rotation_y(snapped_y)
                    } else {
                        new_rotation
                    };
                    action = Some(NostrverseAction::RotateObject {
                        id: sel_id,
                        rotation: new_rotation,
                    });
                    ui.ctx().request_repaint();
                } else if let Some(drag) = state.drag_state.as_ref() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let vp = pos - rect.min.to_vec2();
                        let grid = state.grid_snap_enabled.then_some(state.grid_snap);
                        let update = compute_drag_update(drag, vp.x, vp.y, grid, &r);
                        // For free drags, check if we should snap to a parent
                        let update = if let Some(DragUpdate::Move {
                            ref id,
                            ref position,
                        }) = update
                        {
                            if matches!(
                                state.drag_state.as_ref().map(|d| &d.mode),
                                Some(DragMode::Free)
                            ) {
                                let child_half_h = state
                                    .objects
                                    .iter()
                                    .find(|o| o.id == *id)
                                    .and_then(|o| o.model_handle)
                                    .and_then(|m| r.model_bounds(m))
                                    .map(|b| (b.max.y - b.min.y) * 0.5)
                                    .unwrap_or(0.0);
                                find_snap_parent(
                                    *position,
                                    id,
                                    child_half_h,
                                    vp.x,
                                    vp.y,
                                    &state.objects,
                                    &r,
                                )
                                .or(update)
                            } else {
                                update
                            }
                        } else {
                            update
                        };

                        if let Some(update) = update
                            && let Some(a) = apply_drag_update(update, state, &mut r)
                        {
                            action = Some(a);
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
                state.rotate_drag = false;
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

        if let Some(a) = handle_keyboard_input(ui, state, &mut r) {
            action = Some(a);
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
    let space_name = state
        .space
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("Loading...");

    let mut info_text = format!("{} | Objects: {}", space_name, state.objects.len());
    if state.rotate_mode {
        info_text.push_str(" | Rotate (R)");
    }

    // Measure text to size the background
    let font_id = egui::FontId::proportional(14.0);
    let text_pos = Pos2::new(rect.left() + 10.0, rect.top() + 10.0);
    let galley = painter.layout_no_wrap(
        info_text,
        font_id,
        Color32::from_rgba_unmultiplied(200, 200, 210, 220),
    );
    let padding = egui::vec2(12.0, 6.0);
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, rect.top() + 4.0),
            galley.size() + padding,
        ),
        4.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 160),
    );
    painter.galley(text_pos, galley, Color32::PLACEHOLDER);
}

/// Render the object list and add-object button. Returns an action if triggered.
fn render_object_list(ui: &mut Ui, state: &NostrverseState) -> Option<NostrverseAction> {
    ui.strong("Objects");
    ui.separator();

    let mut action = None;
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

    action
}

/// Render the object inspector panel for the selected object.
/// Returns an action and whether any property changed.
fn render_object_inspector(
    ui: &mut Ui,
    selected_id: &str,
    obj: &mut RoomObject,
    grid_snap_enabled: bool,
    rotation_snap: f32,
) -> (Option<NostrverseAction>, bool) {
    let mut action = None;

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

    // Editable scale
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

    // Editable Y rotation (degrees)
    let (_, angle_y, _) = obj.rotation.to_euler(glam::EulerRot::YXZ);
    let mut deg = angle_y.to_degrees();
    let rot_changed = ui
        .horizontal(|ui| {
            ui.label("Rot Y:");
            let speed = if grid_snap_enabled {
                rotation_snap
            } else {
                1.0
            };
            ui.add(egui::DragValue::new(&mut deg).speed(speed).suffix("°"))
                .changed()
        })
        .inner;
    if rot_changed {
        if grid_snap_enabled {
            deg = (deg / rotation_snap).round() * rotation_snap;
        }
        obj.rotation = Quat::from_rotation_y(deg.to_radians());
    }

    // Model URL (read-only for now)
    if let Some(url) = &obj.model_url {
        ui.add_space(4.0);
        ui.small(format!("Model: {}", url));
    }

    let changed = name_changed || pos_changed || scale_changed || rot_changed;

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Duplicate").clicked() {
            action = Some(NostrverseAction::DuplicateObject(selected_id.to_owned()));
        }
        if ui.button("Delete").clicked() {
            action = Some(NostrverseAction::RemoveObject(selected_id.to_owned()));
        }
    });

    (action, changed)
}

/// Render grid snap and rotation snap controls.
fn render_grid_snap_controls(ui: &mut Ui, state: &mut NostrverseState) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.grid_snap_enabled, "Grid Snap (G)");
        if state.grid_snap_enabled {
            ui.add(
                egui::DragValue::new(&mut state.grid_snap)
                    .speed(0.05)
                    .range(0.05..=10.0)
                    .suffix("m"),
            );
        }
    });
    if state.grid_snap_enabled {
        ui.horizontal(|ui| {
            ui.label("  Rot snap:");
            ui.add(
                egui::DragValue::new(&mut state.rotation_snap)
                    .speed(1.0)
                    .range(1.0..=90.0)
                    .suffix("°"),
            );
        });
    }
}

/// Render the syntax-highlighted scene source preview.
fn render_scene_preview(ui: &mut Ui, state: &mut NostrverseState) {
    // Only re-serialize when not actively dragging an object
    if state.drag_state.is_none()
        && let Some(info) = &state.space
    {
        let space = convert::build_space(info, &state.objects);
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
}

/// Render the side panel with space editing, object list, and object inspector.
pub fn render_editing_panel(ui: &mut Ui, state: &mut NostrverseState) -> Option<NostrverseAction> {
    let mut action = None;

    // --- Space Properties ---
    if let Some(info) = &mut state.space {
        ui.strong("Space");
        ui.separator();

        let name_changed = ui
            .horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut info.name).changed()
            })
            .inner;

        if name_changed {
            state.dirty = true;
        }

        ui.add_space(8.0);
    }

    // --- Object List ---
    if let Some(a) = render_object_list(ui, state) {
        action = Some(a);
    }

    ui.add_space(12.0);

    // --- Object Inspector ---
    if let Some(selected_id) = state.selected_object.clone()
        && let Some(obj) = state.objects.iter_mut().find(|o| o.id == selected_id)
    {
        let (inspector_action, changed) = render_object_inspector(
            ui,
            &selected_id,
            obj,
            state.grid_snap_enabled,
            state.rotation_snap,
        );
        if let Some(a) = inspector_action {
            action = Some(a);
        }
        if changed {
            state.dirty = true;
        }
    }

    // --- Grid Snap ---
    ui.add_space(8.0);
    render_grid_snap_controls(ui, state);

    // --- Save button ---
    ui.add_space(12.0);
    ui.separator();
    let save_label = if state.dirty { "Save *" } else { "Save" };
    if ui
        .add_enabled(state.dirty, egui::Button::new(save_label))
        .clicked()
    {
        action = Some(NostrverseAction::SaveSpace);
    }

    // --- Scene body ---
    render_scene_preview(ui, state);

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
            | "rotation"
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
