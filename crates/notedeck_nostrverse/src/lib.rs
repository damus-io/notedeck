//! Nostrverse: Virtual rooms as Nostr events
//!
//! This app implements spatial views for nostrverse - a protocol where
//! rooms and objects are Nostr events (kinds 37555, 37556, 10555).
//!
//! Rooms are rendered as 3D scenes using renderbud's PBR pipeline,
//! embedded in egui via wgpu paint callbacks.

mod room_state;
mod room_view;

pub use room_state::{
    NostrverseAction, NostrverseState, Room, RoomObject, RoomRef, RoomShape, RoomUser,
};
pub use room_view::{NostrverseResponse, render_inspection_panel, show_room_view};

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
    /// Whether the app has been initialized with demo data
    initialized: bool,
    /// Cached avatar model AABB for ground placement
    avatar_bounds: Option<renderbud::Aabb>,
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

    /// Initialize with demo data (for testing)
    fn init_demo_data(&mut self) {
        if self.initialized {
            return;
        }

        // Set up demo room
        self.state.room = Some(Room {
            name: "Demo Room".to_string(),
            shape: RoomShape::Rectangle,
            width: 20.0,
            height: 15.0,
            depth: 10.0,
        });

        // Load test models from disk
        let bottle = self.load_model("/home/jb55/var/models/WaterBottle.glb");
        let ironwood = self.load_model("/home/jb55/var/models/ironwood/ironwood.glb");

        // Query AABBs for placement
        let renderer = self.renderer.as_ref();
        let model_bounds = |m: Option<renderbud::Model>| -> Option<renderbud::Aabb> {
            let r = renderer?.renderer.lock().unwrap();
            r.model_bounds(m?)
        };

        let table_bounds = model_bounds(ironwood);
        let bottle_bounds = model_bounds(bottle);

        // Table top Y (in model space, 1 unit = 1 meter)
        let table_top_y = table_bounds.map(|b| b.max.y).unwrap_or(0.86);
        // Bottle half-height (real-world scale, ~0.26m tall)
        let bottle_half_h = bottle_bounds
            .map(|b| (b.max.y - b.min.y) * 0.5)
            .unwrap_or(0.0);

        // Ironwood (table) at origin
        let mut obj1 = RoomObject::new(
            "obj1".to_string(),
            "Ironwood Table".to_string(),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .with_scale(Vec3::splat(1.0));
        obj1.model_handle = ironwood;

        // Water bottle on top of the table: table_top + half bottle height
        let mut obj2 = RoomObject::new(
            "obj2".to_string(),
            "Water Bottle".to_string(),
            Vec3::new(0.0, table_top_y + bottle_half_h, 0.0),
        )
        .with_scale(Vec3::splat(1.0));
        obj2.model_handle = bottle;

        self.state.objects = vec![obj1, obj2];

        // Add self user
        self.state.users = vec![
            RoomUser::new(
                demo_pubkey(),
                "jb55".to_string(),
                Vec3::new(-2.0, 0.0, -2.0),
            )
            .with_self(true),
        ];

        // Assign the bottle model as avatar placeholder for all users
        if let Some(model) = bottle {
            for user in &mut self.state.users {
                user.model_handle = Some(model);
            }
        }
        self.avatar_bounds = bottle_bounds;

        // Switch to third-person camera mode centered on the self-user
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
    fn update(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // Initialize demo data on first frame
        self.init_demo_data();

        // Sync state to 3D scene
        self.sync_scene();

        // Get available size before layout
        let available = ui.available_size();

        // Main layout with room view and optional inspection panel
        ui.allocate_ui(available, |ui| {
            ui.horizontal(|ui| {
                // Reserve space for panel if needed
                let room_width = if self.state.selected_object.is_some() {
                    available.x - 200.0
                } else {
                    available.x
                };

                ui.allocate_ui(egui::vec2(room_width, available.y), |ui| {
                    if let Some(renderer) = &self.renderer {
                        let response = show_room_view(ui, &mut self.state, renderer);

                        // Handle actions from room view
                        if let Some(action) = response.action {
                            match action {
                                NostrverseAction::MoveObject { id, position } => {
                                    tracing::info!("Object {} moved to {:?}", id, position);
                                }
                                NostrverseAction::SelectObject(selected) => {
                                    self.state.selected_object = selected;
                                }
                            }
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("3D rendering unavailable (no wgpu)");
                        });
                    }
                });

                // Inspection panel when object selected
                if self.state.selected_object.is_some() {
                    ui.allocate_ui(egui::vec2(200.0, available.y), |ui| {
                        if let Some(action) = render_inspection_panel(ui, &mut self.state)
                            && let NostrverseAction::SelectObject(None) = action
                        {
                            self.state.selected_object = None;
                        }
                    });
                }
            });
        });

        AppResponse::none()
    }
}
