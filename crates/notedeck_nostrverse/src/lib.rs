//! Nostrverse: Virtual spaces as Nostr events
//!
//! This app implements spatial views for nostrverse - a protocol where
//! spaces and objects are Nostr events (kinds 37555, 37556, 10555).
//!
//! Spaces are rendered as 3D scenes using renderbud's PBR pipeline,
//! embedded in egui via wgpu paint callbacks.

mod convert;
mod model_cache;
mod nostr_events;
mod presence;
mod room_state;
mod room_view;
mod subscriptions;

pub use room_state::{
    NostrverseAction, NostrverseState, RoomObject, RoomObjectType, RoomUser, SpaceInfo, SpaceRef,
};
pub use room_view::{NostrverseResponse, render_editing_panel, show_room_view};

use enostr::Pubkey;
use glam::Vec3;
use nostrdb::Filter;
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
/// How fast remote avatar position lerps toward extrapolated target
const AVATAR_POS_LERP_SPEED: f32 = 8.0;
/// Maximum extrapolation time (seconds) before clamping dead reckoning
const MAX_EXTRAPOLATION_TIME: f64 = 3.0;
/// Maximum extrapolation distance from last known position
const MAX_EXTRAPOLATION_DISTANCE: f32 = 10.0;

/// Demo space in protoverse .space format
const DEMO_SPACE: &str = r#"(space (name "Demo Space")
  (group
    (table (id obj1) (name "Ironwood Table")
           (model-url "/home/jb55/var/models/ironwood/ironwood.glb")
           (position 0 0 0))
    (prop (id obj2) (name "Water Bottle")
          (model-url "/home/jb55/var/models/WaterBottle.glb")
          (location top-of obj1))))"#;

/// Event kinds for nostrverse
pub mod kinds {
    /// Room event kind (addressable)
    pub const ROOM: u16 = 37555;
    /// Object event kind (addressable)
    pub const OBJECT: u16 = 37556;
    /// Presence event kind (user-replaceable)
    pub const PRESENCE: u16 = 10555;
}

/// Nostrverse app - a 3D spatial canvas for virtual spaces
pub struct NostrverseApp {
    /// Current space state
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
    /// Local nostrdb subscription for space events
    room_sub: Option<subscriptions::RoomSubscription>,
    /// Presence publisher (throttled heartbeats)
    presence_pub: presence::PresencePublisher,
    /// Presence expiry (throttled stale-user cleanup)
    presence_expiry: presence::PresenceExpiry,
    /// Local nostrdb subscription for presence events
    presence_sub: Option<subscriptions::PresenceSubscription>,
    /// Cached space naddr string (avoids format! per frame)
    space_naddr: String,
    /// Event ID of the last save we made (to skip our own echo in polls)
    last_save_id: Option<[u8; 32]>,
    /// Monotonic time tracker (seconds since app start)
    start_time: std::time::Instant,
    /// Model download/cache manager (initialized lazily in initialize())
    model_cache: Option<model_cache::ModelCache>,
    /// Dedicated relay URL for multiplayer sync (from NOSTRVERSE_RELAY env)
    relay_url: Option<String>,
    /// Pending relay subscription ID — Some means we still need to send REQ
    pending_relay_sub: Option<String>,
}

impl NostrverseApp {
    const DEFAULT_RELAY: &str = "ws://relay.jb55.com";

    /// Create a new nostrverse app with a space reference
    pub fn new(space_ref: SpaceRef, render_state: Option<&egui_wgpu::RenderState>) -> Self {
        let renderer = render_state.map(|rs| renderbud::egui::EguiRenderer::new(rs, (800, 600)));

        let device = render_state.map(|rs| rs.device.clone());
        let queue = render_state.map(|rs| rs.queue.clone());

        let relay_url = Some(
            std::env::var("NOSTRVERSE_RELAY").unwrap_or_else(|_| Self::DEFAULT_RELAY.to_string()),
        );

        let space_naddr = space_ref.to_naddr();
        Self {
            state: NostrverseState::new(space_ref),
            renderer,
            device,
            queue,
            initialized: false,
            avatar_bounds: None,
            room_sub: None,
            presence_pub: presence::PresencePublisher::new(),
            presence_expiry: presence::PresenceExpiry::new(),
            presence_sub: None,
            space_naddr,
            last_save_id: None,
            start_time: std::time::Instant::now(),
            model_cache: None,
            relay_url,
            pending_relay_sub: None,
        }
    }

