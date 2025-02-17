use crate::{
    gif::GifStateMap,
    nav::RenderNavAction,
    profile::ProfileAction,
    timeline::{TimelineCache, TimelineKind},
    ui::{self, note::NoteOptions, profile::ProfileView},
};

use enostr::Pubkey;
use nostrdb::Ndb;
use notedeck::{Accounts, MediaCache, MuteFun, NoteCache, UnknownIds, UrlMimes};

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    ndb: &Ndb,
    img_cache: &mut MediaCache,
    urls: &mut UrlMimes,
    unknown_ids: &mut UnknownIds,
    note_cache: &mut NoteCache,
    timeline_cache: &mut TimelineCache,
    gifs: &mut GifStateMap,
    accounts: &mut Accounts,
    kind: &TimelineKind,
    col: usize,
    textmode: bool,
    depth: usize,
    ui: &mut egui::Ui,
) -> Option<RenderNavAction> {
    let note_options = {
        let mut options = NoteOptions::new(kind == &TimelineKind::Universe);
        options.set_textmode(textmode);
        options
    };

    match kind {
        TimelineKind::List(_)
        | TimelineKind::Algo(_)
        | TimelineKind::Notifications(_)
        | TimelineKind::Universe
        | TimelineKind::Hashtag(_)
        | TimelineKind::Generic(_) => {
            let note_action = ui::TimelineView::new(
                kind,
                timeline_cache,
                ndb,
                note_cache,
                img_cache,
                urls,
                gifs,
                note_options,
                &accounts.mutefun(),
            )
            .ui(ui);

            note_action.map(RenderNavAction::NoteAction)
        }

        TimelineKind::Profile(pubkey) => {
            if depth > 1 {
                render_profile_route(
                    pubkey,
                    accounts,
                    ndb,
                    timeline_cache,
                    img_cache,
                    urls,
                    note_cache,
                    unknown_ids,
                    col,
                    ui,
                    &accounts.mutefun(),
                    gifs,
                )
            } else {
                // we render profiles like timelines if they are at the root
                let note_action = ui::TimelineView::new(
                    kind,
                    timeline_cache,
                    ndb,
                    note_cache,
                    img_cache,
                    urls,
                    gifs,
                    note_options,
                    &accounts.mutefun(),
                )
                .ui(ui);

                note_action.map(RenderNavAction::NoteAction)
            }
        }

        TimelineKind::Thread(id) => ui::ThreadView::new(
            timeline_cache,
            ndb,
            note_cache,
            unknown_ids,
            img_cache,
            urls,
            gifs,
            id.selected_or_root(),
            textmode,
            &accounts.mutefun(),
        )
        .id_source(egui::Id::new(("threadscroll", col)))
        .ui(ui)
        .map(Into::into),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    accounts: &Accounts,
    ndb: &Ndb,
    timeline_cache: &mut TimelineCache,
    img_cache: &mut MediaCache,
    urls: &mut UrlMimes,
    note_cache: &mut NoteCache,
    unknown_ids: &mut UnknownIds,
    col: usize,
    ui: &mut egui::Ui,
    is_muted: &MuteFun,
    gifs: &mut GifStateMap,
) -> Option<RenderNavAction> {
    let action = ProfileView::new(
        pubkey,
        accounts,
        col,
        timeline_cache,
        ndb,
        note_cache,
        img_cache,
        urls,
        gifs,
        unknown_ids,
        is_muted,
        NoteOptions::default(),
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

#[cfg(test)]
mod tests {
    use enostr::NoteId;
    use tokenator::{TokenParser, TokenWriter};

    use crate::timeline::{ThreadSelection, TimelineKind};
    use enostr::Pubkey;
    use notedeck::RootNoteIdBuf;

    #[test]
    fn test_timeline_route_serialize() {
        let note_id_hex = "1c54e5b0c386425f7e017d9e068ddef8962eb2ce1bb08ed27e24b93411c12e60";
        let note_id = NoteId::from_hex(note_id_hex).unwrap();
        let data_str = format!("thread:{}", note_id_hex);
        let data = &data_str.split(":").collect::<Vec<&str>>();
        let mut token_writer = TokenWriter::default();
        let mut parser = TokenParser::new(&data);
        let parsed = TimelineKind::parse(&mut parser, &Pubkey::new(*note_id.bytes())).unwrap();
        let expected = TimelineKind::Thread(ThreadSelection::from_root_id(
            RootNoteIdBuf::new_unsafe(*note_id.bytes()),
        ));
        parsed.serialize_tokens(&mut token_writer);
        assert_eq!(expected, parsed);
        assert_eq!(token_writer.str(), data_str);
    }
}
