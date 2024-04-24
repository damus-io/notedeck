pub mod anim;
pub mod note;
pub mod preview;
pub mod profile;
pub mod relay;
pub mod username;

pub use note::Note;
pub use preview::{Preview, PreviewApp};
pub use profile::{ProfilePic, ProfilePreview};
pub use relay::RelayView;
pub use username::Username;

use egui::Margin;

/// This is kind of like the Widget trait but is meant for larger top-level
/// views that are typically stateful. The Widget trait forces us to add mutable
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
    egui::Frame::none()
        .inner_margin(amount)
        .show(ui, add_contents)
}

pub fn is_mobile(ctx: &egui::Context) -> bool {
    //true
    let screen_size = ctx.screen_rect().size();
    screen_size.x < 550.0
}
