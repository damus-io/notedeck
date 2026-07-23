//! Pure message/response types shared by the session engine.
//!
//! Only the egui-free protocol types live here for now. Splitting the full set
//! of pure message types out of `notedeck_dave`'s `messages.rs` (away from its
//! egui rendering) is tracked separately; this module is scaffolded with the
//! single leaf type the session-protocol modules currently need.

/// The recorded response type for display purposes (without channel details)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResponseType {
    Allowed,
    Denied,
}
