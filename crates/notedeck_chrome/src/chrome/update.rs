//! The `auto-update` feature's chrome side: polling the updater each frame and
//! the "Update to …" item it adds to the sidebar.

use super::ChromePanelAction;
use egui::Color32;
use notedeck::NotedeckTextStyle;

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

#[cfg(feature = "auto-update")]
pub(super) fn update_sidebar_item_ui(
    updater: &notedeck::updater::Updater,
    ui: &mut egui::Ui,
) -> Option<ChromePanelAction> {
    let version = updater.update_ready()?;

    let accent = notedeck_ui::colors::PINK;
    let desired_size = egui::vec2(ui.available_width(), 40.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let rounding = 8.0;
        let bg = if response.hovered() {
            accent.gamma_multiply(0.9)
        } else {
            accent
        };
        ui.painter().rect_filled(rect, rounding, bg);

        // Draw update arrow icon on the left
        let icon_size = 16.0;
        let icon_center = egui::pos2(rect.left() + 20.0, rect.center().y);
        notedeck_ui::icons::draw_update_icon(
            ui.painter(),
            icon_center,
            icon_size,
            Color32::WHITE,
            2.0,
        );

        // "Update available" text
        let text_pos = egui::pos2(rect.left() + 38.0, rect.center().y);
        let font = egui::FontId::new(
            notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
            egui::FontFamily::Name(notedeck::fonts::NamedFontFamily::Bold.as_str().into()),
        );
        let galley =
            ui.painter()
                .layout_no_wrap(format!("Update to {version}"), font, Color32::WHITE);
        let text_y = text_pos.y - galley.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(text_pos.x, text_y), galley, Color32::WHITE);
    }

    if response.clicked() {
        Some(ChromePanelAction::ApplyUpdate)
    } else {
        None
    }
}
