pub mod account_login_view;
pub mod accounts;
pub mod add_column;
pub mod anim;
pub mod column;
pub mod configure_deck;
pub mod edit_deck;
pub mod images;
pub mod mention;
pub mod note;
pub mod preview;
pub mod profile;
pub mod relay;
pub mod search;
pub mod search_results;
pub mod side_panel;
pub mod support;
pub mod thread;
pub mod timeline;
pub mod username;

pub use accounts::AccountsView;
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

pub fn show_pointer(ui: &egui::Ui) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
}
