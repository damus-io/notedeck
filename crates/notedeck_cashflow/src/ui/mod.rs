//! Cashflow's UI pieces shared across views: the palette, the view nav, the
//! frames panes are drawn in, and the Adventure view's game HUD.

pub mod hud;
pub mod nav;
pub mod palette;

use palette::Palette;

/// The frame every Cashflow pane is drawn in: the palette's background, no
/// margin (callers add their own).
pub fn panel_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new().fill(palette.background)
}
