use egui::{FontFamily, FontId};

use notedeck::fonts::NamedFontFamily;

pub static DECK_ICON_SIZE: f32 = 24.0;

pub fn deck_icon_font_sized(size: f32) -> FontId {
    egui::FontId::new(size, emoji_font_family())
}

pub fn emoji_font_family() -> FontFamily {
    egui::FontFamily::Name(NamedFontFamily::Emoji.as_str().into())
}
