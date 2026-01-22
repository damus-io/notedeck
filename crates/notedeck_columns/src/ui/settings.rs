use egui::{
    vec2, Button, Color32, ComboBox, CornerRadius, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, TextEdit, ThemePreference,
};
use egui_extras::{Size, StripBuilder};
use enostr::NoteId;
use nostrdb::Transaction;
use notedeck::{
    tr, ui::richtext_small, DragResponse, Images, LanguageIdentifier, Localization, NoteContext,
    NotedeckTextStyle, Settings, SettingsHandler, DEFAULT_MAX_HASHTAGS_PER_NOTE,
    DEFAULT_NOTE_BODY_FONT_SIZE,
};

#[cfg(not(target_os = "android"))]
use notedeck::notifications::NotificationManager;
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
    EnableNotifications,
    DisableNotifications,
    RequestNotificationPermission,
}

impl SettingsAction {
    /// Process a settings action on Android.
    #[cfg(target_os = "android")]
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        settings: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
        accounts: &mut notedeck::Accounts,
    ) -> Option<RouterAction> {
        Self::process_settings_action_impl(self, app, settings, i18n, img_cache, ctx, accounts, None)
    }

    /// Process a settings action on desktop (requires NotificationManager).
    #[cfg(not(target_os = "android"))]
    #[allow(clippy::too_many_arguments)]
    pub fn process_settings_action<'a>(
        self,
        app: &mut Damus,
        settings: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
        accounts: &mut notedeck::Accounts,
        notification_manager: &mut Option<NotificationManager>,
    ) -> Option<RouterAction> {
        Self::process_settings_action_impl(
            self,
            app,
            settings,
            i18n,
            img_cache,
            ctx,
            accounts,
            Some(notification_manager),
        )
    }

    /// Internal implementation shared between platforms.
    #[allow(clippy::too_many_arguments)]
    fn process_settings_action_impl<'a>(
        self,
        app: &mut Damus,
        settings: &'a mut SettingsHandler,
        i18n: &'a mut Localization,
        img_cache: &mut Images,
        ctx: &egui::Context,
        accounts: &mut notedeck::Accounts,
        #[cfg(not(target_os = "android"))] notification_manager: Option<
            &mut Option<NotificationManager>,
        >,
        #[cfg(target_os = "android")] _notification_manager: Option<()>,
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
            Self::EnableNotifications => {
                let pubkey = accounts.selected_account_pubkey();
                let relay_urls = accounts.get_selected_account_relay_urls();
                #[cfg(target_os = "android")]
                let result = notedeck::platform::enable_notifications(&pubkey.hex(), &relay_urls);
                #[cfg(not(target_os = "android"))]
                let result = {
                    let mgr = notification_manager.expect("notification_manager required on desktop");
                    notedeck::platform::enable_notifications(mgr, &pubkey.hex(), &relay_urls)
                };
                if let Err(e) = result {
                    tracing::error!("Failed to enable notifications: {}", e);
                }
            }
            Self::DisableNotifications => {
                #[cfg(target_os = "android")]
                let result = notedeck::platform::disable_notifications();
                #[cfg(not(target_os = "android"))]
                let result = {
                    let mgr = notification_manager.expect("notification_manager required on desktop");
                    notedeck::platform::disable_notifications(mgr)
                };
                if let Err(e) = result {
                    tracing::error!("Failed to disable notifications: {}", e);
                }
            }
            Self::RequestNotificationPermission => {
                if let Err(e) = notedeck::platform::request_notification_permission() {
                    tracing::error!("Failed to request notification permission: {}", e);
                }
            }
        }
        route_action
    }
}

pub struct SettingsView<'a> {
    settings: &'a mut Settings,
    note_context: &'a mut NoteContext<'a>,
    note_options: &'a mut NoteOptions,
    /// Whether the notification service is actually running (desktop only).
    /// Used to show accurate UI state even if persisted setting differs.
    #[cfg(not(target_os = "android"))]
    notifications_running: bool,
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

// =============================================================================
// Shadcn-inspired notification UI components
// =============================================================================