    /// Create with a demo space
    pub fn demo(render_state: Option<&egui_wgpu::RenderState>) -> Self {
        let space_ref = SpaceRef::new("demo-room".to_string(), demo_pubkey());
        Self::new(space_ref, render_state)
    }

    /// Send a client message to the dedicated relay, if configured.
    fn send_to_relay(&self, pool: &mut enostr::RelayPool, msg: &enostr::ClientMessage) {
        if let Some(relay_url) = &self.relay_url {
            pool.send_to(msg, relay_url);
        }
    }

    /// Send the relay subscription once the relay is connected.
    fn maybe_send_relay_sub(&mut self, pool: &mut enostr::RelayPool) {
        let (Some(sub_id), Some(relay_url)) = (&self.pending_relay_sub, &self.relay_url) else {
            return;
        };

        let connected = pool
            .relays
            .iter()
            .any(|r| r.url() == relay_url && matches!(r.status(), enostr::RelayStatus::Connected));

        if !connected {
            return;
        }

        let room_filter = Filter::new().kinds([kinds::ROOM as u64]).build();
        let presence_filter = Filter::new().kinds([kinds::PRESENCE as u64]).build();

        let req = enostr::ClientMessage::req(sub_id.clone(), vec![room_filter, presence_filter]);
        pool.send_to(&req, relay_url);

        tracing::info!("Sent nostrverse subscription to {}", relay_url);
        self.pending_relay_sub = None;
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

    /// Initialize: ingest demo space into local nostrdb and subscribe.
    fn initialize(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        if self.initialized {
            return;
        }

        // Initialize model cache
        let cache_dir = ctx.path.path(notedeck::DataPathType::Cache).join("models");
        self.model_cache = Some(model_cache::ModelCache::new(cache_dir));

        // Subscribe to space and presence events in local nostrdb
        self.room_sub = Some(subscriptions::RoomSubscription::new(ctx.ndb));
        self.presence_sub = Some(subscriptions::PresenceSubscription::new(ctx.ndb));

        // Add dedicated relay to pool (subscription sent on connect in maybe_send_relay_sub)
        if let Some(relay_url) = &self.relay_url {
            let egui_ctx = egui_ctx.clone();
            if let Err(e) = ctx
                .pool
                .add_url(relay_url.clone(), move || egui_ctx.request_repaint())
            {
                tracing::error!("Failed to add nostrverse relay {}: {}", relay_url, e);
            } else {
                tracing::info!("Added nostrverse relay: {}", relay_url);
                self.pending_relay_sub = Some(format!("nostrverse-{}", uuid::Uuid::new_v4()));
            }
        }

        // Try to load an existing space from nostrdb first
        let txn = nostrdb::Transaction::new(ctx.ndb).expect("txn");
        self.load_space_from_ndb(ctx.ndb, &txn);

        // Only ingest the demo space if no saved space was found
        if self.state.space.is_none() {
            let space = match protoverse::parse(DEMO_SPACE) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to parse demo space: {}", e);
                    return;
                }
            };

            if let Some(kp) = ctx.accounts.selected_filled() {
                let builder = nostr_events::build_space_event(&space, &self.state.space_ref.id);
                if let Some((msg, _id)) = nostr_events::ingest_event(builder, ctx.ndb, kp) {
                    self.send_to_relay(ctx.pool, &msg);
                }
            }
            // room_sub (set up above) will pick up the ingested event
            // on the next poll_space_updates() frame.
        }

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
                .self_user()
                .map(|u| u.position)
                .unwrap_or(Vec3::ZERO);
            let mut r = renderer.renderer.lock().unwrap();
            r.set_third_person_mode(self_pos);
        }

        self.initialized = true;
    }

    /// Apply a parsed Space to the state: convert, load models, update state.
    /// Preserves renderer scene handles for objects that still exist by ID,
    /// and removes orphaned scene objects from the renderer.
    fn apply_space(&mut self, space: &protoverse::Space) {
        let (info, mut objects) = convert::convert_space(space);
        self.state.space = Some(info);

        // Transfer scene/model handles from existing objects with matching IDs
        for new_obj in &mut objects {
            if let Some(old_obj) = self.state.objects.iter().find(|o| o.id == new_obj.id) {
                new_obj.scene_object_id = old_obj.scene_object_id;
                new_obj.model_handle = old_obj.model_handle;
            }
        }

        // Remove orphaned scene objects (old objects not in the new set)
        if let Some(renderer) = &self.renderer {
            let mut r = renderer.renderer.lock().unwrap();
            for old_obj in &self.state.objects {
                if let Some(scene_id) = old_obj.scene_object_id
                    && !objects.iter().any(|o| o.id == old_obj.id)
                {
                    r.remove_object(scene_id);
                }
            }
        }

        self.load_object_models(&mut objects);
        self.state.objects = objects;
        self.state.dirty = false;
    }

    /// Load space state from a nostrdb query result.
    fn load_space_from_ndb(&mut self, ndb: &nostrdb::Ndb, txn: &nostrdb::Transaction) {
        let notes = subscriptions::RoomSubscription::query_existing(ndb, txn);

        for note in &notes {
            let Some(space_id) = nostr_events::get_space_id(note) else {
                continue;
            };
            if space_id != self.state.space_ref.id {
                continue;
            }

            let Some(space) = nostr_events::parse_space_event(note) else {
                tracing::warn!("Failed to parse space event content");
                continue;
            };

            self.apply_space(&space);
            tracing::info!("Loaded space '{}' from nostrdb", space_id);
            return;
        }
    }

    /// Save current space state: build Space, serialize, ingest as new nostr event.
    fn save_space(&mut self, ctx: &mut AppContext<'_>) {
        let Some(info) = &self.state.space else {
            tracing::warn!("save_space: no space to save");
            return;
        };
        let Some(kp) = ctx.accounts.selected_filled() else {
            tracing::warn!("save_space: no keypair available");
            return;
        };

        let space = convert::build_space(info, &self.state.objects);
        let builder = nostr_events::build_space_event(&space, &self.state.space_ref.id);
        if let Some((msg, id)) = nostr_events::ingest_event(builder, ctx.ndb, kp) {
            self.last_save_id = Some(id);
            self.send_to_relay(ctx.pool, &msg);
        }
        tracing::info!("Saved space '{}'", self.state.space_ref.id);
    }

    /// Load 3D models for objects, then resolve any semantic locations
    /// (e.g. "top-of obj1") to concrete positions using AABB bounds.
    ///
    /// For remote URLs (http/https), the model cache handles async download
    /// and disk caching. Models that aren't yet downloaded will be loaded
    /// on a future frame via `poll_model_downloads`.
    fn load_object_models(&mut self, objects: &mut [RoomObject]) {
        let renderer = self.renderer.as_ref();
        let model_bounds_fn = |m: Option<renderbud::Model>| -> Option<renderbud::Aabb> {
            let r = renderer?.renderer.lock().unwrap();
            r.model_bounds(m?)
        };

        // Phase 1: Load all models and cache their AABB bounds.
        // Remote URLs may return None (download in progress); those objects
        // will get their model_handle assigned later via poll_model_downloads.
        let mut bounds_by_id: std::collections::HashMap<String, renderbud::Aabb> =
            std::collections::HashMap::new();

        for obj in objects.iter_mut() {
            // Skip if already loaded
            if obj.model_handle.is_some() {
                if let Some(bounds) = model_bounds_fn(obj.model_handle) {
                    bounds_by_id.insert(obj.id.clone(), bounds);
                }
                continue;
            }

            if let Some(url) = obj.model_url.clone() {
                let local_path = if let Some(cache) = &mut self.model_cache {
                    cache.request(&url)
                } else {
                    Some(std::path::PathBuf::from(&url))
                };

                if let Some(path) = local_path {
                    let model = self.load_model(path.to_str().unwrap_or(&url));
                    if let Some(bounds) = model_bounds_fn(model) {
                        bounds_by_id.insert(obj.id.clone(), bounds);
                    }
                    obj.model_handle = model;
                    if let Some(cache) = &mut self.model_cache {
                        cache.mark_loaded(&url);
                    }
                }
            }
        }

        resolve_locations(objects, &bounds_by_id);
    }

    /// Poll for completed model downloads, load into GPU, and re-resolve
    /// semantic locations so dependent objects are positioned correctly.
    fn poll_model_downloads(&mut self) {
        let Some(cache) = &mut self.model_cache else {
            return;
        };

        let ready = cache.poll();
        if ready.is_empty() {
            return;
        }

        let mut any_loaded = false;
        for (url, path) in ready {
            let path_str = path.to_string_lossy();
            let model = self.load_model(&path_str);

            if model.is_none() {
                tracing::warn!("Failed to load cached model at {}", path_str);
                continue;
            }

            for obj in &mut self.state.objects {
                if obj.model_url.as_deref() == Some(&url) && obj.model_handle.is_none() {
                    obj.model_handle = model;
                    obj.scene_object_id = None;
                    any_loaded = true;
                }
            }

            if let Some(cache) = &mut self.model_cache {
                cache.mark_loaded(&url);
            }
        }

        if any_loaded {
            resolve_object_locations(self.renderer.as_ref(), &mut self.state.objects);
        }
    }

    /// Poll the space subscription for updates.
    /// Skips applying updates while the space has unsaved local edits.
    fn poll_space_updates(&mut self, ndb: &nostrdb::Ndb) {
        if self.state.dirty {
            return;
        }
        let Some(sub) = &self.room_sub else {
            return;
        };
        let txn = nostrdb::Transaction::new(ndb).expect("txn");
        let notes = sub.poll(ndb, &txn);

        for note in &notes {
            // Skip our own save — the in-memory state is already correct
            if let Some(last_id) = &self.last_save_id
                && note.id() == last_id
            {
                self.last_save_id = None;
                continue;
            }

            let Some(space_id) = nostr_events::get_space_id(note) else {
                continue;
            };
            if space_id != self.state.space_ref.id {
                continue;
            }

            let Some(space) = nostr_events::parse_space_event(note) else {
                continue;
            };

            self.apply_space(&space);
            tracing::info!("Space '{}' updated from nostrdb", space_id);
        }
    }

    /// Run one tick of presence: publish local position, poll remote, expire stale.
    fn tick_presence(&mut self, ctx: &mut AppContext<'_>) {
        let now = self.start_time.elapsed().as_secs_f64();

        // Publish our position (throttled — only on change or keep-alive)
        if let Some(kp) = ctx.accounts.selected_filled() {
            let self_pos = self
                .state
                .self_user()
                .map(|u| u.position)
                .unwrap_or(Vec3::ZERO);

            if let Some(msg) =
                self.presence_pub
                    .maybe_publish(ctx.ndb, kp, &self.space_naddr, self_pos, now)
            {
                self.send_to_relay(ctx.pool, &msg);
            }
        }

        // Poll for remote presence events
        let self_pubkey = *ctx.accounts.selected_account_pubkey();
        if let Some(sub) = &self.presence_sub {
            let changed = presence::poll_presence(
                sub,
                ctx.ndb,
                &self.space_naddr,
                &self_pubkey,
                &mut self.state.users,
                now,
            );

            // Assign avatar model to new users
            if changed {
                let avatar_model = self.state.self_user().and_then(|u| u.model_handle);
                if let Some(model) = avatar_model {
                    for user in &mut self.state.users {
                        if user.model_handle.is_none() {
                            user.model_handle = Some(model);
                        }
                    }
                }
            }
        }

        // Expire stale remote users (throttled to every ~10s)
        let removed = self
            .presence_expiry
            .maybe_expire(&mut self.state.users, now);
        if removed > 0 {
            tracing::info!("Expired {} stale users", removed);
        }
    }

    /// Sync space objects and user avatars to the renderbud scene
    fn sync_scene(&mut self) {
        let Some(renderer) = &self.renderer else {
            return;
        };
        let mut r = renderer.renderer.lock().unwrap();

        sync_objects_to_scene(&mut self.state.objects, &mut r);

        // Update self-user's position from the camera controller
        if let Some(pos) = r.avatar_position()
            && let Some(self_user) = self.state.self_user_mut()
        {
            self_user.position = pos;
            self_user.display_position = pos;
        }

        // Smoothly lerp avatar yaw toward controller target
        let dt = 1.0 / 60.0_f32;
        if let Some(target_yaw) = r.avatar_yaw() {
            self.state.smooth_avatar_yaw = lerp_yaw(
                self.state.smooth_avatar_yaw,
                target_yaw,
                AVATAR_YAW_LERP_SPEED * dt,
            );
        }

        let now = self.start_time.elapsed().as_secs_f64();
        let avatar_y_offset = self
            .avatar_bounds
            .map(|b| (b.max.y - b.min.y) * 0.5)
            .unwrap_or(0.0)
            * AVATAR_SCALE;

        update_remote_user_positions(&mut self.state.users, dt, now);
        sync_users_to_scene(
            &mut self.state.users,
            self.state.smooth_avatar_yaw,
            avatar_y_offset,
            &mut r,
        );
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
        let egui_ctx = ui.ctx().clone();
        self.initialize(ctx, &egui_ctx);

        // Send relay subscription once connected
        self.maybe_send_relay_sub(ctx.pool);

        // Poll for space event updates
        self.poll_space_updates(ctx.ndb);

        // Poll for completed model downloads
        self.poll_model_downloads();

        // Presence: publish, poll, expire
        self.tick_presence(ctx);

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
                    ui.allocate_ui_with_layout(
                        egui::vec2(panel_width, available.y),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            egui::Frame::default().inner_margin(8.0).show(ui, |ui| {
                                if let Some(action) = render_editing_panel(ui, &mut self.state) {
                                    self.handle_action(action, ctx);
                                }
                            });
                        },
                    );
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
                // Update renderer outline highlight
                if let Some(renderer) = &self.renderer {
                    let scene_id = selected.as_ref().and_then(|sel_id| {
                        self.state
                            .objects
                            .iter()
                            .find(|o| &o.id == sel_id)
                            .and_then(|o| o.scene_object_id)
                    });
                    renderer.renderer.lock().unwrap().set_selected(scene_id);
                }
                self.state.selected_object = selected;
            }
            NostrverseAction::SaveSpace => {
                self.save_space(ctx);
                self.state.dirty = false;
            }
            NostrverseAction::AddObject(mut obj) => {
                // Try to load model immediately (handles local + cached remote)
                if let Some(url) = obj.model_url.clone() {
                    let local_path = self.model_cache.as_mut().and_then(|c| c.request(&url));
                    if let Some(path) = local_path {
                        obj.model_handle = self.load_model(path.to_str().unwrap_or(&url));
                        if obj.model_handle.is_some()
                            && let Some(cache) = &mut self.model_cache
                        {
                            cache.mark_loaded(&url);
                        }
                    }
                }
                self.state.objects.push(obj);
                self.state.dirty = true;
            }
            NostrverseAction::RemoveObject(id) => {
                self.state.objects.retain(|o| o.id != id);
                if self.state.selected_object.as_ref() == Some(&id) {
                    self.state.selected_object = None;
                    if let Some(renderer) = &self.renderer {
                        renderer.renderer.lock().unwrap().set_selected(None);
                    }
                }
                self.state.dirty = true;
            }
            NostrverseAction::RotateObject { id, rotation } => {
                if let Some(obj) = self.state.get_object_mut(&id) {
                    obj.rotation = rotation;
                    self.state.dirty = true;
                }
            }
            NostrverseAction::DuplicateObject(id) => {
                let Some(src) = self.state.objects.iter().find(|o| o.id == id).cloned() else {
                    return;
                };
                let new_id = format!("{}-copy-{}", src.id, self.state.objects.len());
                let mut dup = src;
                dup.id = new_id.clone();
                dup.name = format!("{} (copy)", dup.name);
                dup.position.x += 0.5;
                // Clear scene node — sync_scene will create a new one.
                // Keep model_handle: it's a shared ref to loaded GPU data.
                dup.scene_object_id = None;
                self.state.objects.push(dup);
                self.state.dirty = true;
                self.state.selected_object = Some(new_id);
            }
        }
    }
}

