//! Nostrverse: Virtual rooms as Nostr events
//!
//! This app implements spatial views for nostrverse - a protocol where
//! rooms and objects are Nostr events (kinds 37555, 37556, 10555).
//!
//! Unlike timeline views which are scrollable lists, nostrverse views
//! are 2D spatial canvases where objects have positions.

mod room_state;
mod room_view;

pub use room_state::{
    NostrverseAction, NostrverseState, ObjectShape, Presence, Room, RoomObject, RoomRef, RoomShape,
    RoomUser,
};
pub use room_view::{NostrverseResponse, render_inspection_panel, show_room_view};

use egui::Vec2;
use enostr::Pubkey;
use notedeck::{AppContext, AppResponse};

/// Event kinds for nostrverse
pub mod kinds {
    /// Room event kind (addressable)
    pub const ROOM: u16 = 37555;
    /// Object event kind (addressable)
    pub const OBJECT: u16 = 37556;
    /// Presence event kind (user-replaceable)
    pub const PRESENCE: u16 = 10555;
}

/// Nostrverse app - a spatial canvas for virtual rooms
pub struct NostrverseApp {
    /// Current room state
    state: NostrverseState,
    /// Whether the app has been initialized with demo data
    initialized: bool,
}

impl NostrverseApp {
    /// Create a new nostrverse app with a room reference
    pub fn new(room_ref: RoomRef) -> Self {
        Self {
            state: NostrverseState::new(room_ref),
            initialized: false,
        }
    }

    /// Create with a demo room
    pub fn demo() -> Self {
        let demo_pubkey =
            Pubkey::from_hex("32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245")
                .unwrap_or_else(|_| {
                    // Fallback pubkey if parsing fails
                    Pubkey::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000001",
                    )
                    .unwrap()
                });

        let room_ref = RoomRef::new("demo-room".to_string(), demo_pubkey);
        Self::new(room_ref)
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
        });

        // Add some demo objects
        self.state.objects = vec![
            RoomObject {
                id: "obj1".to_string(),
                name: "Table".to_string(),
                shape: ObjectShape::Rectangle,
                position: Vec2::new(0.0, 0.0),
                size: Vec2::new(3.0, 2.0),
            },
            RoomObject {
                id: "obj2".to_string(),
                name: "Chair".to_string(),
                shape: ObjectShape::Circle,
                position: Vec2::new(-4.0, 2.0),
                size: Vec2::new(1.0, 1.0),
            },
            RoomObject {
                id: "obj3".to_string(),
                name: "Plant".to_string(),
                shape: ObjectShape::Triangle,
                position: Vec2::new(5.0, -3.0),
                size: Vec2::new(1.5, 2.0),
            },
        ];

        // Add demo users
        let user1_pubkey =
            Pubkey::from_hex("32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245")
                .unwrap_or_else(|_| {
                    Pubkey::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000001",
                    )
                    .unwrap()
                });

        let user2_pubkey =
            Pubkey::from_hex("fa984bd7dbb282f07e16e7ae87b26a2a7b9b90b7246a44771f0cf5ae58018f52")
                .unwrap_or_else(|_| {
                    Pubkey::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000002",
                    )
                    .unwrap()
                });

        let agent_pubkey =
            Pubkey::from_hex("ee11a5dff40c19a555f41fe42b48f00e618c91225622ae37b6c2bb67b76c4e49")
                .unwrap_or_else(|_| {
                    Pubkey::from_hex(
                        "0000000000000000000000000000000000000000000000000000000000000003",
                    )
                    .unwrap()
                });

        self.state.users = vec![
            RoomUser::new(user1_pubkey, "jb55".to_string(), Vec2::new(-2.0, -2.0)).with_self(true),
            RoomUser::new(user2_pubkey, "fiatjaf".to_string(), Vec2::new(3.0, 1.0)),
            RoomUser::new(agent_pubkey, "Claude".to_string(), Vec2::new(-5.0, 4.0))
                .with_agent(true),
        ];

        self.initialized = true;
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
                    let response = show_room_view(ui, &mut self.state);

                    // Handle actions from room view
                    if let Some(action) = response.action {
                        match action {
                            NostrverseAction::MoveObject { id, position } => {
                                // In a real implementation, this would publish a Nostr event
                                tracing::info!("Object {} moved to {:?}", id, position);
                            }
                            NostrverseAction::SelectObject(selected) => {
                                self.state.selected_object = selected;
                            }
                            NostrverseAction::OpenAddObject => {
                                // TODO: Open add object dialog
                            }
                        }
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
