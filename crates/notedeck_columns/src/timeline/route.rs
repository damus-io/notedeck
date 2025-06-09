use crate::{
    nav::RenderNavAction,
    profile::ProfileAction,
    timeline::{thread::Threads, ThreadSelection, TimelineCache, TimelineKind},
    ui::{self, ProfileView},
};

use enostr::Pubkey;
use notedeck::{Accounts, MuteFun, NoteContext};
use notedeck_ui::{jobs::JobsCache, NoteOptions};

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    timeline_cache: &mut TimelineCache,
    accounts: &mut Accounts,
    kind: &TimelineKind,
    col: usize,
    note_options: NoteOptions,
    depth: usize,
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
) -> Option<RenderNavAction> {
    match kind {
        TimelineKind::List(_)
        | TimelineKind::Search(_)
        | TimelineKind::Algo(_)
        | TimelineKind::Notifications(_)
        | TimelineKind::Universe
        | TimelineKind::Hashtag(_)
        | TimelineKind::Generic(_) => {
            let note_action = ui::TimelineView::new(
                kind,
                timeline_cache,
                &accounts.mutefun(),
                note_context,
                note_options,
                &accounts.get_selected_account().map(|a| (&a.key).into()),
                jobs,
            )
            .ui(ui);

            note_action.map(RenderNavAction::NoteAction)
        }

        TimelineKind::Profile(pubkey) => {
            if depth > 1 {
                render_profile_route(
                    pubkey,
                    accounts,
                    timeline_cache,
                    col,
                    ui,
                    &accounts.mutefun(),
                    note_options,
                    note_context,
                    jobs,
                )
            } else {
                // we render profiles like timelines if they are at the root
                let note_action = ui::TimelineView::new(
                    kind,
                    timeline_cache,
                    &accounts.mutefun(),
                    note_context,
                    note_options,
                    &accounts.get_selected_account().map(|a| (&a.key).into()),
                    jobs,
                )
                .ui(ui);

                note_action.map(RenderNavAction::NoteAction)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_thread_route(
    threads: &mut Threads,
    accounts: &mut Accounts,
    selection: &ThreadSelection,
    col: usize,
    mut note_options: NoteOptions,
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
) -> Option<RenderNavAction> {
    // don't truncate thread notes for now, since they are
    // default truncated everywher eelse
    note_options.set_truncate(false);

    ui::ThreadView::new(
        threads,
        selection.selected_or_root(),
        note_options,
        &accounts.mutefun(),
        note_context,
        &accounts.get_selected_account().map(|a| (&a.key).into()),
        jobs,
    )
    .id_source(col)
    .ui(ui)
    .map(Into::into)
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    accounts: &Accounts,
    timeline_cache: &mut TimelineCache,
    col: usize,
    ui: &mut egui::Ui,
    is_muted: &MuteFun,
    note_options: NoteOptions,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
) -> Option<RenderNavAction> {
    let action = ProfileView::new(
        pubkey,
        accounts,
        col,
        timeline_cache,
        note_options,
        is_muted,
        note_context,
        jobs,
    )
    .ui(ui);

    if let Some(action) = action {
        match action {
            ui::profile::ProfileViewAction::EditProfile => accounts
                .get_full(pubkey.bytes())
                .map(|kp| RenderNavAction::ProfileAction(ProfileAction::Edit(kp.to_full()))),
            ui::profile::ProfileViewAction::Note(note_action) => {
                Some(RenderNavAction::NoteAction(note_action))
            }
        }
    } else {
        None
    }
}
