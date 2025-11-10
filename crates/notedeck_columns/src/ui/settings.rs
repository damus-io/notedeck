use egui::{
    vec2, Color32, ComboBox, CornerRadius, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, TextEdit, ThemePreference,
};
use egui_extras::{Size, StripBuilder};
use enostr::NoteId;
use nostrdb::Transaction;
use notedeck::{
    tr,
    ui::richtext_small,
    Images, JobsCache, LanguageIdentifier, Localization, NoteContext, NotedeckTextStyle, Settings,
    SettingsHandler, DEFAULT_NOTE_BODY_FONT_SIZE,
};
use notedeck_ui::{
    app_images::{connected_image, copy_to_clipboard_dark_image, copy_to_clipboard_image, key_image, settings_dark_image, settings_light_image},
    rounded_button, segmented_button, AnimationHelper, NoteOptions, NoteView,
};

use crate::{
    nav::{BodyResponse, RouterAction},
    ui::account_login_view::eye_button,
    Damus, Route, SettingsRoute,
};

const PREVIEW_NOTE_ID: &str = "note1edjc8ggj07hwv77g2405uh6j2jkk5aud22gktxrvc2wnre4vdwgqzlv2gw";

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 3.0;
const ZOOM_STEP: f32 = 0.1;
const RESET_ZOOM: f32 = 1.0;

enum SettingsIcon<'a> {
    Image(egui::Image<'a>),
    Emoji(&'a str),
}

pub enum SettingsAction {
    SetZoomFactor(f32),
    SetTheme(ThemePreference),
    SetLocale(LanguageIdentifier),
    SetRepliestNewestFirst(bool),
    SetNoteBodyFontSize(f32),
    SetAnimateNavTransitions(bool),
    OpenRelays,
    OpenCacheFolder,
    ClearCacheFolder,
    RouteToSettings(SettingsRoute),
}

impl SettingsAction {
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        settings: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
    ) -> Option<RouterAction> {
        let mut route_action: Option<RouterAction> = None;

        match self {
            Self::RouteToSettings(settings_route) => {
                route_action = Some(RouterAction::route_to(Route::Settings(settings_route)));
            }
            Self::OpenRelays => {
                route_action = Some(RouterAction::route_to(Route::Relays));
            }
            Self::SetZoomFactor(zoom_factor) => {
                ctx.set_zoom_factor(zoom_factor);
                settings.set_zoom_factor(zoom_factor);
            }
            Self::SetTheme(theme) => {
                ctx.set_theme(theme);
                settings.set_theme(theme);
            }
            Self::SetLocale(language) => {
                if i18n.set_locale(language.clone()).is_ok() {
                    settings.set_locale(language.to_string());
                }
            }
            Self::SetRepliestNewestFirst(value) => {
                app.note_options.set(NoteOptions::RepliesNewestFirst, value);
                settings.set_show_replies_newest_first(value);
            }
            Self::OpenCacheFolder => {
                use opener;
                let _ = opener::open(img_cache.base_path.clone());
            }
            Self::ClearCacheFolder => {
                let _ = img_cache.clear_folder_contents();
            }
            Self::SetNoteBodyFontSize(size) => {
                let mut style = (*ctx.style()).clone();
                style.text_styles.insert(
                    NotedeckTextStyle::NoteBody.text_style(),
                    FontId::proportional(size),
                );
                ctx.set_style(style);

                settings.set_note_body_font_size(size);
            }
            Self::SetAnimateNavTransitions(value) => {
                settings.set_animate_nav_transitions(value);
            }
        }
        route_action
    }
}

pub struct SettingsView<'a> {
    settings: &'a mut Settings,
    note_context: &'a mut NoteContext<'a>,
    note_options: &'a mut NoteOptions,
    jobs: &'a mut JobsCache,
}

fn settings_group<S>(ui: &mut egui::Ui, title: S, contents: impl FnOnce(&mut egui::Ui))
where
    S: Into<String>,
{
    ui.label(
        RichText::new(title)
            .text_style(NotedeckTextStyle::Small.text_style())
            .color(ui.visuals().weak_text_color()),
    );

    ui.add_space(8.0);

    Frame::group(ui.style())
        .fill(ui.style().visuals.widgets.open.bg_fill)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            contents(ui)
        });
}

