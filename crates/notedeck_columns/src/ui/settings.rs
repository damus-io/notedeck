use egui::{
    vec2, Button, Color32, ComboBox, CornerRadius, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, TextEdit, ThemePreference,
};
use egui_extras::{Size, StripBuilder};
use enostr::NoteId;
use nostrdb::Transaction;
use notedeck::{
    tr, ui::richtext_small, BodyResponse, Images, LanguageIdentifier, Localization, NoteContext,
    NotedeckTextStyle, Settings, SettingsHandler, DEFAULT_MAX_HASHTAGS_PER_NOTE,
    DEFAULT_NOTE_BODY_FONT_SIZE,
};
use notedeck_ui::{
    app_images::{copy_to_clipboard_dark_image, copy_to_clipboard_image},
    AnimationHelper, NoteOptions, NoteView,
};

use crate::{nav::RouterAction, ui::account_login_view::eye_button, Damus, Route};

const PREVIEW_NOTE_ID: &str = "note1edjc8ggj07hwv77g2405uh6j2jkk5aud22gktxrvc2wnre4vdwgqzlv2gw";

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 3.0;
const ZOOM_STEP: f32 = 0.1;
const RESET_ZOOM: f32 = 1.0;

pub enum SettingsAction {
    SetZoomFactor(f32),
    SetTheme(ThemePreference),
    SetLocale(LanguageIdentifier),
    SetRepliestNewestFirst(bool),
    SetNoteBodyFontSize(f32),
    SetAnimateNavTransitions(bool),
    SetMaxHashtagsPerNote(usize),
    OpenRelays,
    OpenCacheFolder,
    ClearCacheFolder,
}

impl SettingsAction {
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        settings: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
        accounts: &mut notedeck::Accounts,
    ) -> Option<RouterAction> {
        let mut route_action: Option<RouterAction> = None;

        match self {
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

            Self::SetMaxHashtagsPerNote(value) => {
                settings.set_max_hashtags_per_note(value);
                accounts.update_max_hashtags_per_note(value);
            }
        }
        route_action
    }
}

pub struct SettingsView<'a> {
    settings: &'a mut Settings,
    note_context: &'a mut NoteContext<'a>,
    note_options: &'a mut NoteOptions,
}

fn settings_group<S>(ui: &mut egui::Ui, title: S, contents: impl FnOnce(&mut egui::Ui))
where
    S: Into<String>,
{
    Frame::group(ui.style())
        .fill(ui.style().visuals.widgets.open.bg_fill)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.label(RichText::new(title).text_style(NotedeckTextStyle::Body.text_style()));
            ui.separator();

            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);

                contents(ui)
            });
        });
}

