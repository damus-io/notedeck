use egui::{vec2, Button, Color32, ComboBox, Frame, Margin, RichText, ThemePreference};
use notedeck::{tr, Images, LanguageIdentifier, Localization, NotedeckTextStyle, SettingsHandler};
use notedeck_ui::NoteOptions;
use strum::Display;

use crate::{nav::RouterAction, Damus, Route};

#[derive(Clone, Copy, PartialEq, Eq, Display)]
pub enum ShowSourceClientOption {
    Hide,
    Top,
    Bottom,
}

impl Into<String> for ShowSourceClientOption {
    fn into(self) -> String {
        match self {
            Self::Hide => "hide".to_string(),
            Self::Top => "top".to_string(),
            Self::Bottom => "bottom".to_string(),
        }
    }
}

impl From<NoteOptions> for ShowSourceClientOption {
    fn from(note_options: NoteOptions) -> Self {
        if note_options.contains(NoteOptions::ShowNoteClientTop) {
            ShowSourceClientOption::Top
        } else if note_options.contains(NoteOptions::ShowNoteClientBottom) {
            ShowSourceClientOption::Bottom
        } else {
            ShowSourceClientOption::Hide
        }
    }
}

impl From<String> for ShowSourceClientOption {
    fn from(s: String) -> Self {
        match s.to_lowercase().as_str() {
            "hide" => Self::Hide,
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            _ => Self::Hide, // default fallback
        }
    }
}

impl ShowSourceClientOption {
    pub fn set_note_options(self, note_options: &mut NoteOptions) {
        match self {
            Self::Hide => {
                note_options.set(NoteOptions::ShowNoteClientTop, false);
                note_options.set(NoteOptions::ShowNoteClientBottom, false);
            }
            Self::Bottom => {
                note_options.set(NoteOptions::ShowNoteClientTop, false);
                note_options.set(NoteOptions::ShowNoteClientBottom, true);
            }
            Self::Top => {
                note_options.set(NoteOptions::ShowNoteClientTop, true);
                note_options.set(NoteOptions::ShowNoteClientBottom, false);
            }
        }
    }
}

pub enum SettingsAction {
    SetZoomFactor(f32),
    SetTheme(ThemePreference),
    SetShowSourceClient(ShowSourceClientOption),
    SetLocale(LanguageIdentifier),
    OpenRelays,
    OpenCacheFolder,
    ClearCacheFolder,
}

impl SettingsAction {
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        settings_handler: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
    ) -> Option<RouterAction> {
        let mut route_action: Option<RouterAction> = None;

        match self {
            SettingsAction::OpenRelays => {
                route_action = Some(RouterAction::route_to(Route::Relays));
            }
            SettingsAction::SetZoomFactor(zoom_factor) => {
                ctx.set_zoom_factor(zoom_factor);
                settings_handler.set_zoom_factor(zoom_factor);
            }
            SettingsAction::SetShowSourceClient(option) => {
                option.set_note_options(&mut app.note_options);

                settings_handler.set_show_source_client(option);
            }
            SettingsAction::SetTheme(theme) => {
                ctx.options_mut(|o| {
                    o.theme_preference = theme;
                });
                settings_handler.set_theme(theme);
            }
            SettingsAction::SetLocale(language) => {
                if i18n.set_locale(language.clone()).is_ok() {
                    settings_handler.set_locale(language.to_string());
                }
            }
            SettingsAction::OpenCacheFolder => {
                use opener;
                let _ = opener::open(img_cache.base_path.clone());
            }
            SettingsAction::ClearCacheFolder => {
                let _ = img_cache.clear_folder_contents();
            }
        }
        settings_handler.save();
        route_action
    }
}

pub struct SettingsView<'a> {
    theme: &'a mut String,
    selected_language: &'a mut String,
    show_note_client: &'a mut ShowSourceClientOption,
    i18n: &'a mut Localization,
    img_cache: &'a mut Images,
}

impl<'a> SettingsView<'a> {
    pub fn new(
        img_cache: &'a mut Images,
        selected_language: &'a mut String,
        theme: &'a mut String,
        show_note_client: &'a mut ShowSourceClientOption,
        i18n: &'a mut Localization,
    ) -> Self {
        Self {
            show_note_client,
            theme,
            img_cache,
            selected_language,
            i18n,
        }
    }

    /// Get the localized name for a language identifier
    fn get_selected_language_name(&mut self) -> String {
        if let Ok(lang_id) = self.selected_language.parse::<LanguageIdentifier>() {
            self.i18n
                .get_locale_native_name(&lang_id)
                .map(|s| s.to_owned())
                .unwrap_or_else(|| lang_id.to_string())
        } else {
            self.selected_language.clone()
        }
    }

