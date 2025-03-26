// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use egui::{Button, Label, Layout, RichText, ThemePreference, Widget};
use egui_extras::{Size, StripBuilder};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::{AppContext, NotedeckTextStyle, UserAccount};
use notedeck_ui::{profile::get_profile_url, AnimationHelper, ProfilePic};

static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

#[derive(Default)]
pub struct Chrome {
    active: i32,
    apps: Vec<Box<dyn notedeck::App>>,
}

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    SaveTheme(ThemePreference),
}

impl Chrome {
    pub fn new() -> Self {
        Chrome::default()
    }

    pub fn add_app(&mut self, app: impl notedeck::App + 'static) {
        self.apps.push(Box::new(app));
    }

    pub fn set_active(&mut self, app: i32) {
        self.active = app;
    }

    /// Show the side menu or bar, depending on if we're on a narrow
    /// or wide screen.
    ///
    /// The side menu should hover over the screen, while the side bar
    /// is collapsible but persistent on the screen.
    fn show(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 0.0;

        let side_panel_width: f32 = 68.0;
        StripBuilder::new(ui)
            .size(Size::exact(side_panel_width)) // collapsible sidebar
            .size(Size::remainder()) // the main app contents
            .clip(true)
            .horizontal(|mut strip| {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    if !ui.visuals().dark_mode {
                        let rect = ui.available_rect_before_wrap();
                        ui.painter().rect(
                            rect,
                            0,
                            notedeck_ui::colors::ALMOST_WHITE,
                            egui::Stroke::new(0.0, egui::Color32::TRANSPARENT),
                            egui::StrokeKind::Inside,
                        );
                    }

                    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        self.topdown_sidebar(ui);
                    });

                    ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                        self.bottomup_sidebar(ctx, ui);
                    });

                    // vertical sidebar line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                strip.cell(|ui| {
                    /*
                    let rect = ui.available_rect_before_wrap();
                    ui.painter().rect(
                        rect,
                        0,
                        egui::Color32::RED,
                        egui::Stroke::new(1.0, egui::Color32::BLUE),
                        egui::StrokeKind::Inside,
                    );
                    */

                    self.apps[self.active as usize].update(ctx, ui);
                });
            });
    }

    /// The section of the chrome sidebar that starts at the
    /// bottom and goes up
    fn bottomup_sidebar(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> Option<ChromePanelAction> {
        let dark_mode = ui.ctx().style().visuals.dark_mode;
        let pfp_resp = self.pfp_button(ctx, ui);
        let settings_resp = ui.add(settings_button(dark_mode));

        let theme_action = match ui.ctx().theme() {
            egui::Theme::Dark => {
                let resp = ui
                    .add(Button::new("☀").frame(false))
                    .on_hover_text("Switch to light mode");
                if resp.clicked() {
                    Some(ChromePanelAction::SaveTheme(ThemePreference::Light))
                } else {
                    None
                }
            }
            egui::Theme::Light => {
                let resp = ui
                    .add(Button::new("🌙").frame(false))
                    .on_hover_text("Switch to dark mode");
                if resp.clicked() {
                    Some(ChromePanelAction::SaveTheme(ThemePreference::Light))
                } else {
                    None
                }
            }
        };

        if ui.add(support_button()).clicked() {
            return Some(ChromePanelAction::Support);
        }

        if theme_action.is_some() {
            return theme_action;
        }

        if pfp_resp.clicked() {
            Some(ChromePanelAction::Account)
        } else if settings_resp.clicked() || settings_resp.hovered() {
            Some(ChromePanelAction::Settings)
        } else {
            None
        }
    }

    fn pfp_button(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let helper = AnimationHelper::new(ui, "pfp-button", egui::vec2(max_size, max_size));

        let min_pfp_size = ICON_WIDTH;
        let cur_pfp_size = helper.scale_1d_pos(min_pfp_size);

        let txn = Transaction::new(ctx.ndb).expect("should be able to create txn");
        let profile_url = get_account_url(&txn, ctx.ndb, ctx.accounts.get_selected_account());

        let widget = ProfilePic::new(ctx.img_cache, profile_url).size(cur_pfp_size);

        ui.put(helper.get_animation_rect(), widget);

        helper.take_animation_response()
    }

    fn topdown_sidebar(&mut self, ui: &mut egui::Ui) {
        // macos needs a bit of space to make room for window
        // minimize/close buttons
        if cfg!(target_os = "macos") {
            ui.add_space(28.0);
        }

        if ui.add(expand_side_panel_button()).clicked() {
            self.active = (self.active + 1) % (self.apps.len() as i32);
        }

        ui.add_space(4.0);
        ui.add(milestone_name());
        ui.add_space(16.0);
        //let dark_mode = ui.ctx().style().visuals.dark_mode;
        //ui.add(add_column_button(dark_mode))
    }
}

impl notedeck::App for Chrome {
    fn update(&mut self, ctx: &mut notedeck::AppContext, ui: &mut egui::Ui) {
        self.show(ctx, ui);
        // TODO: unify this constant with the columns side panel width. ui crate?
    }
}

fn milestone_name() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        ui.vertical_centered(|ui| {
            let font = egui::FontId::new(
                notedeck::fonts::get_font_size(
                    ui.ctx(),
                    &NotedeckTextStyle::Tiny,
                ),
                egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(Label::new(
                RichText::new("ALPHA")
                    .color( ui.style().visuals.noninteractive().fg_stroke.color)
                    .font(font),
            ).selectable(false)).on_hover_text("Notedeck is an alpha product. Expect bugs and contact us when you run into issues.").on_hover_cursor(egui::CursorIcon::Help)
        })
            .inner
    }
}

fn expand_side_panel_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 40.0;
        let img_data = egui::include_image!("../../../assets/damus_rounded_80.png");
        let img = egui::Image::new(img_data)
            .max_width(img_size)
            .sense(egui::Sense::click());

        ui.add(img)
    }
}

fn support_button() -> impl Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let img_size = 16.0;

        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = if ui.visuals().dark_mode {
            egui::include_image!("../../../assets/icons/help_icon_dark_4x.png")
        } else {
            egui::include_image!("../../../assets/icons/help_icon_inverted_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "help-button", egui::vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper.take_animation_response()
    }
}

fn settings_button(dark_mode: bool) -> impl Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
        let img_data = if dark_mode {
            egui::include_image!("../../../assets/icons/settings_dark_4x.png")
        } else {
            egui::include_image!("../../../assets/icons/settings_light_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = AnimationHelper::new(ui, "settings-button", egui::vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper.take_animation_response()
    }
}

pub fn get_profile_url_owned(profile: Option<ProfileRecord<'_>>) -> &str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

pub fn get_account_url<'a>(
    txn: &'a nostrdb::Transaction,
    ndb: &nostrdb::Ndb,
    account: Option<&UserAccount>,
) -> &'a str {
    if let Some(selected_account) = account {
        if let Ok(profile) = ndb.get_profile_by_pubkey(txn, selected_account.key.pubkey.bytes()) {
            get_profile_url_owned(Some(profile))
        } else {
            get_profile_url_owned(None)
        }
    } else {
        get_profile_url(None)
    }
}
