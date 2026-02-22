//! Nostrverse: Virtual rooms as Nostr events
//!
//! This app implements spatial views for nostrverse - a protocol where
//! rooms and objects are Nostr events (kinds 37555, 37556, 10555).
//!
//! Rooms are rendered as 3D scenes using renderbud's PBR pipeline,
//! embedded in egui via wgpu paint callbacks.

mod convert;
mod nostr_events;
mod room_state;
mod room_view;
mod subscriptions;

pub use room_state::{
    NostrverseAction, NostrverseState, Room, RoomObject, RoomObjectType, RoomRef, RoomShape,
    RoomUser,
};
pub use room_view::{NostrverseResponse, render_editing_panel, show_room_view};

use enostr::Pubkey;
use glam::Vec3;
use notedeck::{AppContext, AppResponse};
use renderbud::Transform;

use egui_wgpu::wgpu;

/// Demo pubkey (jb55) used for testing
const DEMO_PUBKEY_HEX: &str = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245";
const FALLBACK_PUBKEY_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000001";

fn demo_pubkey() -> Pubkey {
    Pubkey::from_hex(DEMO_PUBKEY_HEX)
        .unwrap_or_else(|_| Pubkey::from_hex(FALLBACK_PUBKEY_HEX).unwrap())
}

/// Avatar scale: water bottle model is ~0.26m, scaled to human height (~1.8m)
const AVATAR_SCALE: f32 = 7.0;
/// How fast the avatar yaw lerps toward the target (higher = faster)
const AVATAR_YAW_LERP_SPEED: f32 = 10.0;

/// Demo room in protoverse .space format
const DEMO_SPACE: &str = r#"(room (name "Demo Room") (shape rectangle) (width 20) (height 15) (depth 10)
  (group
    (table (id obj1) (name "Ironwood Table")
           (model-url "/home/jb55/var/models/ironwood/ironwood.glb")
           (position 0 0 0))
    (prop (id obj2) (name "Water Bottle")
          (model-url "/home/jb55/var/models/WaterBottle.glb"))))"#;

/// Event kinds for nostrverse
pub mod kinds {
    /// Room event kind (addressable)
    pub const ROOM: u16 = 37555;
    /// Object event kind (addressable)
    pub const OBJECT: u16 = 37556;
    /// Presence event kind (user-replaceable)
    pub const PRESENCE: u16 = 10555;
}

/// Nostrverse app - a 3D spatial canvas for virtual rooms
pub struct NostrverseApp {
    /// Current room state
    state: NostrverseState,
    /// 3D renderer (None if wgpu unavailable)
    renderer: Option<renderbud::egui::EguiRenderer>,
    /// GPU device for model loading (Arc-wrapped internally by wgpu)
    device: Option<wgpu::Device>,
    /// GPU queue for model loading (Arc-wrapped internally by wgpu)
    queue: Option<wgpu::Queue>,
    /// Whether the app has been initialized
    initialized: bool,
    /// Cached avatar model AABB for ground placement
    avatar_bounds: Option<renderbud::Aabb>,
    /// Local nostrdb subscription for room events
    room_sub: Option<subscriptions::RoomSubscription>,
}

impl NostrverseApp {
    /// Create a new nostrverse app with a room reference
    pub fn new(room_ref: RoomRef, render_state: Option<&egui_wgpu::RenderState>) -> Self {
        let renderer = render_state.map(|rs| renderbud::egui::EguiRenderer::new(rs, (800, 600)));

        let device = render_state.map(|rs| rs.device.clone());
        let queue = render_state.map(|rs| rs.queue.clone());

        Self {
            state: NostrverseState::new(room_ref),
            renderer,
            device,
            queue,
            initialized: false,
            avatar_bounds: None,
            room_sub: None,
        }
    }

    /// Create with a demo room
    pub fn demo(render_state: Option<&egui_wgpu::RenderState>) -> Self {
        let room_ref = RoomRef::new("demo-room".to_string(), demo_pubkey());
        Self::new(room_ref, render_state)
    }

    /// Load a glTF model and return its handle
    fn load_model(&self, path: &str) -> Option<renderbud::Model> {
        let renderer = self.renderer.as_ref()?;
        let device = self.device.as_ref()?;
        let queue = self.queue.as_ref()?;
        let mut r = renderer.renderer.lock().unwrap();
        match r.load_gltf_model(device, queue, path) {
            Ok(model) => Some(model),
            Err(e) => {
                tracing::warn!("Failed to load model {}: {}", path, e);
                None
            }
        }
    }