impl<'a> SettingsView<'a> {
    pub fn new(
        settings: &'a mut Settings,
        note_context: &'a mut NoteContext<'a>,
        note_options: &'a mut NoteOptions,
    ) -> Self {
        Self {
            settings,
            note_context,
            note_options,
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
            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Font size:",
                    "Label for font size, Appearance settings section",
                )));

                if ui
                    .add(
                        egui::Slider::new(&mut self.settings.note_body_font_size, 8.0..=32.0)
                            .text(""),
                    )
                    .changed()
                {
                    action = Some(SettingsAction::SetNoteBodyFontSize(
                        self.settings.note_body_font_size,
                    ));
                };

                if ui
                    .button(richtext_small(tr!(
                        self.note_context.i18n,
                        "Reset",
                        "Label for reset note body font size, Appearance settings section",
                    )))
                    .clicked()
                {
                    action = Some(SettingsAction::SetNoteBodyFontSize(
                        DEFAULT_NOTE_BODY_FONT_SIZE,
                    ));
                }
            });

            let txn = Transaction::new(self.note_context.ndb).unwrap();

            if let Some(note_id) = NoteId::from_bech(PREVIEW_NOTE_ID) {
                if let Ok(preview_note) =
                    self.note_context.ndb.get_note_by_id(&txn, note_id.bytes())
                {
                    notedeck_ui::padding(8.0, ui, |ui| {
                        if notedeck::ui::is_narrow(ui.ctx()) {
                            ui.set_max_width(ui.available_width());

                            NoteView::new(self.note_context, &preview_note, *self.note_options)
                                .actionbar(false)
                                .options_button(false)
                                .show(ui);
                        }
                    });
                    ui.separator();
                }
            }

            let current_zoom = ui.ctx().zoom_factor();

            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Zoom Level:",
                    "Label for zoom level, Appearance settings section",
                )));

                let min_reached = current_zoom <= MIN_ZOOM;
                let max_reached = current_zoom >= MAX_ZOOM;

                if ui
                    .add_enabled(
                        !min_reached,
                        Button::new(
                            RichText::new("-").text_style(NotedeckTextStyle::Small.text_style()),
                        ),
                    )
                    .clicked()
                {
                    let new_zoom = (current_zoom - ZOOM_STEP).max(MIN_ZOOM);
                    action = Some(SettingsAction::SetZoomFactor(new_zoom));
                };

                ui.label(
                    RichText::new(format!("{:.0}%", current_zoom * 100.0))
                        .text_style(NotedeckTextStyle::Small.text_style()),
                );

                if ui
                    .add_enabled(
                        !max_reached,
                        Button::new(
                            RichText::new("+").text_style(NotedeckTextStyle::Small.text_style()),
                        ),
                    )
                    .clicked()
                {
                    let new_zoom = (current_zoom + ZOOM_STEP).min(MAX_ZOOM);
                    action = Some(SettingsAction::SetZoomFactor(new_zoom));
                };

                if ui
                    .button(richtext_small(tr!(
                        self.note_context.i18n,
                        "Reset",
                        "Label for reset zoom level, Appearance settings section",
                    )))
                    .clicked()
                {
                    action = Some(SettingsAction::SetZoomFactor(RESET_ZOOM));
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Language:",
                    "Label for language, Appearance settings section",
                )));

                //
                ComboBox::from_label("")
                    .selected_text(self.get_selected_language_name())
                    .show_ui(ui, |ui| {
                        for lang in self.note_context.i18n.get_available_locales() {
                            let name = self
                                .note_context
                                .i18n
                                .get_locale_native_name(lang)
                                .map(|s| s.to_owned())
                                .unwrap_or_else(|| lang.to_string());
                            if ui
                                .selectable_value(&mut self.settings.locale, lang.to_string(), name)
                                .clicked()
                            {
                                action = Some(SettingsAction::SetLocale(lang.to_owned()))
                            }
                        }
                    });
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Theme:",
                    "Label for theme, Appearance settings section",
                )));

                if ui
                    .selectable_value(
                        &mut self.settings.theme,
                        ThemePreference::Light,
                        richtext_small(tr!(
                            self.note_context.i18n,
                            "Light",
                            "Label for Theme Light, Appearance settings section",
                        )),
                    )
                    .clicked()
                {
                    action = Some(SettingsAction::SetTheme(ThemePreference::Light));
                }

                if ui
                    .selectable_value(
                        &mut self.settings.theme,
                        ThemePreference::Dark,
                        richtext_small(tr!(
                            self.note_context.i18n,
                            "Dark",
                            "Label for Theme Dark, Appearance settings section",
                        )),
                    )
                    .clicked()
                {
                    action = Some(SettingsAction::SetTheme(ThemePreference::Dark));
                }
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
            ui.horizontal_wrapped(|ui| {
                let static_imgs_size = self
                    .note_context
                    .img_cache
                    .static_imgs
                    .cache_size
                    .lock()
                    .unwrap();

                let gifs_size = self.note_context.img_cache.gifs.cache_size.lock().unwrap();

                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        tr!(
                            self.note_context.i18n,
                            "Image cache size:",
                            "Label for Image cache size, Storage settings section"
                        ),
                        format_size(
                            [static_imgs_size, gifs_size]
                                .iter()
                                .fold(0_u64, |acc, cur| acc + cur.unwrap_or_default())
                        )
                    ))
                    .text_style(NotedeckTextStyle::Small.text_style()),
                );

                ui.end_row();

                if !notedeck::ui::is_compiled_as_mobile()
                    && ui
                        .button(richtext_small(tr!(
                            self.note_context.i18n,
                            "View folder",
                            "Label for view folder button, Storage settings section",
                        )))
                        .clicked()
                {
                    action = Some(SettingsAction::OpenCacheFolder);
                }

                let clearcache_resp = ui.button(
                    richtext_small(tr!(
                        self.note_context.i18n,
                        "Clear cache",
                        "Label for clear cache button, Storage settings section",
                    ))
                    .color(Color32::LIGHT_RED),
                );

                let id_clearcache = id.with("clear_cache");
                if clearcache_resp.clicked() {
                    ui.data_mut(|d| d.insert_temp(id_clearcache, true));
                }

                if ui.data_mut(|d| *d.get_temp_mut_or_default(id_clearcache)) {
                    let mut confirm_pressed = false;
                    clearcache_resp.show_tooltip_ui(|ui| {
                        let confirm_resp = ui.button(tr!(
                            self.note_context.i18n,
                            "Confirm",
                            "Label for confirm clear cache, Storage settings section"
                        ));
                        if confirm_resp.clicked() {
                            confirm_pressed = true;
                        }

                        if confirm_resp.clicked()
                            || ui
                                .button(tr!(
                                    self.note_context.i18n,
                                    "Cancel",
                                    "Label for cancel clear cache, Storage settings section"
                                ))
                                .clicked()
                        {
                            ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                        }
                    });

                    if confirm_pressed {
                        action = Some(SettingsAction::ClearCacheFolder);
                    } else if !confirm_pressed && clearcache_resp.clicked_elsewhere() {
                        ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                    }
                };
            });
        });

        action
    }

    fn other_options_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action = None;

        let title = tr!(
            self.note_context.i18n,
            "Others",
            "Label for others settings section"
        );
        settings_group(ui, title, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Sort replies newest first:",
                    "Label for Sort replies newest first, others settings section",
                )));

                if ui
                    .toggle_value(
                        &mut self.settings.show_replies_newest_first,
                        RichText::new(tr!(
                            self.note_context.i18n,
                            "On",
                            "Setting to turn on sorting replies so that the newest are shown first"
                        ))
                        .text_style(NotedeckTextStyle::Small.text_style()),
                    )
                    .changed()
                {
                    action = Some(SettingsAction::SetRepliestNewestFirst(
                        self.settings.show_replies_newest_first,
                    ));
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small("Animate view transitions:"));

                if ui
                    .toggle_value(
                        &mut self.settings.animate_nav_transitions,
                        RichText::new("On").text_style(NotedeckTextStyle::Small.text_style()),
                    )
                    .changed()
                {
                    action = Some(SettingsAction::SetAnimateNavTransitions(
                        self.settings.animate_nav_transitions,
                    ));
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Max hashtags per note:",
                    "Label for max hashtags per note, others settings section",
                )));

                if ui
                    .add(
                        egui::Slider::new(&mut self.settings.max_hashtags_per_note, 0..=20)
                            .text("")
                            .step_by(1.0),
                    )
                    .changed()
                {
                    action = Some(SettingsAction::SetMaxHashtagsPerNote(
                        self.settings.max_hashtags_per_note,
                    ));
                };

                if ui
                    .button(richtext_small(tr!(
                        self.note_context.i18n,
                        "Reset",
                        "Label for reset max hashtags per note, others settings section",
                    )))
                    .clicked()
                {
                    action = Some(SettingsAction::SetMaxHashtagsPerNote(
                        DEFAULT_MAX_HASHTAGS_PER_NOTE,
                    ));
                }
            });

            ui.horizontal_wrapped(|ui| {
                let text = if self.settings.max_hashtags_per_note == 0 {
                    tr!(
                        self.note_context.i18n,
                        "Hashtag filter disabled",
                        "Info text when hashtag filter is disabled (set to 0)"
                    )
                } else {
                    format!(
                        "Hide posts with more than {} hashtags",
                        self.settings.max_hashtags_per_note
                    )
                };
                ui.label(
                    richtext_small(&text).color(ui.visuals().gray_out(ui.visuals().text_color())),
                );
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

    fn manage_relays_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action = None;

        if ui
            .add_sized(
                [ui.available_width(), 30.0],
                Button::new(richtext_small(tr!(
                    self.note_context.i18n,
                    "Configure relays",
                    "Label for configure relays, settings section",
                ))),
            )
            .clicked()
        {
            action = Some(SettingsAction::OpenRelays);
        }

        action
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> BodyResponse<SettingsAction> {
        let scroll_out = Frame::default()
            .inner_margin(Margin::symmetric(10, 10))
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| {
                    let mut action = None;
                    if let Some(new_action) = self.appearance_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(5.0);

                    if let Some(new_action) = self.storage_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(5.0);

                    self.keys_section(ui);

                    ui.add_space(5.0);

                    if let Some(new_action) = self.other_options_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(10.0);

                    if let Some(new_action) = self.manage_relays_section(ui) {
                        action = Some(new_action);
                    }
                    action
                })
            })
            .inner;

        BodyResponse::scroll(scroll_out)
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
