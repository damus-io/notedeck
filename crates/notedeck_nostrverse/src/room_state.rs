//! Space state management for nostrverse views

use enostr::Pubkey;
use glam::{Quat, Vec3};
use renderbud::{Aabb, Model, ObjectId};

/// Actions that can be triggered from the nostrverse view
#[derive(Clone, Debug)]
pub enum NostrverseAction {
    /// Object was moved to a new position (id, new_pos)
    MoveObject { id: String, position: Vec3 },
    /// Object was selected
    SelectObject(Option<String>),
    /// Space or object was edited, needs re-ingest
    SaveSpace,
    /// A new object was added
    AddObject(RoomObject),
    /// An object was removed
    RemoveObject(String),
    /// Duplicate the selected object
    DuplicateObject(String),
    /// Object was rotated (id, new rotation)
    RotateObject { id: String, rotation: Quat },
}

/// Reference to a nostrverse space
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpaceRef {
    /// Space identifier (d-tag)
    pub id: String,
    /// Space owner pubkey
    pub pubkey: Pubkey,
}

impl SpaceRef {
    pub fn new(id: String, pubkey: Pubkey) -> Self {
        Self { id, pubkey }
    }

    /// Get the NIP-33 "a" tag format
    pub fn to_naddr(&self) -> String {
        format!("{}:{}:{}", super::kinds::ROOM, self.pubkey.hex(), self.id)
    }
}

/// Parsed space definition from event
#[derive(Clone, Debug)]
pub struct SpaceInfo {
    pub name: String,
    /// Tilemap ground plane (if present)
    pub tilemap: Option<TilemapData>,
}

impl Default for SpaceInfo {
    fn default() -> Self {
        Self {
            name: "Untitled Space".to_string(),
            tilemap: None,
        }
    }
}

/// Converted space data: space info + objects.
/// Used as the return type from convert_space to avoid fragile tuples.
pub struct SpaceData {
    pub info: SpaceInfo,
    pub objects: Vec<RoomObject>,
}

/// Spatial location relative to the room or another object.
/// Mirrors protoverse::Location for decoupling.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectLocation {
    Center,
    Floor,
    Ceiling,
    /// On top of another object (by id)
    TopOf(String),
    /// Near another object (by id)
    Near(String),
    Custom(String),
}

/// Protoverse object type, preserved for round-trip serialization
#[derive(Clone, Debug, Default)]
pub enum RoomObjectType {
    Table,
    Chair,
    Door,
    Light,
    #[default]
    Prop,
    Custom(String),
}

/// Object in a room - references a 3D model
#[derive(Clone, Debug)]
pub struct RoomObject {
    pub id: String,
    pub name: String,
    /// Protoverse cell type (table, chair, prop, etc.)
    pub object_type: RoomObjectType,
    /// URL to a glTF model (None = use placeholder geometry)
    pub model_url: Option<String>,
    /// Semantic location (e.g. "top-of obj1"), resolved to position at load time
    pub location: Option<ObjectLocation>,
    /// 3D position in world space
    pub position: Vec3,
    /// Base position from resolved location (used to compute offset for saving)
    pub location_base: Option<Vec3>,
    /// 3D rotation
    pub rotation: Quat,
    /// 3D scale
    pub scale: Vec3,
    /// Runtime: renderbud scene object handle
    pub scene_object_id: Option<ObjectId>,
    /// Runtime: loaded model handle
    pub model_handle: Option<Model>,
}

impl RoomObject {
    pub fn new(id: String, name: String, position: Vec3) -> Self {
        Self {
            id,
            name,
            object_type: RoomObjectType::Prop,
            model_url: None,
            location: None,
            position,
            location_base: None,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            scene_object_id: None,
            model_handle: None,
        }
    }

    pub fn with_object_type(mut self, object_type: RoomObjectType) -> Self {
        self.object_type = object_type;
        self
    }

    pub fn with_model_url(mut self, url: String) -> Self {
        self.model_url = Some(url);
        self
    }

    pub fn with_location(mut self, loc: ObjectLocation) -> Self {
        self.location = Some(loc);
        self
    }

    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.scale = scale;
        self
    }
}

/// Parsed tilemap data — compact tile grid representation.
#[derive(Clone, Debug)]
pub struct TilemapData {
    /// Grid width in tiles
    pub width: u32,
    /// Grid height in tiles
    pub height: u32,
    /// Tile type names (index 0 = first name, etc.)
    pub tileset: Vec<String>,
    /// Tile indices, row-major. Length == 1 means fill-all with that value.
    pub tiles: Vec<u8>,
    /// Runtime: renderbud scene object handle for the tilemap mesh
    pub scene_object_id: Option<ObjectId>,
    /// Runtime: loaded model handle for the tilemap mesh
    pub model_handle: Option<Model>,
}

impl TilemapData {
    /// Get the tile index at grid position (x, y).
    pub fn tile_at(&self, x: u32, y: u32) -> u8 {
        if self.tiles.len() == 1 {
            return self.tiles[0];
        }
        let idx = (y * self.width + x) as usize;
        self.tiles.get(idx).copied().unwrap_or(0)
    }

