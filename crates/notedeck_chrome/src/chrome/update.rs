//! The `auto-update` feature's chrome side: polling the updater each frame and
//! the "Update to …" item it adds to the sidebar, or the muted "Update can't
//! install" item that says why when it can't.

use super::ChromePanelAction;
use egui::Color32;
use notedeck::NotedeckTextStyle;
#[cfg(feature = "auto-update")]
use notedeck::{tr, Localization};

#[cfg(feature = "auto-update")]
pub(super) fn poll_updater(
    updater: &mut notedeck::updater::Updater,
    ctx: &mut notedeck::AppContext,
) {
    // Sync release channel from settings (cheap string compare, only parses on change)
    let setting_str = ctx.settings.release_channel();
    if setting_str != updater.channel().as_str() {
        if let Some(ch) = notedeck::updater::nostr::ReleaseChannel::parse(setting_str) {
            updater.set_channel(ch);
        }
    }

    if updater.needs_relay_sub() {
        tracing::debug!("updater: sending release filter to relays");
        let release_pubkey = *updater.release_pubkey();
        let filters = notedeck::updater::nostr::release_filter(&release_pubkey);
        let mut oneshot = ctx.remote.oneshot();
        oneshot.oneshot(filters);
    }

    if updater.wants_release() {
        let release_sub = updater.release_sub();
        let nks = ctx.ndb.poll_for_notes(release_sub, 10);
        if !nks.is_empty() {
            tracing::debug!(
                "updater: got {} new note(s) from release subscription",
                nks.len()
            );
            updater.note_received();
        }
    }
    updater.check_gathering(ctx.ndb);
    updater.poll(ctx.ndb);
}

/// Where a user installs Notedeck by hand when the update can't install
/// itself over the running copy.
#[cfg(feature = "auto-update")]
pub(super) const DOWNLOAD_PAGE_URL: &str = "https://damus.io/notedeck/";

/// The sidebar's update item: "Update to <version>" once an update is staged,
/// or a muted "Update can't install" item when it can't be installed here,
/// whose hover says why, whose click opens the download page and whose
/// right-click hides it.
#[cfg(feature = "auto-update")]
pub(super) fn update_sidebar_item_ui(
    updater: &notedeck::updater::Updater,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<ChromePanelAction> {
    if let Some(reason) = updater.install_problem() {
        return install_problem_item_ui(reason, i18n, ui);
    }

    let version = updater.update_ready()?;

    let accent = notedeck_ui::colors::PINK;
    let colors = ItemColors {
        bg: accent,
        hovered_bg: accent.gamma_multiply(0.9),
        icon: Color32::WHITE,
        text: Color32::WHITE,
    };
    let response = update_item(ui, colors, format!("Update to {version}"));

    if response.clicked() {
        Some(ChromePanelAction::ApplyUpdate)
    } else {
        None
    }
}

/// The muted item shown in place of the update button when the update can't
/// install over this copy. `reason` is borrowed from the updater's state.
#[cfg(feature = "auto-update")]
fn install_problem_item_ui(
    reason: &str,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<ChromePanelAction> {
    let visuals = ui.visuals();
    let colors = ItemColors {
        bg: visuals.widgets.inactive.weak_bg_fill,
        hovered_bg: visuals.widgets.hovered.weak_bg_fill,
        icon: visuals.weak_text_color(),
        text: visuals.text_color(),
    };
    let label = tr!(
        i18n,
        "Update can't install",
        "Sidebar item shown when a downloaded update can't be installed over the running app"
    );
    let response = update_item(ui, colors, label);

    let response = response.on_hover_ui(|ui| {
        ui.set_max_width(280.0);
        ui.label(reason);
        if cfg!(target_os = "macos") {
            ui.label(tr!(
                i18n,
                "Move Notedeck to your Applications folder, then reopen it.",
                "Hint under the reason an update can't install on macOS"
            ));
        }
        ui.label(tr!(
            i18n,
            "Click to download the new version, right-click to hide this.",
            "Hint that clicking the can't-install update item opens the download page and right-clicking hides it"
        ));
    });

    if response.clicked() {
        Some(ChromePanelAction::OpenUpdateDownloadPage)
    } else if response.secondary_clicked() {
        Some(ChromePanelAction::DismissUpdate)
    } else {
        None
    }
}

/// The fill, icon and text colors of a sidebar update item.
#[cfg(feature = "auto-update")]
struct ItemColors {
    bg: Color32,
    hovered_bg: Color32,
    icon: Color32,
    text: Color32,
}

/// Draw one 40pt sidebar update item: a rounded fill, the update arrow and
/// `text` in bold.
#[cfg(feature = "auto-update")]
fn update_item(ui: &mut egui::Ui, colors: ItemColors, text: String) -> egui::Response {
    let desired_size = egui::vec2(ui.available_width(), 40.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if !ui.is_rect_visible(rect) {
        return response;
    }

    let rounding = 8.0;
    let bg = if response.hovered() {
        colors.hovered_bg
    } else {
        colors.bg
    };
    ui.painter().rect_filled(rect, rounding, bg);

    let icon_size = 16.0;
    let icon_center = egui::pos2(rect.left() + 20.0, rect.center().y);
    notedeck_ui::icons::draw_update_icon(ui.painter(), icon_center, icon_size, colors.icon, 2.0);

    let text_pos = egui::pos2(rect.left() + 38.0, rect.center().y);
    let font = egui::FontId::new(
        notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
        egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
    );
    let galley = ui.painter().layout_no_wrap(text, font, colors.text);
    let text_y = text_pos.y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(text_pos.x, text_y), galley, colors.text);

    response
}
