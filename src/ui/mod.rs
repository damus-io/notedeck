pub mod account_login_view;
pub mod account_management;
pub mod add_column;
pub mod anim;
pub mod mention;
pub mod configure_deck;
pub mod note;
pub mod preview;
pub mod profile;
pub mod relay;
pub mod side_panel;
pub mod support;
pub mod thread;
pub mod timeline;
pub mod username;

pub use account_management::AccountsView;
pub use mention::Mention;
pub use note::{NoteResponse, NoteView, PostReplyView, PostView};
pub use preview::{Preview, PreviewApp, PreviewConfig};
pub use profile::{ProfilePic, ProfilePreview};
pub use relay::RelayView;
pub use side_panel::{DesktopSidePanel, SidePanelAction};
pub use thread::ThreadView;
pub use timeline::TimelineView;
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
    egui::Frame::none()
        .inner_margin(amount)
        .show(ui, add_contents)
}

pub fn hline(ui: &egui::Ui) {
    // pixel perfect horizontal line
    let rect = ui.available_rect_before_wrap();
    let resize_y = ui.painter().round_to_pixel(rect.top()) - 0.5;
    let stroke = ui.style().visuals.widgets.noninteractive.bg_stroke;
    ui.painter().hline(rect.x_range(), resize_y, stroke);
}

#[inline]
#[allow(unreachable_code)]
pub fn is_compiled_as_mobile() -> bool {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        true
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        false
    }
}

/// Determine if the screen is narrow. This is useful for detecting mobile
/// contexts, but with the nuance that we may also have a wide android tablet.
pub fn is_narrow(ctx: &egui::Context) -> bool {
    let screen_size = ctx.input(|c| c.screen_rect().size());
    screen_size.x < 550.0
}

pub fn is_oled() -> bool {
    is_compiled_as_mobile()
}
