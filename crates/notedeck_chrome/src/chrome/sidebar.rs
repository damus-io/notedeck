//! The chrome side drawer: the account header, the Profile / Accounts / Wallet /
//! Settings / Theme / Support items, the app launcher list and the `--debug`
//! stats, plus the app labels and icons it shares with the tab strip.

use super::debug::repaint_causes_window;
#[cfg(feature = "memory")]
use super::debug::{format_bytes, memory_debug_ui};
#[cfg(feature = "auto-update")]
use super::update::update_sidebar_item_ui;
use super::{Chrome, ChromePanelAction};
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use bitflags::bitflags;
use egui::{vec2, CornerRadius, Label, Layout, RichText, Sense, ThemePreference, Ui, Widget};
use egui_extras::{Size, StripBuilder};
use nostrdb::{ProfileRecord, Transaction};
use notedeck::fonts::get_font_size;
use notedeck::name::get_display_name;
use notedeck::{tr, AppContext, Localization, NotedeckOptions, NotedeckTextStyle, UserAccount};
use notedeck_ui::{app_images, galley_centered_pos, ProfilePic};

#[cfg(feature = "dave")]
use egui::Rect;
#[cfg(feature = "dave")]
use notedeck_dave::DaveAvatar;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SidebarOptions: u8 {
        const Compact = 1 << 0;
    }
}

pub(super) fn milestone_name<'a>(i18n: &'a mut Localization) -> impl Widget + 'a {
    let text = if notedeck::ui::is_compiled_as_mobile() {
        tr!(
            i18n,
            "Damus Android BETA",
            "Damus android beta version label"
        )
    } else {
        tr!(
            i18n,
            "Damus Notedeck BETA",
            "Damus notedeck beta version label"
        )
    };

    |ui: &mut egui::Ui| -> egui::Response {
        let font = egui::FontId::new(
            notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Tiny),
            egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
        );
        ui.add(
            Label::new(
                RichText::new(text)
                    .color(ui.style().visuals.noninteractive().fg_stroke.color)
                    .font(font),
            )
            .selectable(false),
        )
        .on_hover_text(tr!(
            i18n,
            "Notedeck is a beta product. Expect bugs and contact us when you run into issues.",
            "Beta product warning message"
        ))
        .on_hover_cursor(egui::CursorIcon::Help)
    }
}

#[cfg(feature = "clndash")]
fn clndash_button(ui: &mut egui::Ui) -> egui::Response {
    notedeck_ui::expanding_button(
        "clndash-button",
        24.0,
        app_images::cln_image(),
        app_images::cln_image(),
        ui,
        false,
    )
}

#[cfg(feature = "dave")]
pub(super) fn dave_button(
    avatar: Option<&mut DaveAvatar>,
    ui: &mut egui::Ui,
    rect: Rect,
) -> egui::Response {
    if let Some(avatar) = avatar {
        avatar.render(rect, ui)
    } else {
        // plain icon if wgpu device not available??
        ui.label("fixme")
    }
}

/// The localized display name for a notedeck app, used in the sidebar and the
/// chrome tab strip.
pub(super) fn app_label(loc: &mut Localization, app: &NotedeckApp) -> String {
    match app {
        #[cfg(feature = "dave")]
        NotedeckApp::Dave(_) => tr!(loc, "Dave", "Button to go to the Dave app"),
        NotedeckApp::Columns(_) => tr!(loc, "Columns", "Button to go to the Columns app"),

        #[cfg(feature = "messages")]
        NotedeckApp::Messages(_) => tr!(loc, "Messaging", "Button to go to the messaging app"),

        #[cfg(feature = "dashboard")]
        NotedeckApp::Dashboard(_) => tr!(loc, "Dashboard", "Button to go to the dashboard app"),

        #[cfg(feature = "horizon")]
        NotedeckApp::Horizon(_) => tr!(loc, "Horizon", "Button to go to the Horizon app"),

        #[cfg(feature = "notebook")]
        NotedeckApp::Notebook(_) => tr!(loc, "Notebook", "Button to go to the Notebook app"),

        #[cfg(feature = "headway")]
        NotedeckApp::Headway(_) => tr!(loc, "Headway", "Button to go to the Headway app"),

        #[cfg(feature = "clndash")]
        NotedeckApp::ClnDash(_) => tr!(loc, "ClnDash", "Button to go to the ClnDash app"),

        #[cfg(feature = "nostrverse")]
        NotedeckApp::Nostrverse(_) => tr!(loc, "Nostrverse", "Button to go to the Nostrverse app"),

        NotedeckApp::Other(name, _) => tr!(loc, name.as_str(), "Button to go to a WASM app"),
    }
}

