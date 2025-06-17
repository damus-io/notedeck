pub mod anim;
pub mod blur;
pub mod colors;
pub mod constants;
pub mod contacts;
pub mod context_menu;
pub mod gif;
pub mod icons;
pub mod images;
pub mod jobs;
pub mod mention;
pub mod note;
pub mod profile;
mod username;
pub mod widgets;

pub use anim::{AnimationHelper, PulseAlpha};
pub use mention::Mention;
pub use note::{NoteContents, NoteOptions, NoteView};
pub use profile::{ProfilePic, ProfilePreview};
pub use username::Username;

use egui::Margin;

/// This is kind of like the Widget trait but is meant for larger top-level
/// views that are typically stateful.
///
/// The Widget trait forces us to add mutable
/// implementations at the type level, which screws us when generating Previews
/// for a Widget. I would have just Widget instead of making this Trait otherwise.
///
/// There is some precendent for this, it looks like there's a similar trait
/// in the egui demo library.
pub trait View {
    fn ui(&mut self, ui: &mut egui::Ui);
}

pub fn padding<R>(
    amount: impl Into<Margin>,
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::new()
        .inner_margin(amount)
        .show(ui, add_contents)
}

pub fn hline(ui: &egui::Ui) {
    hline_with_width(ui, ui.available_rect_before_wrap().x_range());
}

pub fn hline_with_width(ui: &egui::Ui, range: egui::Rangef) {
    // pixel perfect horizontal line
    let rect = ui.available_rect_before_wrap();
    #[allow(deprecated)]
    let resize_y = ui.painter().round_to_pixel(rect.top()) - 0.5;
    let stroke = ui.style().visuals.widgets.noninteractive.bg_stroke;
    ui.painter().hline(range, resize_y, stroke);
}

pub fn show_pointer(ui: &egui::Ui) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
}