/// Alert component - displays informational message with icon.
/// Follows shadcn Alert design with rounded corners and subtle background.
fn notification_alert(ui: &mut egui::Ui, i18n: &mut Localization) {
    let alert_bg = if ui.visuals().dark_mode {
        Color32::from_rgb(30, 41, 59) // slate-800
    } else {
        Color32::from_rgb(241, 245, 249) // slate-100
    };

    let alert_border = if ui.visuals().dark_mode {
        Color32::from_rgb(51, 65, 85) // slate-700
    } else {
        Color32::from_rgb(203, 213, 225) // slate-300
    };

    Frame::new()
        .fill(alert_bg)
        .stroke(egui::Stroke::new(1.0, alert_border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Info icon (using text for now, could use an image)
                ui.label(RichText::new("ℹ").size(16.0));

                ui.add_space(8.0);

                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(tr!(
                            i18n,
                            "Stay connected",
                            "Alert title for notifications"
                        ))
                        .text_style(NotedeckTextStyle::Body.text_style())
                        .strong(),
                    );
                    ui.label(richtext_small(tr!(
                        i18n,
                        "Get notified about mentions, replies, reactions, and zaps even when the app is closed.",
                        "Alert description for notifications"
                    )));
                });
            });
        });
}

/// Badge component - displays status with colored background.
/// Follows shadcn Badge design: pill shape, colored background.
fn notification_badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    let text_color = if color.r() as u16 + color.g() as u16 + color.b() as u16 > 382 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };

    Frame::new()
        .fill(color)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .text_style(NotedeckTextStyle::Small.text_style())
                    .color(text_color),
            );
        });
}

/// Toggle switch component - follows shadcn Switch design.
/// 44px touch target with animated track and thumb.
fn notification_toggle(
    ui: &mut egui::Ui,
    is_on: bool,
    enabled: bool,
    i18n: &mut Localization,
) -> egui::Response {
    let desired_size = vec2(52.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let animation_progress = ui.ctx().animate_bool_responsive(response.id, is_on);

        // Track colors
        let (track_off, track_on) = if ui.visuals().dark_mode {
            (
                Color32::from_rgb(51, 65, 85),   // slate-700
                Color32::from_rgb(139, 92, 246), // violet-500
            )
        } else {
            (
                Color32::from_rgb(203, 213, 225), // slate-300
                Color32::from_rgb(124, 58, 237),  // violet-600
            )
        };

        let track_color = if !enabled {
            ui.visuals().gray_out(track_off)
        } else {
            Color32::from_rgba_unmultiplied(
                (track_off.r() as f32
                    + (track_on.r() as f32 - track_off.r() as f32) * animation_progress)
                    as u8,
                (track_off.g() as f32
                    + (track_on.g() as f32 - track_off.g() as f32) * animation_progress)
                    as u8,
                (track_off.b() as f32
                    + (track_on.b() as f32 - track_off.b() as f32) * animation_progress)
                    as u8,
                255,
            )
        };

        // Draw track (rounded rect)
        let track_radius = (rect.height() / 2.0) as u8;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(track_radius), track_color);

        // Draw thumb (circle)
        let thumb_radius = (rect.height() - 4.0) / 2.0;
        let thumb_x = egui::lerp(
            rect.left() + thumb_radius + 2.0..=rect.right() - thumb_radius - 2.0,
            animation_progress,
        );
        let thumb_center = egui::pos2(thumb_x, rect.center().y);
        let thumb_color = if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(200)
        };

        ui.painter()
            .circle_filled(thumb_center, thumb_radius, thumb_color);

        // Add subtle shadow to thumb
        if enabled {
            ui.painter().circle_stroke(
                thumb_center,
                thumb_radius,
                egui::Stroke::new(1.0, Color32::from_black_alpha(20)),
            );
        }
    }

    // Accessibility: add tooltip
    response.on_hover_text(if is_on {
        tr!(
            i18n,
            "Click to disable notifications",
            "Toggle tooltip when on"
        )
    } else {
        tr!(
            i18n,
            "Click to enable notifications",
            "Toggle tooltip when off"
        )
    })
}

impl<'a> SettingsView<'a> {
    #[cfg(target_os = "android")]
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

