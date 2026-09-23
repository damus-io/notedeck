//! Re-export shim: the keycap widget moved to [`notedeck_ui::keybind_hint`].
//! Kept so `crate::ui::keybind_hint::*` paths keep resolving — notably
//! `ui/chord_hints.rs`, which the `dave` branch is still extending. Remove once
//! that branch has converged here and its callers import from notedeck_ui directly.
pub use notedeck_ui::keybind_hint::*;