    /// Get the localized label for ShowNoteClientOption
    fn get_show_note_client_label(&mut self, option: ShowSourceClientOption) -> String {
        match option {
            ShowSourceClientOption::Hide => tr!(
                self.i18n,
                "Hide",
                "Option in settings section to hide the source client label in note display"
            ),
            ShowSourceClientOption::Top => tr!(
                self.i18n,
                "Top",
                "Option in settings section to show the source client label at the top of the note"
            ),
            ShowSourceClientOption::Bottom => tr!(
                self.i18n,
                "Bottom",
                "Option in settings section to show the source client label at the bottom of the note"
            ),
        }.to_string()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        let id = ui.id();
        let mut action = None;

        Frame::default()
            .inner_margin(Margin::symmetric(10, 10))
            .show(ui, |ui| {
                Frame::group(ui.style())
                    .fill(ui.style().visuals.widgets.open.bg_fill)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(tr!(
                                    self.i18n,
                                    "Appearance",
                                    "Label for appearance settings section"
                                ))
                                .text_style(NotedeckTextStyle::Body.text_style()),
                            );
                            ui.separator();
                            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);

                            let current_zoom = ui.ctx().zoom_factor();

                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(tr!(
                                        self.i18n,
                                        "Zoom Level:",
                                        "Label for zoom level, Appearance settings section"
                                    ))
                                    .text_style(NotedeckTextStyle::Small.text_style()),
                                );

                                if ui
                                    .button(
                                        RichText::new("-")
                                            .text_style(NotedeckTextStyle::Small.text_style()),
                                    )
                                    .clicked()
                                {
                                    let new_zoom = (current_zoom - 0.1).max(0.1);
                                    action = Some(SettingsAction::SetZoomFactor(new_zoom));
                                };

                                ui.label(
                                    RichText::new(format!("{:.0}%", current_zoom * 100.0))
                                        .text_style(NotedeckTextStyle::Small.text_style()),
                                );

                                if ui
                                    .button(
                                        RichText::new("+")
                                            .text_style(NotedeckTextStyle::Small.text_style()),
                                    )
                                    .clicked()
                                {
                                    let new_zoom = (current_zoom + 0.1).min(10.0);
                                    action = Some(SettingsAction::SetZoomFactor(new_zoom));
                                };