/// Sync room objects to the renderbud scene graph.
/// Updates transforms for existing objects and places new ones.
fn sync_objects_to_scene(objects: &mut [RoomObject], r: &mut renderbud::Renderer) {
    let mut id_to_scene: std::collections::HashMap<String, renderbud::ObjectId> = objects
        .iter()
        .filter_map(|obj| Some((obj.id.clone(), obj.scene_object_id?)))
        .collect();

    for obj in objects.iter_mut() {
        let transform = Transform {
            translation: obj.position,
            rotation: obj.rotation,
            scale: obj.scale,
        };

        if let Some(scene_id) = obj.scene_object_id {
            r.update_object_transform(scene_id, transform);
        } else if let Some(model) = obj.model_handle {
            let parent_scene_id = obj.location.as_ref().and_then(|loc| match loc {
                room_state::ObjectLocation::TopOf(target_id)
                | room_state::ObjectLocation::Near(target_id) => {
                    id_to_scene.get(target_id).copied()
                }
                _ => None,
            });

            let scene_id = if let Some(parent_id) = parent_scene_id {
                r.place_object_with_parent(model, transform, parent_id)
            } else {
                r.place_object(model, transform)
            };

            obj.scene_object_id = Some(scene_id);
            id_to_scene.insert(obj.id.clone(), scene_id);
        }
    }
}

