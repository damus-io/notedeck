use egui::{
    vec2, Button, Color32, ComboBox, FontId, Frame, Margin, RichText, ScrollArea, ThemePreference,
};
use enostr::NoteId;
use nostrdb::Transaction;
use notedeck::{
    tr,
    ui::{is_narrow, richtext_small},
    Images, JobsCache, LanguageIdentifier, Localization, NoteContext, NotedeckTextStyle, Settings,
    SettingsHandler, DEFAULT_NOTE_BODY_FONT_SIZE,
};
use notedeck_ui::{NoteOptions, NoteView};

use crate::{nav::RouterAction, Damus, Route};

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
            ui.horizontal(|ui| {
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
                        if is_narrow(ui.ctx()) {
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
                        }
                    });
                    ui.separator();
                }
            }

            let current_zoom = ui.ctx().zoom_factor();

            ui.horizontal(|ui| {
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

            ui.horizontal(|ui| {
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

            ui.horizontal(|ui| {
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
            ui.horizontal(|ui| {
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
        });

        action
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

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let mut action: Option<SettingsAction> = None;

        Frame::default()
            .inner_margin(Margin::symmetric(10, 10))
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| {
                    if let Some(new_action) = self.appearance_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(5.0);

                    if let Some(new_action) = self.storage_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(5.0);

                    if let Some(new_action) = self.other_options_section(ui) {
                        action = Some(new_action);
                    }

                    ui.add_space(10.0);

                    if let Some(new_action) = self.manage_relays_section(ui) {
                        action = Some(new_action);
                    }
                });
            });

        action
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
