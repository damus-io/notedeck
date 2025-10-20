use crate::{
    nav::{BodyResponse, RenderNavAction},
    profile::ProfileAction,
    timeline::{thread::Threads, ThreadSelection, TimelineCache, TimelineKind},
    ui::{self, ProfileView},
};

use enostr::Pubkey;
use notedeck::{JobsCache, NoteContext};
use notedeck_ui::NoteOptions;

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    timeline_cache: &mut TimelineCache,
    kind: &TimelineKind,
    col: usize,
    note_options: NoteOptions,
    depth: usize,
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
    scroll_to_top: bool,
) -> BodyResponse<RenderNavAction> {
    match kind {
        TimelineKind::List(_)
        | TimelineKind::Search(_)
        | TimelineKind::Algo(_)
        | TimelineKind::Notifications(_)
        | TimelineKind::Universe
        | TimelineKind::Hashtag(_)
        | TimelineKind::Generic(_) => {
            let resp =
                ui::TimelineView::new(kind, timeline_cache, note_context, note_options, jobs, col)
                    .ui(ui);

            resp.map_output(RenderNavAction::NoteAction)
        }

        TimelineKind::Profile(pubkey) => {
            if depth > 1 {
                render_profile_route(
                    pubkey,
                    timeline_cache,
                    col,
                    ui,
                    note_options,
                    note_context,
                    jobs,
                )
            } else {
                // we render profiles like timelines if they are at the root
                let resp = ui::TimelineView::new(
                    kind,
                    timeline_cache,
                    note_context,
                    note_options,
                    jobs,
                    col,
                )
                .scroll_to_top(scroll_to_top)
                .ui(ui);

                resp.map_output(RenderNavAction::NoteAction)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_thread_route(
    threads: &mut Threads,
    selection: &ThreadSelection,
    col: usize,
    mut note_options: NoteOptions,
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
) -> BodyResponse<RenderNavAction> {
    // don't truncate thread notes for now, since they are
    // default truncated everywher eelse
    note_options.set(NoteOptions::Truncate, false);

    // We need the reply lines in threads
    note_options.set(NoteOptions::Wide, false);

    ui::ThreadView::new(
        threads,
        selection.selected_or_root(),
        note_options,
        note_context,
        jobs,
        col,
    )
    .ui(ui)
    .map_output(RenderNavAction::NoteAction)
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    timeline_cache: &mut TimelineCache,
    col: usize,
    ui: &mut egui::Ui,
    note_options: NoteOptions,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
) -> BodyResponse<RenderNavAction> {
    let profile_view = ProfileView::new(
        pubkey,
        col,
        timeline_cache,
        note_options,
        note_context,
        jobs,
    )
    .ui(ui);

    profile_view.map_output_maybe(|action| match action {
        ui::profile::ProfileViewAction::EditProfile => note_context
            .accounts
            .get_full(pubkey)
            .map(|kp| RenderNavAction::ProfileAction(ProfileAction::Edit(kp.to_full()))),
        ui::profile::ProfileViewAction::Note(note_action) => {
            Some(RenderNavAction::NoteAction(note_action))
        }
        ui::profile::ProfileViewAction::Follow(target_key) => Some(RenderNavAction::ProfileAction(
            ProfileAction::Follow(target_key),
        )),
        ui::profile::ProfileViewAction::Unfollow(target_key) => Some(
            RenderNavAction::ProfileAction(ProfileAction::Unfollow(target_key)),
        ),
        ui::profile::ProfileViewAction::Context(profile_context_selection) => Some(
            RenderNavAction::ProfileAction(ProfileAction::Context(profile_context_selection)),
        ),
    })
}
