use egui::{vec2, ImageSource, Label, Layout, Margin, RichText, Sense, Ui};
use nostrdb::Ndb;

use crate::{
    app_style::NotedeckTextStyle,
    timeline::{PubkeySource, Timeline, TimelineKind},
    user_account::UserAccount,
};

use super::padding;

pub enum AddColumnResponse {
    Timeline(Timeline),
}

enum AddColumnOption {
    Universe,
    Notification(PubkeySource),
    Home(PubkeySource),
}

impl AddColumnOption {
    pub fn take_as_response(self, ndb: &Ndb) -> Option<AddColumnResponse> {
        match self {
            AddColumnOption::Universe => TimelineKind::Universe
                .into_timeline(ndb, None)
                .map(AddColumnResponse::Timeline),
            AddColumnOption::Notification(pubkey) => TimelineKind::Notifications(pubkey)
                .into_timeline(ndb, None)
                .map(AddColumnResponse::Timeline),
            AddColumnOption::Home(pubkey) => TimelineKind::contact_list(pubkey)
                .into_timeline(ndb, None)
                .map(AddColumnResponse::Timeline),
        }
    }
}

pub struct AddColumnView<'a> {
    ndb: &'a Ndb,
    cur_account: Option<&'a UserAccount>,
}

impl<'a> AddColumnView<'a> {
    pub fn new(ndb: &'a Ndb, cur_account: Option<&'a UserAccount>) -> Self {
        Self { ndb, cur_account }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        egui::Frame::none()
            .outer_margin(Margin::symmetric(16.0, 20.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Add column").text_style(NotedeckTextStyle::Body.text_style()),
                );
            });
        ui.separator();

        let width_padding = 8.0;
        let button_height = 69.0;
        let icon_width = 32.0;
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_column_options() {
            let width = ui.available_width() - 2.0 * width_padding;
            let (rect, resp) = ui.allocate_exact_size(vec2(width, button_height), Sense::click());
            ui.allocate_ui_at_rect(rect, |ui| {
                padding(Margin::symmetric(width_padding, 0.0), ui, |ui| {
                    ui.allocate_ui_with_layout(
                        vec2(width, button_height),
                        Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            self.column_option_ui(
                                ui,
                                icon_width,
                                column_option_data.icon,
                                column_option_data.title,
                                column_option_data.description,
                            );
                        },
                    )
                    .response
                })
            });
            ui.separator();

            if resp.clicked() {
                if let Some(resp) = column_option_data.option.take_as_response(self.ndb) {
                    selected_option = Some(resp);
                }
            }
        }

        selected_option
    }

    fn column_option_ui(
        &mut self,
        ui: &mut Ui,
        icon_width: f32,
        icon: ImageSource<'_>,
        title: &str,
        description: &str,
    ) {
        ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(icon_width, icon_width)));

        ui.vertical(|ui| {
            ui.add_space(16.0);
            ui.add(
                Label::new(RichText::new(title).text_style(NotedeckTextStyle::Body.text_style()))
                    .selectable(false),
            );

            ui.add(
                Label::new(
                    RichText::new(description).text_style(NotedeckTextStyle::Button.text_style()),
                )
                .selectable(false),
            );
        });
    }

    fn get_column_options(&self) -> Vec<ColumnOptionData> {
        let mut vec = Vec::new();
        vec.push(ColumnOptionData {
            title: "Universe",
            description: "See the whole nostr universe",
            icon: egui::include_image!("../../assets/icons/universe_icon_dark_4x.png"),
            option: AddColumnOption::Universe,
        });

        if let Some(acc) = self.cur_account {
            let source = PubkeySource::Explicit(acc.pubkey);

            vec.push(ColumnOptionData {
                title: "Home timeline",
                description: "See recommended notes first",
                icon: egui::include_image!("../../assets/icons/home_icon_dark_4x.png"),
                option: AddColumnOption::Home(source.clone()),
            });
            vec.push(ColumnOptionData {
                title: "Notifications",
                description: "Stay up to date with notifications and mentions",
                icon: egui::include_image!("../../assets/icons/notifications_icon_dark_4x.png"),
                option: AddColumnOption::Notification(source),
            });
        }

        vec
    }
}

struct ColumnOptionData {
    title: &'static str,
    description: &'static str,
    icon: ImageSource<'static>,
    option: AddColumnOption,
}

mod preview {
    use crate::{
        test_data,
        ui::{Preview, PreviewConfig, View},
        Damus,
    };

    use super::AddColumnView;

    pub struct AddColumnPreview {
        app: Damus,
    }

    impl AddColumnPreview {
        fn new() -> Self {
            let app = test_data::test_app();

            AddColumnPreview { app }
        }
    }

    impl View for AddColumnPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AddColumnView::new(&self.app.ndb, self.app.accounts.get_selected_account()).ui(ui);
        }
    }

    impl<'a> Preview for AddColumnView<'a> {
        type Prev = AddColumnPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            AddColumnPreview::new()
        }
    }
}
