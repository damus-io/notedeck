//! Room state management for nostrverse views

use enostr::Pubkey;
use glam::{Quat, Vec3};
use renderbud::{Model, ObjectId};

/// Actions that can be triggered from the nostrverse view
#[derive(Clone, Debug)]
pub enum NostrverseAction {
    /// Object was moved to a new position (id, new_pos)
    MoveObject { id: String, position: Vec3 },
    /// Object was selected
    SelectObject(Option<String>),
    /// Room or object was edited, needs re-ingest
    SaveRoom,
    /// A new object was added
    AddObject(RoomObject),
    /// An object was removed
    RemoveObject(String),
}

/// Reference to a nostrverse room
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoomRef {
    /// Room identifier (d-tag)
    pub id: String,
    /// Room owner pubkey
    pub pubkey: Pubkey,
}

impl RoomRef {
    pub fn new(id: String, pubkey: Pubkey) -> Self {
        Self { id, pubkey }
    }

    /// Get the NIP-33 "a" tag format
    pub fn to_naddr(&self) -> String {
        format!("{}:{}:{}", super::kinds::ROOM, self.pubkey.hex(), self.id)
    }
}

/// Parsed room data from event
#[derive(Clone, Debug)]
pub struct Room {
    pub name: String,
    pub shape: RoomShape,
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            name: "Untitled Room".to_string(),
            shape: RoomShape::Rectangle,
            width: 20.0,
            height: 15.0,
            depth: 10.0,
        }
    }
}

/// Room shape types
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RoomShape {
    #[default]
    Rectangle,
    Circle,
    Custom,
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

/// State for a nostrverse view
pub struct NostrverseState {
    /// Reference to the room being viewed
    pub room_ref: RoomRef,
    /// Parsed room data (if loaded)
    pub room: Option<Room>,
    /// Objects in the room
    pub objects: Vec<RoomObject>,
    /// Users currently in the room
    pub users: Vec<RoomUser>,
    /// Currently selected object ID
    pub selected_object: Option<String>,
    /// Whether we're in edit mode
    pub edit_mode: bool,
    /// Smoothed avatar yaw for lerped rotation
    pub smooth_avatar_yaw: f32,
    /// Room has unsaved edits
    pub dirty: bool,
}

impl NostrverseState {
    pub fn new(room_ref: RoomRef) -> Self {
        Self {
            room_ref,
            room: None,
            objects: Vec::new(),
            users: Vec::new(),
            selected_object: None,
            edit_mode: true,
            smooth_avatar_yaw: 0.0,
            dirty: false,
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
}