pub fn get_profile_url_owned(profile: Option<ProfileRecord<'_>>) -> &str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        notedeck::profile::no_pfp_url()
    }
}

pub fn get_account_url<'a>(
    txn: &'a nostrdb::Transaction,
    ndb: &nostrdb::Ndb,
    account: &UserAccount,
) -> &'a str {
    if let Ok(profile) = ndb.get_profile_by_pubkey(txn, account.key.pubkey.bytes()) {
        get_profile_url_owned(Some(profile))
    } else {
        get_profile_url_owned(None)
    }
}

/// The section of the chrome sidebar that starts at the
/// bottom and goes up
pub(super) fn topdown_sidebar(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
    options: SidebarOptions,
) -> Option<ChromePanelAction> {
    let previous_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.y = 12.0;

    let loc = &mut ctx.i18n;

    // macos needs a bit of space to make room for window
    // minimize/close buttons
    if cfg!(target_os = "macos") {
        ui.add_space(8.0);
    }

    let txn = Transaction::new(ctx.ndb).expect("should be able to create txn");
    let profile = ctx
        .ndb
        .get_profile_by_pubkey(&txn, ctx.accounts.get_selected_account().key.pubkey.bytes());

    let disp_name = get_display_name(profile.as_ref().ok());
    let name = if let Some(username) = disp_name.username {
        format!("@{username}")
    } else {
        disp_name.username_or_displayname().to_owned()
    };

    let selected_acc = ctx.accounts.get_selected_account();
    let profile_url = get_account_url(&txn, ctx.ndb, selected_acc);
    if let Ok(profile) = profile {
        get_profile_url_owned(Some(profile))
    } else {
        get_profile_url_owned(None)
    };

    let pfp_resp = ui
        .add(&mut ProfilePic::new(ctx.img_cache, ctx.media_jobs.sender(), profile_url).size(64.0));

    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(
            RichText::new(name)
                .color(ui.visuals().weak_text_color())
                .size(16.0),
        ));
    });

    if let Some(npub) = selected_acc.key.pubkey.npub() {
        if ui.add(copy_npub(&npub, 200.0)).clicked() {
            ui.ctx().copy_text(npub);
        }
    }

    // we skip this whole function in compact mode
    if options.contains(SidebarOptions::Compact) {
        return if pfp_resp.clicked() {
            Some(ChromePanelAction::Profile(
                ctx.accounts.get_selected_account().key.pubkey,
            ))
        } else {
            None
        };
    }

    let mut action = None;

    #[cfg(feature = "auto-update")]
    if let Some(update_action) = update_sidebar_item_ui(&chrome.updater, ui) {
        action = Some(update_action);
    }

    let theme = ui.ctx().theme();

    StripBuilder::new(ui)
        .sizes(Size::exact(40.0), 6)
        .clip(true)
        .vertical(|mut strip| {
            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        let profile_img = if ui.visuals().dark_mode {
                            app_images::profile_image()
                        } else {
                            app_images::profile_image().tint(ui.visuals().text_color())
                        }
                        .max_size(ui.available_size());
                        ui.add(profile_img);
                    },
                    tr!(loc, "Profile", "Button to go to the user's profile"),
                )
                .clicked()
                {
                    action = Some(ChromePanelAction::Profile(
                        ctx.accounts.get_selected_account().key.pubkey,
                    ));
                }
            });

            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        let account_img = if ui.visuals().dark_mode {
                            app_images::accounts_image()
                        } else {
                            app_images::accounts_image().tint(ui.visuals().text_color())
                        }
                        .max_size(ui.available_size());
                        ui.add(account_img);
                    },
                    tr!(loc, "Accounts", "Button to go to the accounts view"),
                )
                .clicked()
                {
                    action = Some(ChromePanelAction::Account);
                }
            });

            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        let img = if ui.visuals().dark_mode {
                            app_images::wallet_dark_image()
                        } else {
                            app_images::wallet_light_image()
                        };

                        ui.add(img);
                    },
                    tr!(loc, "Wallet", "Button to go to the wallet view"),
                )
                .clicked()
                {
                    action = Some(ChromePanelAction::Wallet);
                }
            });

            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        ui.add(if ui.visuals().dark_mode {
                            app_images::settings_dark_image()
                        } else {
                            app_images::settings_light_image()
                        });
                    },
                    tr!(loc, "Settings", "Button to go to the settings view"),
                )
                .clicked()
                {
                    action = Some(ChromePanelAction::Settings);
                }
            });

            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        let c = match theme {
                            egui::Theme::Dark => "🔆",
                            egui::Theme::Light => "🌒",
                        };

                        let painter = ui.painter();
                        let galley = painter.layout_no_wrap(
                            c.to_owned(),
                            NotedeckTextStyle::Heading3.get_font_id(ui.ctx()),
                            ui.visuals().text_color(),
                        );

                        painter.galley(
                            galley_centered_pos(&galley, ui.available_rect_before_wrap().center()),
                            galley,
                            ui.visuals().text_color(),
                        );
                    },
                    tr!(loc, "Theme", "Button to change the theme (light or dark)"),
                )
                .clicked()
                {
                    match theme {
                        egui::Theme::Dark => {
                            action = Some(ChromePanelAction::SaveTheme(ThemePreference::Light));
                        }
                        egui::Theme::Light => {
                            action = Some(ChromePanelAction::SaveTheme(ThemePreference::Dark));
                        }
                    }
                }
            });

            strip.strip(|b| {
                if drawer_item(
                    b,
                    |ui| {
                        ui.add(if ui.visuals().dark_mode {
                            app_images::help_dark_image()
                        } else {
                            app_images::help_light_image()
                        });
                    },
                    tr!(loc, "Support", "Button to go to the support view"),
                )
                .clicked()
                {
                    action = Some(ChromePanelAction::Support);
                }
            });
        });

    // Scroll the app list so it doesn't overflow the sidebar as more apps
    // are added. Reserve a bit of space at the bottom for the milestone label.
    let apps_scroll_height = (ui.available_height() - 32.0).max(0.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(apps_scroll_height)
        .show(ui, |ui| {
            // `set_active` needs `&mut chrome`, which the icon loop already
            // borrows mutably — record the click and apply it after the loop.
            let mut clicked_ind: Option<i32> = None;

            for (i, app) in chrome.apps.iter_mut().enumerate() {
                if chrome.active == i as i32 {
                    continue;
                }

                let text = app_label(loc, app);

                StripBuilder::new(ui)
                    .size(Size::exact(40.0))
                    .clip(true)
                    .vertical(|mut strip| {
                        strip.strip(|b| {
                            let resp = drawer_item(
                                b,
                                |ui| match app {
                                    NotedeckApp::Columns(_columns_app) => {
                                        ui.add(app_images::columns_image());
                                    }

                                    #[cfg(feature = "dave")]
                                    NotedeckApp::Dave(dave) => {
                                        dave_button(
                                            dave.avatar_mut(),
                                            ui,
                                            Rect::from_center_size(
                                                ui.available_rect_before_wrap().center(),
                                                vec2(30.0, 30.0),
                                            ),
                                        );
                                    }

                                    #[cfg(feature = "dashboard")]
                                    NotedeckApp::Dashboard(_columns_app) => {
                                        notedeck_ui::icons::dashboard_icon(ui, 24.0);
                                    }

                                    #[cfg(feature = "horizon")]
                                    NotedeckApp::Horizon(_horizon) => {
                                        notedeck_ui::icons::horizon_icon(ui, 24.0);
                                    }

                                    #[cfg(feature = "messages")]
                                    NotedeckApp::Messages(_dms) => {
                                        notedeck_ui::icons::messages_icon(ui, 24.0);
                                    }

                                    #[cfg(feature = "clndash")]
                                    NotedeckApp::ClnDash(_clndash) => {
                                        clndash_button(ui);
                                    }

                                    #[cfg(feature = "notebook")]
                                    NotedeckApp::Notebook(_notebook) => {
                                        notedeck_ui::icons::notebook_icon(ui, 24.0);
                                    }

                                    #[cfg(feature = "headway")]
                                    NotedeckApp::Headway(_headway) => {
                                        notedeck_ui::icons::headway_icon(ui, 24.0);
                                    }

                                    #[cfg(feature = "nostrverse")]
                                    NotedeckApp::Nostrverse(_nostrverse) => {
                                        ui.add(app_images::universe_image());
                                    }

                                    NotedeckApp::Other(_name, _other) => {
                                        ui.label("W");
                                    }
                                },
                                text,
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);

                            if resp.clicked() {
                                clicked_ind = Some(i as i32);
                                chrome.nav.close();
                            }
                        })
                    });
            }

            if let Some(i) = clicked_ind {
                chrome.set_active(i);
            }
        });

    if ctx.args.options.contains(NotedeckOptions::Debug) {
        let r = ui
            .weak(format!("{}", ctx.frame_history.fps() as i32))
            .union(ui.weak(format!(
                "{:10.1}",
                ctx.frame_history.mean_frame_time() * 1e3
            )))
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        if r.clicked() {
            chrome.options.toggle(ChromeOptions::RepaintDebug);
        }

        if chrome.options.contains(ChromeOptions::RepaintDebug) {
            for cause in ui.ctx().repaint_causes() {
                chrome
                    .repaint_causes
                    .entry(cause)
                    .and_modify(|rc| {
                        *rc += 1;
                    })
                    .or_insert(1);
            }
            repaint_causes_window(ui, &chrome.repaint_causes)
        }

        #[cfg(feature = "memory")]
        {
            let mem_use = re_memory::MemoryUse::capture();
            if let Some(counted) = mem_use.counted {
                if ui
                    .label(format!("{}", format_bytes(counted as f64)))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    chrome.options.toggle(ChromeOptions::MemoryDebug);
                }
            }
            if let Some(resident) = mem_use.resident {
                ui.weak(format!("{}", format_bytes(resident as f64)));
            }

            if chrome.options.contains(ChromeOptions::MemoryDebug) {
                egui::Window::new("Memory Debug").show(ui.ctx(), memory_debug_ui);
            }
        }
    }

    ui.spacing_mut().item_spacing = previous_spacing;

    action
}

