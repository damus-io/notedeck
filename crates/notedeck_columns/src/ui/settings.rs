use egui::{vec2, Button, Color32, ComboBox, Frame, Margin, RichText, ScrollArea, ThemePreference};
use notedeck::{
    tr, Images, LanguageIdentifier, Localization, NotedeckTextStyle, Settings, SettingsHandler,
};
use notedeck_ui::NoteOptions;
use strum::Display;

use crate::{nav::RouterAction, Damus, Route};

const THEME_LIGHT: &str = "Light";
const THEME_DARK: &str = "Dark";

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 3.0;
const ZOOM_STEP: f32 = 0.1;
const RESET_ZOOM: f32 = 1.0;

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

    fn label<'a>(&self, i18n: &'a mut Localization) -> String {
        match self {
            Self::Hide => tr!(i18n, "Hide", "Option in settings section to hide the source client label in note display"),
            Self::Top => tr!(i18n, "Top", "Option in settings section to show the source client label at the top of the note"),
            Self::Bottom => tr!(i18n, "Bottom", "Option in settings section to show the source client label at the bottom of the note"),
        }
    }
}

pub enum SettingsAction {
    SetZoomFactor(f32),
    SetTheme(ThemePreference),
    SetShowSourceClient(ShowSourceClientOption),
    SetLocale(LanguageIdentifier),
    SetRepliestNewestFirst(bool),
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
            Self::OpenRelays => {
                route_action = Some(RouterAction::route_to(Route::Relays));
            }
            Self::SetZoomFactor(zoom_factor) => {
                ctx.set_zoom_factor(zoom_factor);
                settings_handler.set_zoom_factor(zoom_factor);
            }
            Self::SetShowSourceClient(option) => {
                option.set_note_options(&mut app.note_options);

                settings_handler.set_show_source_client(option);
            }
            Self::SetTheme(theme) => {
                ctx.set_theme(theme);
                settings_handler.set_theme(theme);
            }
            Self::SetLocale(language) => {
                if i18n.set_locale(language.clone()).is_ok() {
                    settings_handler.set_locale(language.to_string());
                }
            }
            Self::SetRepliestNewestFirst(value) => {
                app.note_options.set(NoteOptions::RepliesNewestFirst, value);
                settings_handler.set_show_replies_newest_first(value);
                settings_handler.save();
            }
            Self::OpenCacheFolder => {
                use opener;
                let _ = opener::open(img_cache.base_path.clone());
            }
            Self::ClearCacheFolder => {
                let _ = img_cache.clear_folder_contents();
            }
        }
        settings_handler.save();
        route_action
    }
}

pub struct SettingsView<'a> {
    settings: &'a mut Settings,
    i18n: &'a mut Localization,
    img_cache: &'a mut Images,
}

fn small_richtext(i18n: &'_ mut Localization, text: &str, comment: &str) -> RichText {
    RichText::new(tr!(i18n, text, comment)).text_style(NotedeckTextStyle::Small.text_style())
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
        i18n: &'a mut Localization,
        img_cache: &'a mut Images,
        settings: &'a mut Settings,
    ) -> Self {
        Self {
            settings,
            img_cache,
            i18n,
        }
    }

    /// Get the localized name for a language identifier
    fn get_selected_language_name(&mut self) -> String {
        if let Ok(lang_id) = self.settings.locale.parse::<LanguageIdentifier>() {
            self.i18n
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
            self.i18n,
            "Appearance",
            "Label for appearance settings section",
        );
        settings_group(ui, title, |ui| {
            let current_zoom = ui.ctx().zoom_factor();

            ui.horizontal(|ui| {
                ui.label(small_richtext(
                    self.i18n,
                    "Zoom Level:",
                    "Label for zoom level, Appearance settings section",
                ));

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
                    .button(small_richtext(
                        self.i18n,
                        "Reset",
                        "Label for reset zoom level, Appearance settings section",
                    ))
                    .clicked()
                {
                    action = Some(SettingsAction::SetZoomFactor(RESET_ZOOM));
                }
            });

            ui.horizontal(|ui| {
                ui.label(small_richtext(
                    self.i18n,
                    "Language:",
                    "Label for language, Appearance settings section",
                ));

                //
                ComboBox::from_label("")
                    .selected_text(self.get_selected_language_name())
                    .show_ui(ui, |ui| {
                        for lang in self.i18n.get_available_locales() {
                            let name = self
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
                ui.label(small_richtext(
                    self.i18n,
                    "Theme:",
                    "Label for theme, Appearance settings section",
                ));

                if ui
                    .selectable_value(
                        &mut self.settings.theme,
                        ThemePreference::Light,
                        small_richtext(
                            self.i18n,
                            THEME_LIGHT.into(),
                            "Label for Theme Light, Appearance settings section",
                        ),
                    )
                    .clicked()
                {
                    action = Some(SettingsAction::SetTheme(ThemePreference::Light));
                }

                if ui
                    .selectable_value(
                        &mut self.settings.theme,
                        ThemePreference::Dark,
                        small_richtext(
                            self.i18n,
                            THEME_DARK.into(),
                            "Label for Theme Dark, Appearance settings section",
                        ),
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
        let title = tr!(self.i18n, "Storage", "Label for storage settings section");
        settings_group(ui, title, |ui| {
            ui.horizontal_wrapped(|ui| {
                let static_imgs_size = self.img_cache.static_imgs.cache_size.lock().unwrap();

                let gifs_size = self.img_cache.gifs.cache_size.lock().unwrap();

                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        tr!(
                            self.i18n,
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
                        .button(small_richtext(
                            self.i18n,
                            "View folder",
                            "Label for view folder button, Storage settings section",
                        ))
                        .clicked()
                {
                    action = Some(SettingsAction::OpenCacheFolder);
                }

                let clearcache_resp = ui.button(
                    small_richtext(
                        self.i18n,
                        "Clear cache",
                        "Label for clear cache button, Storage settings section",
                    )
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

                        if confirm_resp.clicked()
                            || ui
                                .button(tr!(
                                    self.i18n,
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

        let title = tr!(self.i18n, "Others", "Label for others settings section");
        settings_group(ui, title, |ui| {
            ui.horizontal(|ui| {
                ui.label(small_richtext(
                    self.i18n,
                    "Sort replies newest first",
                    "Label for Sort replies newest first, others settings section",
                ));

                if ui
                    .toggle_value(
                        &mut self.settings.show_replies_newest_first,
                        RichText::new(tr!(self.i18n, "ON", "ON"))
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
                ui.label(small_richtext(
                    self.i18n,
                    "Source client",
                    "Label for Source client, others settings section",
                ));

                for option in [
                    ShowSourceClientOption::Hide,
                    ShowSourceClientOption::Top,
                    ShowSourceClientOption::Bottom,
                ] {
                    let mut current: ShowSourceClientOption =
                        self.settings.show_source_client.clone().into();

                    if ui
                        .selectable_value(
                            &mut current,
                            option,
                            RichText::new(option.label(self.i18n))
                                .text_style(NotedeckTextStyle::Small.text_style()),
                        )
                        .changed()
                    {
                        action = Some(SettingsAction::SetShowSourceClient(option));
                    }
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
                Button::new(small_richtext(
                    self.i18n,
                    "Configure relays",
                    "Label for configure relays, settings section",
                )),
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