    /// Encode tiles back to the compact data string.
    /// If all tiles are the same value, returns just that value.
    pub fn encode_data(&self) -> String {
        if self.tiles.len() == 1 {
            return self.tiles[0].to_string();
        }
        if self.tiles.iter().all(|&t| t == self.tiles[0]) {
            return self.tiles[0].to_string();
        }
        self.tiles
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Parse the compact data string into tile indices.
    pub fn decode_data(data: &str) -> Vec<u8> {
        let parts: Vec<&str> = data.split_whitespace().collect();
        if parts.len() == 1 {
            // Fill-all mode: single value
            let val = parts[0].parse::<u8>().unwrap_or(0);
            vec![val]
        } else {
            parts.iter().map(|s| s.parse::<u8>().unwrap_or(0)).collect()
        }
    }
}

/// A user present in a room (for rendering)
#[derive(Clone, Debug)]
pub struct RoomUser {
    pub pubkey: Pubkey,
    pub display_name: String,
    /// Authoritative position from last presence event
    pub position: Vec3,
    /// Velocity from last presence event (units/second)
    pub velocity: Vec3,
    /// Smoothed display position (interpolated for remote users, direct for self)
    pub display_position: Vec3,
    /// Monotonic time when last presence update was received (extrapolation base)
    pub update_time: f64,
    /// Whether this is the current user
    pub is_self: bool,
    /// Monotonic timestamp (seconds) of last presence update
    pub last_seen: f64,
    /// Runtime: renderbud scene object handle for avatar
    pub scene_object_id: Option<ObjectId>,
    /// Runtime: loaded model handle for avatar
    pub model_handle: Option<Model>,
}

impl RoomUser {
    pub fn new(pubkey: Pubkey, display_name: String, position: Vec3) -> Self {
        Self {
            pubkey,
            display_name,
            position,
            velocity: Vec3::ZERO,
            display_position: position,
            update_time: 0.0,
            is_self: false,
            last_seen: 0.0,
            scene_object_id: None,
            model_handle: None,
        }
    }

    pub fn with_self(mut self, is_self: bool) -> Self {
        self.is_self = is_self;
        self
    }
}

/// How a drag interaction is constrained
#[derive(Clone, Debug)]
pub enum DragMode {
    /// Free object: drag on world-space Y plane
    Free,
    /// Parented object: slide on parent surface, may break away
    Parented {
        parent_id: String,
        parent_scene_id: ObjectId,
        parent_aabb: Aabb,
        /// Local Y where child sits (e.g. parent top + child half height)
        local_y: f32,
    },
}

/// State for an active object drag in the 3D viewport
pub struct DragState {
    /// ID of the object being dragged
    pub object_id: String,
    /// Offset from object position to the initial grab point
    pub grab_offset: Vec3,
    /// Y height of the drag constraint plane
    pub plane_y: f32,
    /// Drag constraint mode
    pub mode: DragMode,
}

/// State for a nostrverse view
pub struct NostrverseState {
    /// Reference to the space being viewed
    pub space_ref: SpaceRef,
    /// Parsed space data (if loaded)
    pub space: Option<SpaceInfo>,
    /// Objects in the space
    pub objects: Vec<RoomObject>,
    /// Users currently in the space
    pub users: Vec<RoomUser>,
    /// Currently selected object ID
    pub selected_object: Option<String>,
    /// Whether we're in edit mode
    pub edit_mode: bool,
    /// Smoothed avatar yaw for lerped rotation
    pub smooth_avatar_yaw: f32,
    /// Space has unsaved edits
    pub dirty: bool,
    /// Active drag state for viewport object manipulation
    pub drag_state: Option<DragState>,
    /// Grid snap size in meters
    pub grid_snap: f32,
    /// Whether grid snapping is enabled
    pub grid_snap_enabled: bool,
    /// Whether rotate mode is active (R key toggle)
    pub rotate_mode: bool,
    /// Whether the current drag is a rotation drag (started on an object in rotate mode)
    pub rotate_drag: bool,
    /// Rotation snap increment in degrees (used when grid snap is enabled)
    pub rotation_snap: f32,
    /// Cached serialized scene text (avoids re-serializing every frame)
    pub cached_scene_text: String,
}

impl NostrverseState {
    pub fn new(space_ref: SpaceRef) -> Self {
        Self {
            space_ref,
            space: None,
            objects: Vec::new(),
            users: Vec::new(),
            selected_object: None,
            edit_mode: true,
            smooth_avatar_yaw: 0.0,
            dirty: false,
            drag_state: None,
            grid_snap: 0.5,
            grid_snap_enabled: false,
            rotate_mode: false,
            rotate_drag: false,
            rotation_snap: 15.0,
            cached_scene_text: String::new(),
        }
    }

    /// Add or update a user in the room
    pub fn update_user(&mut self, user: RoomUser) {
        if let Some(existing) = self.users.iter_mut().find(|u| u.pubkey == user.pubkey) {
            *existing = user;
        } else {
            self.users.push(user);
        }
    }

    /// Remove a user from the room
    pub fn remove_user(&mut self, pubkey: &Pubkey) {
        self.users.retain(|u| &u.pubkey != pubkey);
    }

    /// Get a mutable reference to an object by ID
    pub fn get_object_mut(&mut self, id: &str) -> Option<&mut RoomObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// Get the tilemap (if present in the space info)
    pub fn tilemap(&self) -> Option<&TilemapData> {
        self.space.as_ref()?.tilemap.as_ref()
    }

    /// Get the tilemap mutably (if present in the space info)
    pub fn tilemap_mut(&mut self) -> Option<&mut TilemapData> {
        self.space.as_mut()?.tilemap.as_mut()
    }

    /// Get the local user
    pub fn self_user(&self) -> Option<&RoomUser> {
        self.users.iter().find(|u| u.is_self)
    }

    /// Get the local user mutably
    pub fn self_user_mut(&mut self) -> Option<&mut RoomUser> {
        self.users.iter_mut().find(|u| u.is_self)
    }
}