fn drawer_item(builder: StripBuilder, icon: impl FnOnce(&mut Ui), text: String) -> egui::Response {
    builder
        .cell_layout(Layout::left_to_right(egui::Align::Center))
        .sense(Sense::click())
        .size(Size::exact(24.0))
        .size(Size::exact(8.0)) // free space
        .size(Size::remainder())
        .horizontal(|mut strip| {
            strip.cell(icon);

            strip.empty();

            strip.cell(|ui| {
                ui.add(drawer_label(ui.ctx(), &text));
            });
        })
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn drawer_label(ctx: &egui::Context, text: &str) -> egui::Label {
    egui::Label::new(RichText::new(text).size(get_font_size(ctx, &NotedeckTextStyle::Heading2)))
        .selectable(false)
}

fn copy_npub<'a>(npub: &'a String, width: f32) -> impl Widget + use<'a> {
    move |ui: &mut egui::Ui| -> egui::Response {
        let size = vec2(width, 24.0);
        let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
        resp = resp.on_hover_cursor(egui::CursorIcon::Copy);

        let painter = ui.painter_at(rect);

        painter.rect_filled(
            rect,
            CornerRadius::same(32),
            if resp.hovered() {
                ui.visuals().widgets.active.bg_fill
            } else {
                // ui.visuals().panel_fill
                ui.visuals().widgets.inactive.bg_fill
            },
        );

        let text =
            Label::new(RichText::new(npub).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Tiny)))
                .truncate()
                .selectable(false);

        let (label_rect, copy_rect) = {
            let rect = rect.shrink(4.0);
            let (l, r) = rect.split_left_right_at_x(rect.right() - 24.0);
            (l, r.shrink2(vec2(4.0, 0.0)))
        };

        app_images::copy_to_clipboard_image()
            .tint(ui.visuals().text_color())
            .maintain_aspect_ratio(true)
            // .max_size(vec2(24.0, 24.0))
            .paint_at(ui, copy_rect);

        ui.put(label_rect, text);

        resp
    }
}