impl<'a> SettingsView<'a> {
    pub fn new(
        settings: &'a mut Settings,
        note_context: &'a mut NoteContext<'a>,
        note_options: &'a mut NoteOptions,
        jobs: &'a mut JobsCache,
    ) -> Self {
        Self {
            settings,
            note_context,
            note_options,
            jobs,
        }
    }

    /// Get the localized name for a language identifier
    fn get_selected_language_name(&mut self) -> String {
        if let Ok(lang_id) = self.settings.locale.parse::<LanguageIdentifier>() {
            self.note_context
                .i18n
                .get_locale_native_name(&lang_id)
                .map(|s| s.to_owned())
                .unwrap_or_else(|| lang_id.to_string())
        } else {
            self.settings.locale.clone()
        }
    }

    pub fn appearance_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action = None;
        let title = tr!(
            self.note_context.i18n,
            "Appearance",
            "Label for appearance settings section",
        );
        settings_group(ui, title, |ui| {
            // Font size row
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Font size",
                    "Label for font size, Appearance settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    if ui.add(rounded_button(tr!(
                        self.note_context.i18n,
                        "Reset",
                        "Label for reset note body font size, Appearance settings section",
                    ))).clicked() {
                        action = Some(SettingsAction::SetNoteBodyFontSize(DEFAULT_NOTE_BODY_FONT_SIZE));
                    }

                    ui.add_space(8.0);
                    ui.label(format!("{:.0}", self.settings.note_body_font_size));
                    ui.add_space(8.0);

                    if ui.add(egui::Slider::new(&mut self.settings.note_body_font_size, 8.0..=32.0)
                        .text("")
                        .show_value(false)).changed() {
                        action = Some(SettingsAction::SetNoteBodyFontSize(self.settings.note_body_font_size));
                    }
                });
            });

            // Preview note
            let txn = Transaction::new(self.note_context.ndb).unwrap();
            if let Some(note_id) = NoteId::from_bech(PREVIEW_NOTE_ID) {
                if let Ok(preview_note) = self.note_context.ndb.get_note_by_id(&txn, note_id.bytes()) {
                    ui.add_space(8.0);
                    notedeck_ui::padding(8.0, ui, |ui| {
                        ui.set_max_width(ui.available_width());

                        NoteView::new(
                            self.note_context,
                            &preview_note,
                            *self.note_options,
                            self.jobs,
                        )
                        .actionbar(false)
                        .options_button(false)
                        .show(ui);
                    });
                    ui.add_space(8.0);
                }
            }

            ui.painter().line_segment(
                [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            // Zoom level row
            let current_zoom = ui.ctx().zoom_factor();
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Zoom level",
                    "Label for zoom level, Appearance settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    if ui.add(rounded_button(tr!(
                        self.note_context.i18n,
                        "Reset",
                        "Label for reset zoom level, Appearance settings section",
                    ))).clicked() {
                        action = Some(SettingsAction::SetZoomFactor(RESET_ZOOM));
                    }

                    ui.add_space(8.0);

                    let max_reached = current_zoom >= MAX_ZOOM;
                    if ui.add_enabled(!max_reached, rounded_button("+")).clicked() {
                        action = Some(SettingsAction::SetZoomFactor((current_zoom + ZOOM_STEP).min(MAX_ZOOM)));
                    }

                    ui.add_space(4.0);
                    ui.label(format!("{:.0}%", current_zoom * 100.0));
                    ui.add_space(4.0);

                    let min_reached = current_zoom <= MIN_ZOOM;
                    if ui.add_enabled(!min_reached, rounded_button("-")).clicked() {
                        action = Some(SettingsAction::SetZoomFactor((current_zoom - ZOOM_STEP).max(MIN_ZOOM)));
                    }
                });
            });

            ui.painter().line_segment(
                [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            // Language row
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Language",
                    "Label for language, Appearance settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    let combo_style = ui.style_mut();
                    combo_style.spacing.combo_height = 32.0;
                    combo_style.spacing.button_padding = vec2(12.0, 6.0);

                    ComboBox::new("language_combo", "")
                        .selected_text(self.get_selected_language_name())
                        .show_ui(ui, |ui| {
                            for lang in self.note_context.i18n.get_available_locales() {
                                let name = self.note_context.i18n.get_locale_native_name(lang)
                                    .map(|s| s.to_owned())
                                    .unwrap_or_else(|| lang.to_string());
                                if ui.selectable_value(&mut self.settings.locale, lang.to_string(), name).clicked() {
                                    action = Some(SettingsAction::SetLocale(lang.to_owned()));
                                }
                            }
                        });
                });
            });

            ui.painter().line_segment(
                [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            // Theme row
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Theme",
                    "Label for theme, Appearance settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    let is_dark = self.settings.theme == ThemePreference::Dark;
                    if ui.add(segmented_button(
                        tr!(self.note_context.i18n, "Dark", "Label for Theme Dark, Appearance settings section"),
                        is_dark,
                        ui
                    )).clicked() {
                        action = Some(SettingsAction::SetTheme(ThemePreference::Dark));
                    }

                    ui.add_space(4.0);

                    let is_light = self.settings.theme == ThemePreference::Light;
                    if ui.add(segmented_button(
                        tr!(self.note_context.i18n, "Light", "Label for Theme Light, Appearance settings section"),
                        is_light,
                        ui
                    )).clicked() {
                        action = Some(SettingsAction::SetTheme(ThemePreference::Light));
                    }
                });
            });

            ui.painter().line_segment(
                [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            // Animate transitions row (last row, no separator)
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new("Animate view transitions").text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    let btn_text = if self.settings.animate_nav_transitions { "On" } else { "Off" };
                    if ui.add(rounded_button(btn_text)
                        .fill(if self.settings.animate_nav_transitions {
                            ui.visuals().selection.bg_fill
                        } else {
                            ui.visuals().widgets.inactive.bg_fill
                        })).clicked() {
                        self.settings.animate_nav_transitions = !self.settings.animate_nav_transitions;
                        action = Some(SettingsAction::SetAnimateNavTransitions(self.settings.animate_nav_transitions));
                    }
                });
            });
        });

        action
    }

    pub fn storage_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let id = ui.id();
        let mut action: Option<SettingsAction> = None;
        let title = tr!(
            self.note_context.i18n,
            "Storage",
            "Label for storage settings section"
        );
        settings_group(ui, title, |ui| {
            // Image cache size row
            let static_imgs_size = self.note_context.img_cache.static_imgs.cache_size.lock().unwrap();
            let gifs_size = self.note_context.img_cache.gifs.cache_size.lock().unwrap();
            let total_size = [static_imgs_size, gifs_size].iter().fold(0_u64, |acc, cur| acc + cur.unwrap_or_default());

            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Image cache size",
                    "Label for Image cache size, Storage settings section"
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    ui.label(format_size(total_size));
                });
            });

            // View folder row
            if !notedeck::ui::is_compiled_as_mobile() {
                ui.painter().line_segment(
                    [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                    egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );

                ui.horizontal(|ui| {
                    ui.set_height(44.0);

                    let response = ui.interact(ui.max_rect(), ui.id().with("view_folder"), egui::Sense::click());

                    ui.add_space(16.0);
                    ui.label(RichText::new(tr!(
                        self.note_context.i18n,
                        "View folder",
                        "Label for view folder button, Storage settings section",
                    )).text_style(NotedeckTextStyle::Body.text_style()));

                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(16.0);
                        ui.label(RichText::new("›").color(ui.visuals().weak_text_color()));
                    });

                    if response.clicked() {
                        action = Some(SettingsAction::OpenCacheFolder);
                    }
                });
            }

            // Clear cache row
            let id_clearcache = id.with("clear_cache");

            ui.painter().line_segment(
                [egui::pos2(ui.min_rect().left() + 16.0, ui.min_rect().bottom()), egui::pos2(ui.min_rect().right(), ui.min_rect().bottom())],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            ui.horizontal(|ui| {
                ui.set_height(44.0);

                let clearcache_resp = ui.interact(ui.max_rect(), ui.id().with("clear_cache_btn"), egui::Sense::click());

                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Clear cache",
                    "Label for clear cache button, Storage settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()).color(Color32::from_rgb(255, 69, 58)));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new("›").color(ui.visuals().weak_text_color()));
                });

                if clearcache_resp.clicked() {
                    ui.data_mut(|d| d.insert_temp(id_clearcache, true));
                }

                if ui.data_mut(|d| *d.get_temp_mut_or_default(id_clearcache)) {
                    let mut confirm_pressed = false;
                    clearcache_resp.show_tooltip_ui(|ui| {
                        let confirm_resp = ui.add(rounded_button(tr!(
                            self.note_context.i18n,
                            "Confirm",
                            "Label for confirm clear cache, Storage settings section"
                        )));

                        if confirm_resp.clicked() {
                            confirm_pressed = true;
                        }

                        if confirm_resp.clicked()
                            || ui.add(rounded_button(tr!(
                                self.note_context.i18n,
                                "Cancel",
                                "Label for cancel clear cache, Storage settings section"
                            ))).clicked()
                        {
                            ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                        }
                    });

                    if confirm_pressed {
                        action = Some(SettingsAction::ClearCacheFolder);
                    } else if !confirm_pressed && clearcache_resp.clicked_elsewhere() {
                        ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                    }
                }
            });
        });

        action
    }

    fn other_options_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action = None;

        let title = tr!(
            self.note_context.i18n,
            "Content",
            "Label for content settings section"
        );
        settings_group(ui, title, |ui| {
            // Sort replies row
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.add_space(16.0);
                ui.label(RichText::new(tr!(
                    self.note_context.i18n,
                    "Sort replies newest first",
                    "Label for Sort replies newest first, content settings section",
                )).text_style(NotedeckTextStyle::Body.text_style()));

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);

                    if ui.add(segmented_button("On", self.settings.show_replies_newest_first, ui)).clicked() {
                        self.settings.show_replies_newest_first = true;
                        action = Some(SettingsAction::SetRepliestNewestFirst(true));
                    }

                    ui.add_space(4.0);

                    if ui.add(segmented_button("Off", !self.settings.show_replies_newest_first, ui)).clicked() {
                        self.settings.show_replies_newest_first = false;
                        action = Some(SettingsAction::SetRepliestNewestFirst(false));
                    }
                });
            });
        });

        action
    }

    fn keys_section(&mut self, ui: &mut egui::Ui) {
        let title = tr!(
            self.note_context.i18n,
            "Keys",
            "label for keys setting section"
        );

        settings_group(ui, title, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    richtext_small(tr!(
                        self.note_context.i18n,
                        "PUBLIC ACCOUNT ID",
                        "label describing public key"
                    ))
                    .color(ui.visuals().gray_out(ui.visuals().text_color())),
                );
            });

            let copy_img = if ui.visuals().dark_mode {
                copy_to_clipboard_image()
            } else {
                copy_to_clipboard_dark_image()
            };
            let copy_max_size = vec2(16.0, 16.0);

            if let Some(npub) = self.note_context.accounts.selected_account_pubkey().npub() {
                item_frame(ui).show(ui, |ui| {
                    StripBuilder::new(ui)
                        .size(Size::exact(24.0))
                        .cell_layout(Layout::left_to_right(egui::Align::Center))
                        .vertical(|mut strip| {
                            strip.strip(|builder| {
                                builder
                                    .size(Size::remainder())
                                    .size(Size::exact(16.0))
                                    .cell_layout(Layout::left_to_right(egui::Align::Center))
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(richtext_small(&npub));
                                            });
                                        });

                                        strip.cell(|ui| {
                                            let helper = AnimationHelper::new(
                                                ui,
                                                "copy-to-clipboard-npub",
                                                copy_max_size,
                                            );

                                            copy_img.paint_at(ui, helper.scaled_rect());

                                            if helper.take_animation_response().clicked() {
                                                ui.ctx().copy_text(npub);
                                            }
                                        });
                                    });
                            });
                        });
                });
            }

            let Some(filled) = self.note_context.accounts.selected_filled() else {
                return;
            };
            let Some(mut nsec) = bech32::encode::<bech32::Bech32>(
                bech32::Hrp::parse_unchecked("nsec"),
                &filled.secret_key.secret_bytes(),
            )
            .ok() else {
                return;
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    richtext_small(tr!(
                        self.note_context.i18n,
                        "SECRET ACCOUNT LOGIN KEY",
                        "label describing secret key"
                    ))
                    .color(ui.visuals().gray_out(ui.visuals().text_color())),
                );
            });

            let is_password_id = ui.id().with("is-password");
            let is_password = ui
                .ctx()
                .data_mut(|d| d.get_temp(is_password_id))
                .unwrap_or(true);

            item_frame(ui).show(ui, |ui| {
                StripBuilder::new(ui)
                    .size(Size::exact(24.0))
                    .cell_layout(Layout::left_to_right(egui::Align::Center))
                    .vertical(|mut strip| {
                        strip.strip(|builder| {
                            builder
                                .size(Size::remainder())
                                .size(Size::exact(48.0))
                                .cell_layout(Layout::left_to_right(egui::Align::Center))
                                .horizontal(|mut strip| {
                                    strip.cell(|ui| {
                                        if is_password {
                                            ui.add(
                                                TextEdit::singleline(&mut nsec)
                                                    .password(is_password)
                                                    .interactive(false)
                                                    .frame(false),
                                            );
                                        } else {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(richtext_small(&nsec));
                                            });
                                        }
                                    });

                                    strip.cell(|ui| {
                                        let helper = AnimationHelper::new(
                                            ui,
                                            "copy-to-clipboard-nsec",
                                            copy_max_size,
                                        );

                                        copy_img.paint_at(ui, helper.scaled_rect());

                                        if helper.take_animation_response().clicked() {
                                            ui.ctx().copy_text(nsec);
                                        }

                                        if eye_button(ui, is_password).clicked() {
                                            ui.ctx().data_mut(|d| {
                                                d.insert_temp(is_password_id, !is_password)
                                            });
                                        }
                                    });
                                });
                        });
                    });
            });
        });
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action = None;

        Frame::default()
            .inner_margin(Margin::symmetric(10, 10))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                let dark_mode = ui.visuals().dark_mode;
                self.settings_section_with_relay(ui, "", &mut action, &[
                    ("Appearance", SettingsRoute::Appearance, Some(SettingsIcon::Image(if dark_mode { settings_dark_image() } else { settings_light_image() }))),
                    ("Content", SettingsRoute::Others, Some(SettingsIcon::Emoji("📄"))),
                    ("Storage", SettingsRoute::Storage, Some(SettingsIcon::Emoji("💾"))),
                    ("Keys", SettingsRoute::Keys, Some(SettingsIcon::Image(key_image()))),
                ]);
            });

        action
    }

    fn settings_section_with_relay<'b>(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        action: &mut Option<SettingsAction>,
        items: &[(&str, SettingsRoute, Option<SettingsIcon<'b>>)],
    ) {
        self.settings_section(ui, title, action, items, true);
    }

    fn settings_section<'b>(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        action: &mut Option<SettingsAction>,
        items: &[(&str, SettingsRoute, Option<SettingsIcon<'b>>)],
        include_relay: bool,
    ) {
        if !title.is_empty() {
            ui.label(
                RichText::new(title)
                    .text_style(NotedeckTextStyle::Small.text_style())
                    .color(ui.visuals().weak_text_color()),
            );

            ui.add_space(8.0);
        }

        Frame::group(ui.style())
            .fill(ui.style().visuals.widgets.open.bg_fill)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                for (idx, (label, route, icon)) in items.iter().enumerate() {
                    let label = *label;
                    let route = *route;
                    let is_last = idx == items.len() - 1 && !include_relay;

                    let response = ui.allocate_response(
                        vec2(ui.available_width(), 44.0),
                        egui::Sense::click(),
                    );

                    if response.clicked() {
                        *action = Some(SettingsAction::RouteToSettings(route));
                    }

                    let rect = response.rect;
                    let visuals = ui.style().interact(&response);

                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(0),
                            ui.visuals().widgets.hovered.bg_fill,
                        );
                    }

                    let mut text_x = rect.left() + 16.0;

                    // Draw icon if present
                    if let Some(icon_data) = icon {
                        let icon_size = 20.0;
                        match icon_data {
                            SettingsIcon::Image(img) => {
                                let icon_rect = egui::Rect::from_center_size(
                                    egui::pos2(text_x + icon_size / 2.0, rect.center().y),
                                    vec2(icon_size, icon_size),
                                );
                                img.clone().paint_at(ui, icon_rect);
                                text_x += icon_size + 12.0;
                            }
                            SettingsIcon::Emoji(emoji) => {
                                let emoji_galley = ui.painter().layout_no_wrap(
                                    emoji.to_string(),
                                    NotedeckTextStyle::Body.text_style().resolve(ui.style()),
                                    visuals.text_color(),
                                );
                                ui.painter().galley(
                                    egui::pos2(text_x, rect.center().y - emoji_galley.size().y / 2.0),
                                    emoji_galley,
                                    visuals.text_color(),
                                );
                                text_x += icon_size + 12.0;
                            }
                        }
                    }

                    let galley = ui.painter().layout_no_wrap(
                        label.to_string(),
                        NotedeckTextStyle::Body.text_style().resolve(ui.style()),
                        visuals.text_color(),
                    );

                    ui.painter().galley(
                        egui::pos2(text_x, rect.center().y - galley.size().y / 2.0),
                        galley,
                        visuals.text_color(),
                    );

                    // Draw chevron
                    let chevron_galley = ui.painter().layout_no_wrap(
                        "›".to_string(),
                        NotedeckTextStyle::Body.text_style().resolve(ui.style()),
                        ui.visuals().weak_text_color(),
                    );

                    ui.painter().galley(
                        rect.right_center() + vec2(-16.0 - chevron_galley.size().x, -chevron_galley.size().y / 2.0),
                        chevron_galley,
                        ui.visuals().weak_text_color(),
                    );

                    // Draw separator line
                    if !is_last {
                        let line_y = rect.bottom();
                        ui.painter().line_segment(
                            [
                                egui::pos2(rect.left() + 16.0, line_y),
                                egui::pos2(rect.right(), line_y),
                            ],
                            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                        );
                    }
                }

                // Add relay configuration item if requested
                if include_relay {
                    let response = ui.allocate_response(
                        vec2(ui.available_width(), 44.0),
                        egui::Sense::click(),
                    );

                    if response.clicked() {
                        *action = Some(SettingsAction::OpenRelays);
                    }

                    let rect = response.rect;
                    let visuals = ui.style().interact(&response);

                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(0),
                            ui.visuals().widgets.hovered.bg_fill,
                        );
                    }

                    let mut text_x = rect.left() + 16.0;

                    // Draw relay icon
                    let icon_size = 20.0;
                    let icon_rect = egui::Rect::from_center_size(
                        egui::pos2(text_x + icon_size / 2.0, rect.center().y),
                        vec2(icon_size, icon_size),
                    );
                    connected_image().paint_at(ui, icon_rect);
                    text_x += icon_size + 12.0;

                    // Draw label
                    let label = tr!(
                        self.note_context.i18n,
                        "Configure relays",
                        "Label for configure relays, settings section",
                    );
                    let galley = ui.painter().layout_no_wrap(
                        label,
                        NotedeckTextStyle::Body.text_style().resolve(ui.style()),
                        visuals.text_color(),
                    );

                    ui.painter().galley(
                        egui::pos2(text_x, rect.center().y - galley.size().y / 2.0),
                        galley,
                        visuals.text_color(),
                    );

                    // Draw chevron
                    let chevron_galley = ui.painter().layout_no_wrap(
                        "›".to_string(),
                        NotedeckTextStyle::Body.text_style().resolve(ui.style()),
                        ui.visuals().weak_text_color(),
                    );

                    ui.painter().galley(
                        rect.right_center() + vec2(-16.0 - chevron_galley.size().x, -chevron_galley.size().y / 2.0),
                        chevron_galley,
                        ui.visuals().weak_text_color(),
                    );
                }
            });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, route: &SettingsRoute) -> BodyResponse<SettingsAction> {
        match route {
            SettingsRoute::Menu => {
                BodyResponse::output(self.settings_menu(ui))
            }
            _ => {
                let scroll_out = Frame::default()
                    .inner_margin(Margin::symmetric(10, 10))
                    .show(ui, |ui| {
                        ScrollArea::vertical().show(ui, |ui| {
                            let mut action = None;

                            match route {
                                SettingsRoute::Appearance => {
                                    if let Some(new_action) = self.appearance_section(ui) {
                                        action = Some(new_action);
                                    }
                                }
                                SettingsRoute::Storage => {
                                    if let Some(new_action) = self.storage_section(ui) {
                                        action = Some(new_action);
                                    }
                                }
                                SettingsRoute::Keys => {
                                    self.keys_section(ui);
                                }
                                SettingsRoute::Others => {
                                    if let Some(new_action) = self.other_options_section(ui) {
                                        action = Some(new_action);
                                    }
                                }
                                SettingsRoute::Menu => {}
                            }

                            action
                        })
                    })
                    .inner;

                BodyResponse::scroll(scroll_out)
            }
        }
    }
}

pub fn format_size(size_bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let size = size_bytes as f64;

    if size < KB {
        format!("{size:.0} Bytes")
    } else if size < MB {
        format!("{:.1} KB", size / KB)
    } else if size < GB {
        format!("{:.1} MB", size / MB)
    } else {
        format!("{:.2} GB", size / GB)
    }
}

fn item_frame(ui: &egui::Ui) -> egui::Frame {
    Frame::new()
        .inner_margin(Margin::same(8))
        .corner_radius(CornerRadius::same(8))
        .fill(ui.visuals().panel_fill)
}
