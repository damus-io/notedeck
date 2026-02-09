/// Top buttons UI for the Dave chat interface.
///
/// Contains the profile picture button, settings button, and session list toggle
/// that appear at the top of the chat view.
use super::DaveAction;
use nostrdb::{Ndb, Transaction};
use notedeck::{Accounts, AppContext, Images, MediaJobSender};
use notedeck_ui::{app_images, ProfilePic};

/// Render the top buttons UI (profile pic, settings, session list toggle)
pub fn top_buttons_ui(app_ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<DaveAction> {
    let mut action: Option<DaveAction> = None;
    let mut rect = ui.available_rect_before_wrap();
    rect = rect.translate(egui::vec2(20.0, 20.0));
    rect.set_height(32.0);
    rect.set_width(32.0);

    // Show session list button on mobile/narrow screens
    if notedeck::ui::is_narrow(ui.ctx()) {
        let r = ui
            .put(rect, egui::Button::new("\u{2630}").frame(false))
            .on_hover_text("Show chats")
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        if r.clicked() {
            action = Some(DaveAction::ShowSessionList);
        }

        rect = rect.translate(egui::vec2(30.0, 0.0));
    }

    let txn = Transaction::new(app_ctx.ndb).unwrap();
    let r = ui
        .put(
            rect,
            &mut pfp_button(
                &txn,
                app_ctx.accounts,
                app_ctx.img_cache,
                app_ctx.ndb,
                app_ctx.media_jobs.sender(),
            ),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if r.clicked() {
        action = Some(DaveAction::ToggleChrome);
    }

    // Settings button
    rect = rect.translate(egui::vec2(30.0, 0.0));
    let dark_mode = ui.visuals().dark_mode;
    let r = ui
        .put(rect, settings_button(dark_mode))
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if r.clicked() {
        action = Some(DaveAction::OpenSettings);
    }

    action
}

fn settings_button(dark_mode: bool) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = 32.0;

        let img = if dark_mode {
            app_images::settings_dark_image()
        } else {
            app_images::settings_light_image()
        }
        .max_width(img_size);

        let helper = notedeck_ui::anim::AnimationHelper::new(
            ui,
            "settings-button",
            egui::vec2(max_size, max_size),
        );

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper.take_animation_response()
    }
}

fn pfp_button<'me, 'a>(
    txn: &'a Transaction,
    accounts: &Accounts,
    img_cache: &'me mut Images,
    ndb: &Ndb,
    jobs: &'me MediaJobSender,
) -> ProfilePic<'me, 'a> {
    let account = accounts.get_selected_account();
    let profile = ndb
        .get_profile_by_pubkey(txn, account.key.pubkey.bytes())
        .ok();

    ProfilePic::from_profile_or_default(img_cache, jobs, profile.as_ref())
        .size(24.0)
        .sense(egui::Sense::click())
}
