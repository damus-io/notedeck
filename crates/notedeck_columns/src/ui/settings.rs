use egui::{vec2, Button, Color32, ComboBox, Frame, Margin, RichText, ThemePreference};
use notedeck::{tr, Images, LanguageIdentifier, Localization, NotedeckTextStyle, ThemeHandler};
use notedeck_ui::NoteOptions;
use strum::Display;

use crate::{nav::RouterAction, Damus, Route};

#[derive(Clone, Copy, PartialEq, Eq, Display)]
pub enum ShowNoteClientOptions {
    Hide,
    Top,
    Bottom,
}

pub enum SettingsAction {
    SetZoom(f32),
    SetTheme(ThemePreference),
    SetShowNoteClient(ShowNoteClientOptions),
    SetLocale(LanguageIdentifier),
    OpenRelays,
    OpenCacheFolder,
    ClearCacheFolder,
}

impl SettingsAction {
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        theme_handler: &'a mut ThemeHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
    ) -> Option<RouterAction> {
        let mut route_action: Option<RouterAction> = None;

        match self {
            SettingsAction::OpenRelays => {
                route_action = Some(RouterAction::route_to(Route::Relays));
            }
            SettingsAction::SetZoom(zoom_level) => {
                ctx.set_zoom_factor(zoom_level);
            }
            SettingsAction::SetShowNoteClient(newvalue) => match newvalue {
                ShowNoteClientOptions::Hide => {
                    app.note_options.set(NoteOptions::ShowNoteClientTop, false);
                    app.note_options
                        .set(NoteOptions::ShowNoteClientBottom, false);
                }
                ShowNoteClientOptions::Bottom => {
                    app.note_options.set(NoteOptions::ShowNoteClientTop, false);
                    app.note_options
                        .set(NoteOptions::ShowNoteClientBottom, true);
                }
                ShowNoteClientOptions::Top => {
                    app.note_options.set(NoteOptions::ShowNoteClientTop, true);
                    app.note_options
                        .set(NoteOptions::ShowNoteClientBottom, false);
                }
            },
            SettingsAction::SetTheme(theme) => {
                ctx.options_mut(|o| {
                    o.theme_preference = theme;
                });
                theme_handler.save(theme);
            }
            SettingsAction::SetLocale(language) => {
                _ = i18n.set_locale(language);
            }
            SettingsAction::OpenCacheFolder => {
                use opener;
                let _ = opener::open(img_cache.base_path.clone());
            }
            SettingsAction::ClearCacheFolder => {
                let _ = img_cache.clear_folder_contents();
            }
        }
        route_action
    }
}

pub struct SettingsView<'a> {
    theme: &'a mut String,
    selected_language: &'a mut String,
    show_note_client: &'a mut ShowNoteClientOptions,
    i18n: &'a mut Localization,
    img_cache: &'a mut Images,
}

impl<'a> SettingsView<'a> {
    pub fn new(
        img_cache: &'a mut Images,
        selected_language: &'a mut String,
        theme: &'a mut String,
        show_note_client: &'a mut ShowNoteClientOptions,
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
                                    action = Some(SettingsAction::SetZoom(new_zoom));
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
                                    action = Some(SettingsAction::SetZoom(new_zoom));
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
                                    action = Some(SettingsAction::SetZoom(1.0));
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
                                    .selected_text(self.selected_language.to_owned())
                                    .show_ui(ui, |ui| {
                                        for lang in self.i18n.get_available_locales() {
                                            if ui
                                                .selectable_value(
                                                    self.selected_language,
                                                    lang.to_string(),
                                                    lang.to_string(),
                                                )
                                                .clicked()
                                            {
                                                action =
                                                    Some(SettingsAction::SetLocale(lang.to_owned()))
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
                                    ui.button(RichText::new(tr!(self.i18n, "View folder:", "Label for view folder button, Storage settings section"))
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
                                    ShowNoteClientOptions::Hide,
                                    ShowNoteClientOptions::Top,
                                    ShowNoteClientOptions::Bottom,
                                ] {
                                    let label = option.clone().to_string();

                                    if ui
                                        .selectable_value(
                                            self.show_note_client,
                                            option,
                                            RichText::new(label)
                                                .text_style(NotedeckTextStyle::Small.text_style()),
                                        )
                                        .changed()
                                    {
                                        action = Some(SettingsAction::SetShowNoteClient(option));
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
