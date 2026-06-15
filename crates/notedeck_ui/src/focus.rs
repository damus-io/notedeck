//! Browser-style autofocus for app switching.
//!
//! egui drops keyboard focus when a widget isn't rendered for a frame, so when
//! the chrome switches the active app the new app starts with nothing focused.
//! [`crate::request_autofocus`] lets the chrome ask the newly-active app to
//! focus its primary input, and [`autofocus`] is the one-line opt-in an app
//! adds at that input — the egui equivalent of HTML's `autofocus` attribute.
//!
//! This only fires on cold activation (no remembered focus). Once an app has
//! been focused, the chrome restores the exact prior focus instead.

use egui::{Context, Id, Response};

/// Well-known id for the chrome's pending autofocus request.
fn autofocus_id() -> Id {
    Id::new("notedeck_autofocus_request")
}

/// Ask the active app's autofocus widget to grab keyboard focus this frame.
///
/// Called by the chrome when an app becomes active with no remembered focus.
/// Does nothing unless that app has an [`autofocus`] opt-in widget.
pub fn request_autofocus(ctx: &Context) {
    ctx.memory_mut(|m| m.data.insert_temp(autofocus_id(), true));
}

/// Browser-style autofocus opt-in for an app's primary input.
///
/// Pass the input's [`Response`]. If the chrome requested autofocus this frame
/// (the app just became active with no prior focus), this grabs focus.
/// Generic — no per-app trait wiring. The request is consumed on first use, so
/// only the first opt-in widget rendered after activation grabs focus.
pub fn autofocus(resp: &Response, ctx: &Context) {
    let consumed = ctx.memory_mut(|m| {
        if m.data.get_temp::<bool>(autofocus_id()).unwrap_or(false) {
            m.data.remove::<bool>(autofocus_id());
            true
        } else {
            false
        }
    });
    if consumed {
        resp.request_focus();
    }
}
