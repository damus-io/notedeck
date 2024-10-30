use egui::{pos2, vec2, Color32, FontId, ImageSource, Pos2, Rect, Separator, Ui};
use nostrdb::Ndb;

use crate::{
    app_style::{get_font_size, NotedeckTextStyle},
    timeline::{PubkeySource, Timeline, TimelineKind},
    ui::anim::ICON_EXPANSION_MULTIPLE,
    user_account::UserAccount,
};

use super::anim::AnimationHelper;

pub enum AddColumnResponse {
    Timeline(Timeline),
}

#[derive(Clone, Debug)]
enum AddColumnOption {
    Universe,
    Notification(PubkeySource),
    Home(PubkeySource),
}

impl AddColumnOption {
    pub fn take_as_response(
        self,
        ndb: &Ndb,
        cur_account: Option<&UserAccount>,
    ) -> Option<AddColumnResponse> {
        match self {
            AddColumnOption::Universe => TimelineKind::Universe
                .into_timeline(ndb, None)
                .map(AddColumnResponse::Timeline),
            AddColumnOption::Notification(pubkey) => TimelineKind::Notifications(pubkey)
                .into_timeline(ndb, cur_account.map(|a| a.pubkey.bytes()))
                .map(AddColumnResponse::Timeline),
            AddColumnOption::Home(pubkey) => {
                let tlk = TimelineKind::contact_list(pubkey);
                tlk.into_timeline(ndb, cur_account.map(|a| a.pubkey.bytes()))
                    .map(AddColumnResponse::Timeline)
            }
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
        let mut selected_option: Option<AddColumnResponse> = None;
        for column_option_data in self.get_column_options() {
            let option = column_option_data.option.clone();
            if self.column_option_ui(ui, column_option_data).clicked() {
                selected_option = option.take_as_response(self.ndb, self.cur_account);
            }

            ui.add(Separator::default().spacing(0.0));
        }

        selected_option
    }

    fn column_option_ui(&mut self, ui: &mut Ui, data: ColumnOptionData) -> egui::Response {
        let icon_padding = 8.0;
        let min_icon_width = 32.0;
        let height_padding = 12.0;
        let max_width = ui.available_width();
        let title_style = NotedeckTextStyle::Body;
        let desc_style = NotedeckTextStyle::Button;
        let title_min_font_size = get_font_size(ui.ctx(), &title_style);
        let desc_min_font_size = get_font_size(ui.ctx(), &desc_style);

        let max_height = {
            let max_wrap_width =
                max_width - ((icon_padding * 2.0) + (min_icon_width * ICON_EXPANSION_MULTIPLE));
            let title_max_font = FontId::new(
                title_min_font_size * ICON_EXPANSION_MULTIPLE,
                title_style.font_family(),
            );
            let desc_max_font = FontId::new(
                desc_min_font_size * ICON_EXPANSION_MULTIPLE,
                desc_style.font_family(),
            );
            let max_desc_galley = ui.fonts(|f| {
                f.layout(
                    data.description.to_string(),
                    desc_max_font,
                    Color32::WHITE,
                    max_wrap_width,
                )
            });

            let max_title_galley = ui.fonts(|f| {
                f.layout(
                    data.title.to_string(),
                    title_max_font,
                    Color32::WHITE,
                    max_wrap_width,
                )
            });

            let desc_font_max_size = max_desc_galley.rect.height();
            let title_font_max_size = max_title_galley.rect.height();
            title_font_max_size + desc_font_max_size + (2.0 * height_padding)
        };

        let helper = AnimationHelper::new(ui, data.title, vec2(max_width, max_height));
        let animation_rect = helper.get_animation_rect();

        let cur_icon_width = helper.scale_1d_pos(min_icon_width);
        let painter = ui.painter_at(animation_rect);

        let cur_icon_size = vec2(cur_icon_width, cur_icon_width);
        let cur_icon_x_pos = animation_rect.left() + (icon_padding) + (cur_icon_width / 2.0);

        let title_cur_font = FontId::new(
            helper.scale_1d_pos(title_min_font_size),
            title_style.font_family(),
        );

        let desc_cur_font = FontId::new(
            helper.scale_1d_pos(desc_min_font_size),
            desc_style.font_family(),
        );

        let wrap_width = max_width - (cur_icon_width + (icon_padding * 2.0));
        let text_color = ui.ctx().style().visuals.text_color();
        let fallback_color = ui.ctx().style().visuals.weak_text_color();

        let title_galley = painter.layout(
            data.title.to_string(),
            title_cur_font,
            text_color,
            wrap_width,
        );
        let desc_galley = painter.layout(
            data.description.to_string(),
            desc_cur_font,
            text_color,
            wrap_width,
        );

        let galley_heights = title_galley.rect.height() + desc_galley.rect.height();

        let cur_height_padding = (animation_rect.height() - galley_heights) / 2.0;
        let corner_x_pos = cur_icon_x_pos + (cur_icon_width / 2.0) + icon_padding;
        let title_corner_pos = Pos2::new(corner_x_pos, animation_rect.top() + cur_height_padding);
        let desc_corner_pos = Pos2::new(
            corner_x_pos,
            title_corner_pos.y + title_galley.rect.height(),
        );

        let icon_cur_y = animation_rect.top() + cur_height_padding + (galley_heights / 2.0);
        let icon_img = egui::Image::new(data.icon).fit_to_exact_size(cur_icon_size);
        let icon_rect = Rect::from_center_size(pos2(cur_icon_x_pos, icon_cur_y), cur_icon_size);

        icon_img.paint_at(ui, icon_rect);
        painter.galley(title_corner_pos, title_galley, fallback_color);
        painter.galley(desc_corner_pos, desc_galley, fallback_color);

        helper.take_animation_response()
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
