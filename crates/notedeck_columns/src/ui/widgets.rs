use egui::{vec2, Widget};
use enostr::Pubkey;
use nostrdb::ProfileRecord;
use notedeck::{fonts::get_font_size, name::get_display_name, profile::get_profile_url, Accounts, Images, NotedeckTextStyle};
use notedeck_ui::widgets::styled_button_toggleable;
use notedeck_ui::ProfilePic;

/// Sized and styled to match the figma design
pub fn styled_button(text: &str, fill_color: egui::Color32) -> impl Widget + '_ {
    styled_button_toggleable(text, fill_color, true)
}

pub struct UserRow<'a> {
    profile: Option<&'a ProfileRecord<'a>>,
    pubkey: &'a Pubkey,
    cache: &'a mut Images,
    accounts: Option<&'a Accounts>,
    width: f32,
    is_selected: bool,
}

impl<'a> UserRow<'a> {
    pub fn new(
        profile: Option<&'a ProfileRecord<'a>>,
        pubkey: &'a Pubkey,
        cache: &'a mut Images,
        width: f32,
    ) -> Self {
        Self {
            profile,
            pubkey,
            cache,
            accounts: None,
            width,
            is_selected: false,
        }
    }

    pub fn with_accounts(mut self, accounts: &'a Accounts) -> Self {
        self.accounts = Some(accounts);
        self
    }

    pub fn with_selection(mut self, is_selected: bool) -> Self {
        self.is_selected = is_selected;
        self
    }
}

impl<'a> Widget for UserRow<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let pic_size = 48.0;
        let spacing = 8.0;
        let body_font_size = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);

        let (rect, resp) = ui.allocate_exact_size(
            vec2(self.width, pic_size + 8.0),
            egui::Sense::click(),
        );

        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

        if self.is_selected {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill,
            );
        }

        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().widgets.hovered.bg_fill,
            );
        }

        let pfp_rect = egui::Rect::from_min_size(
            rect.min + vec2(4.0, 4.0),
            vec2(pic_size, pic_size),
        );

        let mut profile_pic = ProfilePic::new(self.cache, get_profile_url(self.profile))
            .size(pic_size);

        if let Some(accounts) = self.accounts {
            profile_pic = profile_pic.with_follow_check(self.pubkey, accounts);
        }

        ui.put(pfp_rect, &mut profile_pic);

        let name = get_display_name(self.profile).name();
        let name_font = egui::FontId::new(body_font_size, NotedeckTextStyle::Body.font_family());
        let painter = ui.painter();
        let name_galley = painter.layout(
            name.to_owned(),
            name_font,
            ui.visuals().text_color(),
            self.width - pic_size - spacing - 8.0,
        );

        let galley_pos = egui::Pos2::new(
            pfp_rect.right() + spacing,
            rect.center().y - (name_galley.rect.height() / 2.0),
        );

        painter.galley(galley_pos, name_galley, ui.visuals().text_color());

        resp
    }
}