    /// Initialize: ingest demo room into local nostrdb and subscribe.
    fn initialize(&mut self, ctx: &mut AppContext<'_>) {
        if self.initialized {
            return;
        }

        // Parse the demo room and ingest it as a local nostr event
        let space = match protoverse::parse(DEMO_SPACE) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to parse demo space: {}", e);
                return;
            }
        };

        // Ingest as a local-only room event if we have a keypair
        if let Some(kp) = ctx.accounts.selected_filled() {
            let builder = nostr_events::build_room_event(&space, &self.state.room_ref.id);
            nostr_events::ingest_room_event(builder, ctx.ndb, kp);
        }

        // Subscribe to room events in local nostrdb
        self.room_sub = Some(subscriptions::RoomSubscription::new(ctx.ndb));

        // Query for any existing room events (including the one we just ingested)
        let txn = nostrdb::Transaction::new(ctx.ndb).expect("txn");
        self.load_room_from_ndb(ctx.ndb, &txn);

        // Add self user
        let self_pubkey = *ctx.accounts.selected_account_pubkey();
        self.state.users = vec![
            RoomUser::new(self_pubkey, "jb55".to_string(), Vec3::new(-2.0, 0.0, -2.0))
                .with_self(true),
        ];

        // Assign avatar model (use first model with id "obj2" as placeholder)
        let avatar_model = self
            .state
            .objects
            .iter()
            .find(|o| o.id == "obj2")
            .and_then(|o| o.model_handle);
        let avatar_bounds = avatar_model.and_then(|m| {
            let renderer = self.renderer.as_ref()?;
            let r = renderer.renderer.lock().unwrap();
            r.model_bounds(m)
        });
        if let Some(model) = avatar_model {
            for user in &mut self.state.users {
                user.model_handle = Some(model);
            }
        }
        self.avatar_bounds = avatar_bounds;

        // Switch to third-person camera mode
        if let Some(renderer) = &self.renderer {
            let self_pos = self
                .state
                .users
                .iter()
                .find(|u| u.is_self)
                .map(|u| u.position)
                .unwrap_or(Vec3::ZERO);
            let mut r = renderer.renderer.lock().unwrap();
            r.set_third_person_mode(self_pos);
        }

        self.initialized = true;
    }

    /// Apply a parsed Space to the room state: convert, load models, update state.
    fn apply_space(&mut self, space: &protoverse::Space) {
        let (room, mut objects) = convert::convert_space(space);
        self.state.room = Some(room);
        self.load_object_models(&mut objects);
        self.state.objects = objects;
        self.state.dirty = false;
    }

    /// Load room state from a nostrdb query result.
    fn load_room_from_ndb(&mut self, ndb: &nostrdb::Ndb, txn: &nostrdb::Transaction) {
        let notes = subscriptions::RoomSubscription::query_existing(ndb, txn);

        for note in &notes {
            let Some(room_id) = nostr_events::get_room_id(note) else {
                continue;
            };
            if room_id != self.state.room_ref.id {
                continue;
            }

            let Some(space) = nostr_events::parse_room_event(note) else {
                tracing::warn!("Failed to parse room event content");
                continue;
            };

            self.apply_space(&space);
            tracing::info!("Loaded room '{}' from nostrdb", room_id);
            return;
        }
    }

    /// Save current room state: build Space, serialize, ingest as new nostr event.
    fn save_room(&self, ctx: &mut AppContext<'_>) {
        let Some(room) = &self.state.room else {
            tracing::warn!("save_room: no room to save");
            return;
        };
        let Some(kp) = ctx.accounts.selected_filled() else {
            tracing::warn!("save_room: no keypair available");
            return;
        };

        let space = convert::build_space(room, &self.state.objects);
        let builder = nostr_events::build_room_event(&space, &self.state.room_ref.id);
        nostr_events::ingest_room_event(builder, ctx.ndb, kp);
        tracing::info!("Saved room '{}'", self.state.room_ref.id);
    }

    /// Load 3D models for objects and handle AABB-based placement.
    fn load_object_models(&self, objects: &mut Vec<RoomObject>) {
        let renderer = self.renderer.as_ref();
        let model_bounds_fn = |m: Option<renderbud::Model>| -> Option<renderbud::Aabb> {
            let r = renderer?.renderer.lock().unwrap();
            r.model_bounds(m?)
        };

        let mut table_top_y: f32 = 0.86;
        let mut bottle_bounds = None;

        for obj in objects.iter_mut() {
            if let Some(url) = &obj.model_url {
                let model = self.load_model(url);
                let bounds = model_bounds_fn(model);

                if obj.id == "obj1" {
                    if let Some(b) = bounds {
                        table_top_y = b.max.y;
                    }
                }

                if obj.id == "obj2" {
                    bottle_bounds = bounds;
                }

                obj.model_handle = model;
            }
        }

        // Position the bottle on top of the table (runtime AABB placement)
        if let Some(obj2) = objects.iter_mut().find(|o| o.id == "obj2") {
            let bottle_half_h = bottle_bounds
                .map(|b| (b.max.y - b.min.y) * 0.5)
                .unwrap_or(0.0);
            obj2.position = Vec3::new(0.0, table_top_y + bottle_half_h, 0.0);
        }
    }

    /// Poll the room subscription for updates.
    /// Skips applying updates while the room has unsaved local edits.
    fn poll_room_updates(&mut self, ndb: &nostrdb::Ndb) {
        if self.state.dirty {
            return;
        }
        let Some(sub) = &self.room_sub else {
            return;
        };
        let txn = nostrdb::Transaction::new(ndb).expect("txn");
        let notes = sub.poll(ndb, &txn);

        for note in &notes {
            let Some(room_id) = nostr_events::get_room_id(note) else {
                continue;
            };
            if room_id != self.state.room_ref.id {
                continue;
            }

            let Some(space) = nostr_events::parse_room_event(note) else {
                continue;
            };

            self.apply_space(&space);
            tracing::info!("Room '{}' updated from nostrdb", room_id);
        }
    }

    /// Sync room objects and user avatars to the renderbud scene
    fn sync_scene(&mut self) {
        let Some(renderer) = &self.renderer else {
            return;
        };
        let mut r = renderer.renderer.lock().unwrap();

        // Sync room objects
        for obj in &mut self.state.objects {
            let transform = Transform {
                translation: obj.position,
                rotation: obj.rotation,
                scale: obj.scale,
            };

            if let Some(scene_id) = obj.scene_object_id {
                r.update_object_transform(scene_id, transform);
            } else if let Some(model) = obj.model_handle {
                let scene_id = r.place_object(model, transform);
                obj.scene_object_id = Some(scene_id);
            }
        }

        // Read avatar position/yaw from the third-person controller
        let avatar_pos = r.avatar_position();
        let avatar_yaw = r.avatar_yaw();

        // Update self-user's position from the controller
        if let Some(pos) = avatar_pos {
            if let Some(self_user) = self.state.users.iter_mut().find(|u| u.is_self) {
                self_user.position = pos;
            }
        }

        // Sync all user avatars to the scene
        let avatar_half_h = self
            .avatar_bounds
            .map(|b| (b.max.y - b.min.y) * 0.5)
            .unwrap_or(0.0);
        let avatar_y_offset = avatar_half_h * AVATAR_SCALE;

        // Smoothly lerp avatar yaw toward target
        if let Some(target_yaw) = avatar_yaw {
            let current = self.state.smooth_avatar_yaw;
            let mut diff = target_yaw - current;
            diff = (diff + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            let dt = 1.0 / 60.0;
            let t = (AVATAR_YAW_LERP_SPEED * dt).min(1.0);
            self.state.smooth_avatar_yaw = current + diff * t;
        }

        for user in &mut self.state.users {
            let yaw = if user.is_self {
                self.state.smooth_avatar_yaw
            } else {
                0.0
            };

            let transform = Transform {
                translation: user.position + Vec3::new(0.0, avatar_y_offset, 0.0),
                rotation: glam::Quat::from_rotation_y(yaw),
                scale: Vec3::splat(AVATAR_SCALE),
            };

            if let Some(scene_id) = user.scene_object_id {
                r.update_object_transform(scene_id, transform);
            } else if let Some(model) = user.model_handle {
                let scene_id = r.place_object(model, transform);
                user.scene_object_id = Some(scene_id);
            }
        }
    }

    /// Get the current state
    pub fn state(&self) -> &NostrverseState {
        &self.state
    }

    /// Get mutable state
    pub fn state_mut(&mut self) -> &mut NostrverseState {
        &mut self.state
    }
}

