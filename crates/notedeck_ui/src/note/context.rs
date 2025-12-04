use egui::{Rect, Vec2};
use nostrdb::NoteKey;
use notedeck::{tr, BroadcastContext, Localization, NoteContextSelection};

use crate::context_menu::{context_button, stationary_arbitrary_menu_button};

pub struct NoteContextButton {
    put_at: Option<Rect>,
    note_key: NoteKey,
}

impl egui::Widget for NoteContextButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let r = if let Some(r) = self.put_at {
            r
        } else {
            let mut place = ui.available_rect_before_wrap();
            let size = Self::max_width();
            place.set_width(size);
            place.set_height(size);
            place
        };

        Self::show(ui, self.note_key, r)
    }
}

impl NoteContextButton {
    pub fn new(note_key: NoteKey) -> Self {
        let put_at: Option<Rect> = None;
        NoteContextButton { note_key, put_at }
    }

    pub fn place_at(mut self, rect: Rect) -> Self {
        self.put_at = Some(rect);
        self
    }

    pub fn max_width() -> f32 {
        Self::max_radius() * 3.0 + Self::max_distance_between_circles() * 2.0
    }

    pub fn size() -> Vec2 {
        let width = Self::max_width();
        egui::vec2(width, width)
    }

    fn max_radius() -> f32 {
        4.0
    }

    fn max_distance_between_circles() -> f32 {
        2.0
    }

    #[profiling::function]
    pub fn show(ui: &mut egui::Ui, note_key: NoteKey, put_at: Rect) -> egui::Response {
        let id = ui.id().with(("more_options_anim", note_key));

        context_button(ui, id, put_at)
    }

    #[profiling::function]
    pub fn menu(
        ui: &mut egui::Ui,
        i18n: &mut Localization,
        button_response: egui::Response,
    ) -> Option<NoteContextSelection> {
        let mut context_selection: Option<NoteContextSelection> = None;

        stationary_arbitrary_menu_button(ui, button_response, |ui| {
            ui.set_max_width(200.0);

            if ui
                .button(tr!(
                    i18n,
                    "Copy Note Link",
                    "Copy the damus.io note link for this note to clipboard"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::CopyNeventLink);
                ui.close_menu();
            }

            // Debug: Check what the tr! macro returns
            let copy_text = tr!(
                i18n,
                "Copy Text",
                "Copy the text content of the note to clipboard"
            );
            tracing::debug!("Copy Text translation: '{}'", copy_text);

            if ui.button(copy_text).clicked() {
                context_selection = Some(NoteContextSelection::CopyText);
                ui.close_menu();
            }
            if ui
                .button(tr!(
                    i18n,
                    "Copy Pubkey",
                    "Copy the author's public key to clipboard"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::CopyPubkey);
                ui.close_menu();
            }
            if ui
                .button(tr!(
                    i18n,
                    "Copy Note ID",
                    "Copy the note identifier to clipboard"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::CopyNevent);
                ui.close_menu();
            }
            if ui
                .button(tr!(
                    i18n,
                    "Copy Note JSON",
                    "Copy the raw note data in JSON format to clipboard"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::CopyNoteJSON);
                ui.close_menu();
            }
            if ui
                .button(tr!(
                    i18n,
                    "Broadcast",
                    "Broadcast the note to all connected relays"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::Broadcast(
                    BroadcastContext::Everywhere,
                ));
                ui.close_menu();
            }
            if ui
                .button(tr!(
                    i18n,
                    "Broadcast Local",
                    "Broadcast the note only to local network relays"
                ))
                .clicked()
            {
                context_selection = Some(NoteContextSelection::Broadcast(
                    BroadcastContext::LocalNetwork,
                ));
                ui.close_menu();
            }
        });

        context_selection
    }
}