    #[cfg(not(target_os = "android"))]
    pub fn new(
        settings: &'a mut Settings,
        note_context: &'a mut NoteContext<'a>,
        note_options: &'a mut NoteOptions,
        notifications_running: bool,
    ) -> Self {
        Self {
            settings,
            note_context,
            note_options,
            notifications_running,
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

    /// Notifications section - only shown on supported platforms.
    /// Uses shadcn-inspired design: 44px touch targets, card containers, badges.
    fn notifications_section(&mut self, ui: &mut egui::Ui) -> Option<SettingsAction> {
        // Only show notifications on supported platforms
        if !notedeck::platform::supports_notifications() {
            return None;
        }

        let mut action = None;

        // On desktop, use actual running state; on Android, use persisted setting
        #[cfg(not(target_os = "android"))]
        let notifications_enabled = self.notifications_running;
        #[cfg(target_os = "android")]
        let notifications_enabled = self.settings.notifications_enabled;

        let title = tr!(
            self.note_context.i18n,
            "Notifications",
            "Label for notifications settings section"
        );

        settings_group(ui, title, |ui| {
            // Check current notification permission state
            let permission_granted =
                notedeck::platform::is_notification_permission_granted().unwrap_or(false);
            let permission_pending = notedeck::platform::is_notification_permission_pending();

            // Info alert explaining the feature
            notification_alert(ui, self.note_context.i18n);

            ui.add_space(8.0);

            // Permission status with badge
            ui.horizontal(|ui| {
                ui.label(richtext_small(tr!(
                    self.note_context.i18n,
                    "Permission:",
                    "Label for notification permission status",
                )));

                // Badge-style permission indicator
                let (badge_text, badge_color) = if permission_granted {
                    (
                        tr!(
                            self.note_context.i18n,
                            "Granted",
                            "Badge text when permission granted"
                        ),
                        Color32::from_rgb(34, 197, 94), // Green
                    )
                } else if permission_pending {
                    (
                        tr!(
                            self.note_context.i18n,
                            "Pending",
                            "Badge text when permission pending"
                        ),
                        Color32::from_rgb(234, 179, 8), // Yellow
                    )
                } else {
                    (
                        tr!(
                            self.note_context.i18n,
                            "Required",
                            "Badge text when permission required"
                        ),
                        Color32::from_rgb(239, 68, 68), // Red
                    )
                };

                notification_badge(ui, &badge_text, badge_color);
            });

            ui.add_space(8.0);

            // Permission request button - 44px min touch target
            if !permission_granted && !permission_pending {
                let btn_resp = ui.add_sized(
                    [ui.available_width(), 44.0],
                    Button::new(
                        RichText::new(tr!(
                            self.note_context.i18n,
                            "Grant Notification Permission",
                            "Button to request notification permission",
                        ))
                        .text_style(NotedeckTextStyle::Body.text_style()),
                    ),
                );

                if btn_resp.clicked() {
                    action = Some(SettingsAction::RequestNotificationPermission);
                }

                ui.add_space(8.0);
            }

            // Toggle switch row - 44px min touch target
            ui.horizontal(|ui| {
                ui.set_min_height(44.0);

                ui.label(
                    RichText::new(tr!(
                        self.note_context.i18n,
                        "Push notifications",
                        "Label for push notifications toggle",
                    ))
                    .text_style(NotedeckTextStyle::Body.text_style()),
                );

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let toggle_enabled = permission_granted && !permission_pending;

                    // Custom toggle switch styled like shadcn Switch
                    let resp = notification_toggle(
                        ui,
                        notifications_enabled,
                        toggle_enabled,
                        self.note_context.i18n,
                    );

                    if resp.clicked() && toggle_enabled {
                        action = if notifications_enabled {
                            Some(SettingsAction::DisableNotifications)
                        } else {
                            Some(SettingsAction::EnableNotifications)
                        };
                    }
                });
            });

            // Status description
            ui.add_space(4.0);
            let status_text = if !permission_granted {
                tr!(
                    self.note_context.i18n,
                    "Grant permission to enable notifications",
                    "Help text when permission not granted"
                )
            } else if notifications_enabled {
                tr!(
                    self.note_context.i18n,
                    "Receiving mentions, replies, reactions, and zaps",
                    "Help text when notifications enabled"
                )
            } else {
                tr!(
                    self.note_context.i18n,
                    "Notifications are disabled",
                    "Help text when notifications disabled"
                )
            };
            ui.label(
                richtext_small(&status_text)
                    .color(ui.visuals().gray_out(ui.visuals().text_color())),
            );
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

    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<SettingsAction> {
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

                    ui.add_space(5.0);

                    if let Some(new_action) = self.notifications_section(ui) {
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

        DragResponse::scroll(scroll_out)
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