impl notedeck::App for NostrverseApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // Initialize on first frame
        self.initialize(ctx);

        // Poll for room event updates
        self.poll_room_updates(ctx.ndb);

        // Sync state to 3D scene
        self.sync_scene();

        // Get available size before layout
        let available = ui.available_size();
        let panel_width = 240.0;

        // Main layout: 3D view + editing panel
        ui.allocate_ui(available, |ui| {
            ui.horizontal(|ui| {
                let room_width = if self.state.edit_mode {
                    available.x - panel_width
                } else {
                    available.x
                };

                ui.allocate_ui(egui::vec2(room_width, available.y), |ui| {
                    if let Some(renderer) = &self.renderer {
                        let response = show_room_view(ui, &mut self.state, renderer);

                        if let Some(action) = response.action {
                            self.handle_action(action, ctx);
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("3D rendering unavailable (no wgpu)");
                        });
                    }
                });

                // Editing panel (always visible in edit mode)
                if self.state.edit_mode {
                    ui.allocate_ui(egui::vec2(panel_width, available.y), |ui| {
                        if let Some(action) = render_editing_panel(ui, &mut self.state) {
                            self.handle_action(action, ctx);
                        }
                    });
                }
            });
        });

        AppResponse::none()
    }
}

impl NostrverseApp {
    fn handle_action(&mut self, action: NostrverseAction, ctx: &mut AppContext<'_>) {
        match action {
            NostrverseAction::MoveObject { id, position } => {
                if let Some(obj) = self.state.get_object_mut(&id) {
                    obj.position = position;
                    self.state.dirty = true;
                }
            }
            NostrverseAction::SelectObject(selected) => {
                self.state.selected_object = selected;
            }
            NostrverseAction::SaveRoom => {
                self.save_room(ctx);
                self.state.dirty = false;
            }
            NostrverseAction::AddObject(obj) => {
                self.state.objects.push(obj);
                self.state.dirty = true;
            }
            NostrverseAction::RemoveObject(id) => {
                self.state.objects.retain(|o| o.id != id);
                if self.state.selected_object.as_ref() == Some(&id) {
                    self.state.selected_object = None;
                }
                self.state.dirty = true;
            }
        }
    }
}
