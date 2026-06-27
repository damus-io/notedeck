//! Color tokens for Horizon's Fantastical-inspired dark calendar UI.
//!
//! These are deliberately self-contained rather than pulled from the ambient
//! egui visuals so the calendar keeps its own look regardless of the host
//! Notedeck theme.

use egui::Color32;

/// App / panel background (the near-black behind every pane).
pub const BG: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1C);
/// Slightly raised surface — the center timeline canvas and inspector cards.
pub const SURFACE: Color32 = Color32::from_rgb(0x20, 0x20, 0x23);
/// Hairline separators and the hour grid lines.
pub const GRID: Color32 = Color32::from_rgb(0x32, 0x32, 0x36);

/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEE);
/// Secondary / muted text (hour labels, field names, weekday headers).
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x90);
/// Even fainter — trailing/leading month days in the mini calendar.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5A, 0x5A, 0x60);

/// The blue used for the weekday name in the big date header & "today".
pub const ACCENT_BLUE: Color32 = Color32::from_rgb(0x3B, 0x9E, 0xFF);
/// The warm orange/red used for the year in the date header & the now-line.
pub const ACCENT_WARM: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x3D);
/// The live "now" indicator line.
pub const NOW: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);

/// Fill behind the currently selected event block (a pale tint) with dark text.
pub const SELECTED_FILL: Color32 = Color32::from_rgb(0xD8, 0xEC, 0xF8);
pub const SELECTED_TEXT: Color32 = Color32::from_rgb(0x14, 0x2A, 0x38);

/// Translucent fill for an event block painted over the dark timeline.
pub fn block_fill(color: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 0x44)
}

/// A lightened tint of an event color, used for that block's title text.
pub fn block_text(color: Color32) -> Color32 {
    let lift = |c: u8| (c as u16 + (255 - c as u16) * 7 / 10) as u8;
    Color32::from_rgb(lift(color.r()), lift(color.g()), lift(color.b()))
}
