use enostr::Pubkey;
use notedeck::{tr, Localization, ProfileContextSelection};

use crate::context_menu::{context_button, stationary_arbitrary_menu_button};

pub struct ProfileContextWidget {
    place_at: egui::Rect,
}

impl ProfileContextWidget {
    pub fn new(place_at: egui::Rect) -> Self {
        Self { place_at }
    }

    pub fn context_button(&self, ui: &mut egui::Ui, pubkey: &Pubkey) -> egui::Response {
        let painter = ui.painter_at(self.place_at);

        painter.circle_filled(
            self.place_at.center(),
            self.place_at.width() / 2.0,
            ui.visuals().window_fill,
        );

        context_button(ui, ui.id().with(pubkey), self.place_at.shrink(4.0))
    }

    pub fn context_menu(
        ui: &mut egui::Ui,
        i18n: &mut Localization,
        button_response: egui::Response,
        can_sign: bool,
        is_muted: bool,
    ) -> Option<ProfileContextSelection> {
        let mut context_selection: Option<ProfileContextSelection> = None;

        stationary_arbitrary_menu_button(ui, button_response, |ui| {
            ui.set_max_width(100.0);

            if ui
                .button(tr!(
                    i18n,
                    "Add as column",
                    "Add new column to current deck from profile context menu"
                ))
                .clicked()
            {
                context_selection = Some(ProfileContextSelection::AddProfileColumn);
                ui.close_menu();
            }

            if ui
                .button(tr!(i18n, "View as", "Switch active user to this profile"))
                .clicked()
            {
                context_selection = Some(ProfileContextSelection::ViewAs);
                ui.close_menu();
            }

            if ui
                .button(tr!(
                    i18n,
                    "Copy Link",
                    "Copy a damus.io link to the author's profile to keyboard"
                ))
                .clicked()
            {
                context_selection = Some(ProfileContextSelection::CopyLink);
                ui.close_menu();
            }

            if can_sign {
                let label = if is_muted {
                    tr!(i18n, "Unmute User", "Unmute this user's content")
                } else {
                    tr!(i18n, "Mute User", "Mute this user's content")
                };
                if ui.button(label).clicked() {
                    context_selection = Some(ProfileContextSelection::MuteUser);
                    ui.close_menu();
                }
            }
        });

        context_selection
    }
}