/// Smoothly interpolate between two yaw angles, wrapping around TAU.
fn lerp_yaw(current: f32, target: f32, speed: f32) -> f32 {
    let mut diff = target - current;
    diff = (diff + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    current + diff * speed.min(1.0)
}

/// Apply dead reckoning to remote users, smoothing their display positions.
fn update_remote_user_positions(users: &mut [RoomUser], dt: f32, now: f64) {
    for user in users.iter_mut() {
        if user.is_self {
            continue;
        }
        let time_since_update = (now - user.update_time).min(MAX_EXTRAPOLATION_TIME) as f32;
        let extrapolated = user.position + user.velocity * time_since_update;

        let offset = extrapolated - user.position;
        let target = if offset.length() > MAX_EXTRAPOLATION_DISTANCE {
            user.position + offset.normalize() * MAX_EXTRAPOLATION_DISTANCE
        } else {
            extrapolated
        };

        let t = (AVATAR_POS_LERP_SPEED * dt).min(1.0);
        user.display_position = user.display_position.lerp(target, t);
    }
}

/// Sync user avatars to the renderbud scene with proper transforms.
fn sync_users_to_scene(
    users: &mut [RoomUser],
    smooth_yaw: f32,
    avatar_y_offset: f32,
    r: &mut renderbud::Renderer,
) {
    for user in users.iter_mut() {
        let yaw = if user.is_self { smooth_yaw } else { 0.0 };

        let transform = Transform {
            translation: user.display_position + Vec3::new(0.0, avatar_y_offset, 0.0),
            rotation: glam::Quat::from_rotation_y(yaw),
            scale: Vec3::splat(AVATAR_SCALE),
        };

        if let Some(scene_id) = user.scene_object_id {
            r.update_object_transform(scene_id, transform);
        } else if let Some(model) = user.model_handle {
            user.scene_object_id = Some(r.place_object(model, transform));
        }
    }
}

/// Resolve semantic locations (top-of, near, floor) to concrete positions
/// using the provided AABB bounds map.
fn resolve_locations(
    objects: &mut [RoomObject],
    bounds_by_id: &std::collections::HashMap<String, renderbud::Aabb>,
) {
    let mut resolved: Vec<(usize, Vec3, Vec3)> = Vec::new();

    for (i, obj) in objects.iter().enumerate() {
        let Some(loc) = &obj.location else {
            continue;
        };

        let local_base = match loc {
            room_state::ObjectLocation::TopOf(target_id) => {
                let target_top = bounds_by_id.get(target_id).map(|b| b.max.y).unwrap_or(0.0);
                let self_half_h = bounds_by_id
                    .get(&obj.id)
                    .map(|b| (b.max.y - b.min.y) * 0.5)
                    .unwrap_or(0.0);
                Some(Vec3::new(0.0, target_top + self_half_h, 0.0))
            }
            room_state::ObjectLocation::Near(target_id) => {
                let offset = bounds_by_id
                    .get(target_id)
                    .map(|b| b.max.x - b.min.x)
                    .unwrap_or(1.0);
                Some(Vec3::new(offset, 0.0, 0.0))
            }
            room_state::ObjectLocation::Floor => {
                let self_half_h = bounds_by_id
                    .get(&obj.id)
                    .map(|b| (b.max.y - b.min.y) * 0.5)
                    .unwrap_or(0.0);
                Some(Vec3::new(0.0, self_half_h, 0.0))
            }
            _ => None,
        };

        if let Some(base) = local_base {
            resolved.push((i, base, base + obj.position));
        }
    }

    for (i, base, pos) in resolved {
        objects[i].location_base = Some(base);
        objects[i].position = pos;
    }
}

/// Collect AABB bounds for all objects that have a loaded model.
fn collect_bounds(
    renderer: Option<&renderbud::egui::EguiRenderer>,
    objects: &[RoomObject],
) -> std::collections::HashMap<String, renderbud::Aabb> {
    let mut bounds = std::collections::HashMap::new();
    let Some(renderer) = renderer else {
        return bounds;
    };
    let r = renderer.renderer.lock().unwrap();
    for obj in objects {
        if let Some(model) = obj.model_handle
            && let Some(b) = r.model_bounds(model)
        {
            bounds.insert(obj.id.clone(), b);
        }
    }
    bounds
}

/// Re-resolve semantic locations (top-of, near, floor) using current model bounds.
fn resolve_object_locations(
    renderer: Option<&renderbud::egui::EguiRenderer>,
    objects: &mut [RoomObject],
) {
    let bounds_by_id = collect_bounds(renderer, objects);
    resolve_locations(objects, &bounds_by_id);
}
