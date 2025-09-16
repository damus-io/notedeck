use crate::{
    nav::RenderNavAction,
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
) -> Option<RenderNavAction> {
    match kind {
        TimelineKind::List(_)
        | TimelineKind::Search(_)
        | TimelineKind::Algo(_)
        | TimelineKind::Notifications(_)
        | TimelineKind::Universe
        | TimelineKind::Hashtag(_)
        | TimelineKind::Generic(_) => {
            let note_action =
                ui::TimelineView::new(kind, timeline_cache, note_context, note_options, jobs, col)
                    .ui(ui);

            note_action.map(RenderNavAction::NoteAction)
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
                let note_action = ui::TimelineView::new(
                    kind,
                    timeline_cache,
                    note_context,
                    note_options,
                    jobs,
                    col,
                )
                .scroll_to_top(scroll_to_top)
                .ui(ui);

                note_action.map(RenderNavAction::NoteAction)
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
) -> Option<RenderNavAction> {
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
    .map(Into::into)
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
) -> Option<RenderNavAction> {
    let profile_view = ProfileView::new(
        pubkey,
        col,
        timeline_cache,
        note_options,
        note_context,
        jobs,
    )
    .ui(ui);

    if let Some(action) = profile_view {
        match action {
            ui::profile::ProfileViewAction::EditProfile => note_context
                .accounts
                .get_full(pubkey)
                .map(|kp| RenderNavAction::ProfileAction(ProfileAction::Edit(kp.to_full()))),
            ui::profile::ProfileViewAction::Note(note_action) => {
                Some(RenderNavAction::NoteAction(note_action))
            }
            ui::profile::ProfileViewAction::Follow(target_key) => Some(
                RenderNavAction::ProfileAction(ProfileAction::Follow(target_key)),
            ),
            ui::profile::ProfileViewAction::Unfollow(target_key) => Some(
                RenderNavAction::ProfileAction(ProfileAction::Unfollow(target_key)),
            ),
        }
    } else {
        None
    }
}
