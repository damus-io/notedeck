//! Re-export shim: the keycap widget moved to [`notedeck_ui::keybind_hint`].
//! Kept so `crate::ui::keybind_hint::*` paths keep resolving. Remove once its
//! callers import from notedeck_ui directly.
pub use notedeck_ui::keybind_hint::*;