                                if ui
                                    .button(
                                        RichText::new(tr!(
                                            self.i18n,
                                            "Reset",
                                            "Label for reset zoom level, Appearance settings section"
                                        ))
                                        .text_style(NotedeckTextStyle::Small.text_style()),
                                    )
                                    .clicked()
                                {
                                    action = Some(SettingsAction::SetZoomFactor(1.0));
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(tr!(
                                            self.i18n,
                                            "Language:",
                                            "Label for language, Appearance settings section"
                                        ))
                                        .text_style(NotedeckTextStyle::Small.text_style()),
                                );
                                ComboBox::from_label("")
                                    .selected_text(self.get_selected_language_name())
                                    .show_ui(ui, |ui| {
                                        for lang in self.i18n.get_available_locales() {
                                            let name = self.i18n
                                                .get_locale_native_name(lang)
                                                .map(|s| s.to_owned())
                                                .unwrap_or_else(|| lang.to_string());
                                            if ui
                                                .selectable_value(
                                                    self.selected_language,
                                                    lang.to_string(),
                                                    name,
                                                )
                                                .clicked()
                                            {
                                                action = Some(SettingsAction::SetLocale(lang.to_owned()))
                                            }
                                        }
                                    })
                            });

                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(tr!(
                                            self.i18n,
                                            "Theme:",
                                            "Label for theme, Appearance settings section"
                                        ))
                                        .text_style(NotedeckTextStyle::Small.text_style()),
                                );
                                if ui
                                    .selectable_value(
                                        self.theme,
                                        "Light".into(),
                                        RichText::new(tr!(
                                            self.i18n,
                                            "Light",
                                            "Label for Theme Light, Appearance settings section"
                                        ))
                                            .text_style(NotedeckTextStyle::Small.text_style()),
                                    )
                                    .clicked()
                                {
                                    action = Some(SettingsAction::SetTheme(ThemePreference::Light));
                                }
                                if ui
                                    .selectable_value(
                                        self.theme,
                                        "Dark".into(),
                                        RichText::new(tr!(
                                            self.i18n,
                                            "Dark",
                                            "Label for Theme Dark, Appearance settings section"
                                        ))
                                            .text_style(NotedeckTextStyle::Small.text_style()),
                                    )
                                    .clicked()
                                {
                                    action = Some(SettingsAction::SetTheme(ThemePreference::Dark));
                                }
                            });
                        });
                    });

                ui.add_space(5.0);

                Frame::group(ui.style())
                    .fill(ui.style().visuals.widgets.open.bg_fill)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(tr!(
                                            self.i18n,
                                            "Storage",
                                            "Label for storage settings section"
                                        ))
                                .text_style(NotedeckTextStyle::Body.text_style()),
                        );
                        ui.separator();

                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);

                            ui.horizontal_wrapped(|ui| {
                                let static_imgs_size = self
                                    .img_cache
                                    .static_imgs
                                    .cache_size
                                    .lock()
                                    .unwrap();

                                let gifs_size = self.img_cache.gifs.cache_size.lock().unwrap();

                                ui.label(
                                    RichText::new(format!("{} {}",
                                        tr!(
                                            self.i18n,
                                            "Image cache size:",
                                            "Label for Image cache size, Storage settings section"
                                        ),
                                        format_size(
                                            [static_imgs_size, gifs_size]
                                                .iter()
                                                .fold(0_u64, |acc, cur| acc
                                                    + cur.unwrap_or_default())
                                        )
                                    ))
                                    .text_style(NotedeckTextStyle::Small.text_style()),
                                );

                                ui.end_row();

                                if !notedeck::ui::is_compiled_as_mobile() &&
                                    ui.button(RichText::new(tr!(self.i18n, "View folder", "Label for view folder button, Storage settings section"))
                                        .text_style(NotedeckTextStyle::Small.text_style())).clicked() {
                                    action = Some(SettingsAction::OpenCacheFolder);
                                }

                                let clearcache_resp = ui.button(
                                    RichText::new(tr!(
                                            self.i18n,
                                            "Clear cache",
                                            "Label for clear cache button, Storage settings section"
                                        ))
                                        .text_style(NotedeckTextStyle::Small.text_style())
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
                                            self.i18n,
                                            "Confirm",
                                            "Label for confirm clear cache, Storage settings section"
                                        ));
                                        if confirm_resp.clicked() {
                                            confirm_pressed = true;
                                        }

                                        if confirm_resp.clicked() || ui.button(tr!(
                                            self.i18n,
                                            "Cancel",
                                            "Label for cancel clear cache, Storage settings section"
                                        )).clicked() {
                                            ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                                        }
                                    });

                                    if confirm_pressed {
                                        action = Some(SettingsAction::ClearCacheFolder);
                                    } else if !confirm_pressed
                                        && clearcache_resp.clicked_elsewhere()
                                    {
                                        ui.data_mut(|d| d.insert_temp(id_clearcache, false));
                                    }
                                };
                            });
                        });
                    });

                ui.add_space(5.0);

                Frame::group(ui.style())
                    .fill(ui.style().visuals.widgets.open.bg_fill)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(tr!(
                                            self.i18n,
                                            "Others",
                                         "Label for others settings section"
                                        ))
                                .text_style(NotedeckTextStyle::Body.text_style()),
                        );
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(
                                    tr!(
                                            self.i18n,
                                            "Show source client",
                                            "Label for Show source client, others settings section"
                                        ))
                                        .text_style(NotedeckTextStyle::Small.text_style()),
                                );

                                for option in [
                                    ShowSourceClientOption::Hide,
                                    ShowSourceClientOption::Top,
                                    ShowSourceClientOption::Bottom,
                                ] {
                                    let label = self.get_show_note_client_label(option);

                                    if ui
                                        .selectable_value(
                                            self.show_note_client,
                                            option,
                                            RichText::new(label)
                                                .text_style(NotedeckTextStyle::Small.text_style()),
                                        )
                                        .changed()
                                    {
                                        action = Some(SettingsAction::SetShowSourceClient(option));
                                    }
                                }
                            });
                        });
                    });

                ui.add_space(10.0);

                if ui
                    .add_sized(
                        [ui.available_width(), 30.0],
                        Button::new(
                            RichText::new(tr!(
                                            self.i18n,
                                            "Configure relays",
                                            "Label for configure relays, settings section"
                                        ))
                                .text_style(NotedeckTextStyle::Small.text_style()),
                        ),
                    )
                    .clicked()
                {
                    action = Some(SettingsAction::OpenRelays);
                }
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
