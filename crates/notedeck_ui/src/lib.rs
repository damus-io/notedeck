pub mod anim;
pub mod app_images;
pub mod colors;
pub mod constants;
pub mod context_menu;
pub mod debug;
pub mod icons;
pub mod images;
pub mod media;
pub mod mention;
pub mod nip51_set;
pub mod note;
pub mod profile;
mod username;
pub mod widgets;

pub use anim::{rolling_number, AnimationHelper, PulseAlpha};
pub use debug::debug_slider;
pub use icons::{expanding_button, ICON_EXPANSION_MULTIPLE, ICON_WIDTH};
pub use mention::Mention;
pub use note::{NoteContents, NoteOptions, NoteView};
pub use profile::{ProfilePic, ProfilePreview};
pub use username::Username;

use egui::{Label, Margin, Pos2, RichText};

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

pub fn secondary_label(ui: &mut egui::Ui, s: impl Into<String>) -> egui::Response {
    let color = ui.style().visuals.noninteractive().fg_stroke.color;
    ui.add(Label::new(RichText::new(s).size(10.0).color(color)).selectable(false))
}

const INPUT_RECT_KEY: &str = "notedeck_input_rect";

/// Includes an input rect for keyboard visibility purposes. We use this to move the screen up if
/// a soft keyboard intersects with the input box
pub fn include_input(ui: &mut egui::Ui, resp: &egui::Response) {
    // only include input if we have focus
    if !resp.has_focus() {
        return;
    }

    ui.data_mut(|d| {
        let id = egui::Id::new(INPUT_RECT_KEY);
        match d.get_temp::<egui::Rect>(id) {
            Some(r) => d.insert_temp(id, resp.rect.union(r)),
            None => d.insert_temp(id, resp.rect),
        }
    })
}

/// Set the last input rect for keyboard visibility purposes. We use this to move the screen up if
/// a soft keyboard intersects with the input box
pub fn input_rect(ui: &mut egui::Ui) -> Option<egui::Rect> {
    ui.data(|d| d.get_temp(egui::Id::new(INPUT_RECT_KEY)))
}

/// Set the last input rect for keyboard visibility purposes. We use this to move the screen up if
/// a soft keyboard intersects with the input box
pub fn clear_input_rect(ui: &mut egui::Ui) {
    ui.data_mut(|d| d.remove::<egui::Rect>(egui::Id::new(INPUT_RECT_KEY)))
}

/// Center the galley on the center pos, returning the position of the top left position of the galley,
/// for the `painter.galley(..)`
pub fn galley_centered_pos(galley: &std::sync::Arc<egui::Galley>, center: Pos2) -> Pos2 {
    let mut top_left = center;
    top_left.x -= galley.rect.width() / 2.0;
    top_left.y -= galley.rect.height() / 2.0;

    top_left
}
