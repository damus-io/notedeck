//! Room state management for nostrverse views

use egui::Vec2;
use enostr::Pubkey;

/// Actions that can be triggered from the nostrverse view
#[derive(Clone, Debug)]
pub enum NostrverseAction {
    /// Object was moved to a new position (id, new_pos)
    MoveObject { id: String, position: Vec2 },
    /// Object was selected
    SelectObject(Option<String>),
    /// Request to open add object UI
    OpenAddObject,
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
}

impl Default for Room {
    fn default() -> Self {
        Self {
            name: "Untitled Room".to_string(),
            shape: RoomShape::Rectangle,
            width: 20.0,
            height: 15.0,
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

/// Object in a room
#[derive(Clone, Debug)]
pub struct RoomObject {
    pub id: String,
    pub name: String,
    pub shape: ObjectShape,
    pub position: Vec2,
    pub size: Vec2,
}

/// Object shape types
#[derive(Clone, Debug, Default)]
pub enum ObjectShape {
    #[default]
    Rectangle,
    Circle,
    Triangle,
    Icon(String),
}

/// User presence in a room (legacy, use RoomUser for rendering)
#[derive(Clone, Debug)]
pub struct Presence {
    pub pubkey: Pubkey,
    pub position: Vec2,
    pub status: Option<String>,
}

/// A user present in a room (for rendering)
#[derive(Clone, Debug)]
pub struct RoomUser {
    pub pubkey: Pubkey,
    pub display_name: String,
    pub position: Vec2,
    /// Whether this is the current user
    pub is_self: bool,
    /// Whether this user is an AI agent
    pub is_agent: bool,
}

impl RoomUser {
    pub fn new(pubkey: Pubkey, display_name: String, position: Vec2) -> Self {
        Self {
            pubkey,
            display_name,
            position,
            is_self: false,
            is_agent: false,
        }
    }

    pub fn with_self(mut self, is_self: bool) -> Self {
        self.is_self = is_self;
        self
    }

    pub fn with_agent(mut self, is_agent: bool) -> Self {
        self.is_agent = is_agent;
        self
    }

    /// Derive a color from the pubkey for consistent user coloring
    pub fn derive_color(&self) -> egui::Color32 {
        let hex = self.pubkey.hex();
        // Use first 6 chars of hex as RGB
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(128);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(128);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(128);
        // Brighten colors to ensure visibility
        let brighten = |c: u8| ((c as u16 + 128) / 2) as u8 + 64;
        egui::Color32::from_rgb(brighten(r), brighten(g), brighten(b))
    }

    /// Get first character for avatar initial
    pub fn initial(&self) -> char {
        self.display_name
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
    }
}

/// State for a nostrverse view
#[derive(Clone, Debug)]
pub struct NostrverseState {
    /// Reference to the room being viewed
    pub room_ref: RoomRef,
    /// Parsed room data (if loaded)
    pub room: Option<Room>,
    /// Objects in the room
    pub objects: Vec<RoomObject>,
    /// User presences (legacy)
    pub presences: Vec<Presence>,
    /// Users currently in the room
    pub users: Vec<RoomUser>,

    // View state
    /// Camera offset (pan)
    pub camera_offset: Vec2,
    /// Zoom level (1.0 = 100%)
    pub zoom: f32,
    /// Currently selected object ID
    pub selected_object: Option<String>,
    /// Object currently being dragged (ID and original position)
    pub dragging_object: Option<(String, Vec2)>,
    /// Whether we're in edit mode
    pub edit_mode: bool,
}

impl NostrverseState {
    pub fn new(room_ref: RoomRef) -> Self {
        Self {
            room_ref,
            room: None,
            objects: Vec::new(),
            presences: Vec::new(),
            users: Vec::new(),
            camera_offset: Vec2::ZERO,
            zoom: 1.0,
            selected_object: None,
            dragging_object: None,
            edit_mode: false,
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

    /// Get a user by pubkey
    pub fn get_user(&self, pubkey: &Pubkey) -> Option<&RoomUser> {
        self.users.iter().find(|u| &u.pubkey == pubkey)
    }

    /// Get a mutable reference to an object by ID
    pub fn get_object_mut(&mut self, id: &str) -> Option<&mut RoomObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// World coordinates to screen coordinates
    pub fn world_to_screen(&self, world_pos: Vec2, canvas_center: Vec2) -> Vec2 {
        (world_pos - self.camera_offset) * self.zoom * 20.0 + canvas_center
    }

    /// Screen coordinates to world coordinates
    pub fn screen_to_world(&self, screen_pos: Vec2, canvas_center: Vec2) -> Vec2 {
        (screen_pos - canvas_center) / (self.zoom * 20.0) + self.camera_offset
    }

    /// Handle zoom (clamp to reasonable range)
    pub fn zoom_by(&mut self, delta: f32) {
        self.zoom = (self.zoom * (1.0 + delta * 0.1)).clamp(0.1, 5.0);
    }
}
