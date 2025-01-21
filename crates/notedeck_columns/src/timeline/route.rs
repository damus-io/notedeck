use crate::{
    column::Columns,
    draft::Drafts,
    nav::RenderNavAction,
    profile::ProfileAction,
    storage::{ParseError, Payload, Token, TokenParser, TokenSerializable, TokenWriter},
    timeline::{TimelineCache, TimelineId, TimelineKind},
    ui::{
        self,
        note::{NoteOptions, QuoteRepostView},
        profile::ProfileView,
    },
};

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{Accounts, ImageCache, MuteFun, NoteCache, UnknownIds};

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum TimelineRoute {
    Timeline(TimelineId),
    Thread(NoteId),
    Profile(Pubkey),
    Reply(NoteId),
    Quote(NoteId),
}

const PROFILE_TOKENS: &[Token] = &[Token::id("profile"), Token::pubkey()];
const THREAD_TOKENS: &[Token] = &[Token::id("thread"), Token::note_id()];
const REPLY_TOKENS: &[Token] = &[Token::id("reply"), Token::note_id()];
const QUOTE_TOKENS: &[Token] = &[Token::id("quote"), Token::note_id()];

impl TimelineRoute {
    fn payload(&self) -> Option<Payload> {
        match self {
            TimelineRoute::Profile(pk) => Some(Payload::pubkey(*pk)),
            TimelineRoute::Thread(note_id) => Some(Payload::note_id(*note_id)),
            TimelineRoute::Reply(note_id) => Some(Payload::note_id(*note_id)),
            TimelineRoute::Quote(note_id) => Some(Payload::note_id(*note_id)),
            TimelineRoute::Timeline(_timeline_id) => todo!("handle timeline_ids"),
        }
    }

    fn tokens(&self) -> &'static [Token] {
        match self {
            TimelineRoute::Profile(_) => PROFILE_TOKENS,
            TimelineRoute::Thread(_) => THREAD_TOKENS,
            TimelineRoute::Reply(_) => REPLY_TOKENS,
            TimelineRoute::Quote(_) => QUOTE_TOKENS,
            TimelineRoute::Timeline(_) => todo!("handle timeline_ids"),
        }
    }

    /// NOTE!! update parse_from_tokens as well when adding to this match
    fn parse<'a>(&self, parser: &mut TokenParser<'a>) -> Result<TimelineRoute, ParseError<'a>> {
        let payload = Token::parse_all(parser, self.tokens())?;

        match self {
            TimelineRoute::Profile(_) => {
                Ok(TimelineRoute::Profile(Payload::parse_pubkey(payload)?))
            }
            TimelineRoute::Thread(_) => Ok(TimelineRoute::Thread(Payload::parse_note_id(payload)?)),
            TimelineRoute::Reply(_) => Ok(TimelineRoute::Reply(Payload::parse_note_id(payload)?)),
            TimelineRoute::Quote(_) => Ok(TimelineRoute::Quote(Payload::parse_note_id(payload)?)),
            TimelineRoute::Timeline(_) => todo!("handle timeline parsing"),
        }
    }
}

impl TokenSerializable for TimelineRoute {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        Token::serialize_all(writer, self.tokens(), self.payload().as_ref());
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        TokenParser::alt(
            parser,
            &[
                |p| TimelineRoute::Profile(Pubkey::new([0; 32])).parse(p),
                |p| TimelineRoute::Thread(NoteId::new([0; 32])).parse(p),
                |p| TimelineRoute::Reply(NoteId::new([0; 32])).parse(p),
                |p| TimelineRoute::Quote(NoteId::new([0; 32])).parse(p),
                |_p| todo!("handle timeline parsing"),
            ],
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    ndb: &Ndb,
    columns: &mut Columns,
    drafts: &mut Drafts,
    img_cache: &mut ImageCache,
    unknown_ids: &mut UnknownIds,
    note_cache: &mut NoteCache,
    timeline_cache: &mut TimelineCache,
    accounts: &mut Accounts,
    route: TimelineRoute,
    col: usize,
    textmode: bool,
    ui: &mut egui::Ui,
) -> Option<RenderNavAction> {
    match route {
        TimelineRoute::Timeline(timeline_id) => {
            let note_options = {
                let is_universe = if let Some(timeline) = columns.find_timeline(timeline_id) {
                    timeline.kind == TimelineKind::Universe
                } else {
                    false
                };

                let mut options = NoteOptions::new(is_universe);
                options.set_textmode(textmode);
                options
            };

            let note_action = ui::TimelineView::new(
                timeline_id,
                columns,
                ndb,
                note_cache,
                img_cache,
                note_options,
                &accounts.mutefun(),
            )
            .ui(ui);

            note_action.map(RenderNavAction::NoteAction)
        }

        TimelineRoute::Thread(id) => ui::ThreadView::new(
            timeline_cache,
            ndb,
            note_cache,
            unknown_ids,
            img_cache,
            id.bytes(),
            textmode,
            &accounts.mutefun(),
        )
        .id_source(egui::Id::new(("threadscroll", col)))
        .ui(ui)
        .map(Into::into),

        TimelineRoute::Reply(id) => {
            let txn = if let Ok(txn) = Transaction::new(ndb) {
                txn
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let note = if let Ok(note) = ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));
            let poster = accounts.selected_or_first_nsec()?;

            let action = {
                let draft = drafts.reply_mut(note.id());

                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::PostReplyView::new(ndb, poster, draft, note_cache, img_cache, &note)
                        .id_source(id)
                        .show(ui)
                });

                response.inner.action
            };

            action.map(Into::into)
        }

        TimelineRoute::Profile(pubkey) => render_profile_route(
            &pubkey,
            accounts,
            ndb,
            timeline_cache,
            img_cache,
            note_cache,
            unknown_ids,
            col,
            ui,
            &accounts.mutefun(),
        ),

        TimelineRoute::Quote(id) => {
            let txn = Transaction::new(ndb).expect("txn");

            let note = if let Ok(note) = ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Quote of unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));

            let poster = accounts.selected_or_first_nsec()?;
            let draft = drafts.quote_mut(note.id());

            let response = egui::ScrollArea::vertical().show(ui, |ui| {
                QuoteRepostView::new(ndb, poster, note_cache, img_cache, draft, &note)
                    .id_source(id)
                    .show(ui)
            });

            response.inner.action.map(Into::into)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    accounts: &Accounts,
    ndb: &Ndb,
    timeline_cache: &mut TimelineCache,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    unknown_ids: &mut UnknownIds,
    col: usize,
    ui: &mut egui::Ui,
    is_muted: &MuteFun,
) -> Option<RenderNavAction> {
    let action = ProfileView::new(
        pubkey,
        accounts,
        col,
        timeline_cache,
        ndb,
        note_cache,
        img_cache,
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
    use crate::storage::{TokenParser, TokenSerializable, TokenWriter};
    use enostr::NoteId;

    #[test]
    fn test_timeline_route_serialize() {
        use super::TimelineRoute;

        {
            let note_id_hex = "1c54e5b0c386425f7e017d9e068ddef8962eb2ce1bb08ed27e24b93411c12e60";
            let note_id = NoteId::from_hex(note_id_hex).unwrap();
            let data_str = format!("thread:{}", note_id_hex);
            let data = &data_str.split(":").collect::<Vec<&str>>();
            let mut token_writer = TokenWriter::default();
            let mut parser = TokenParser::new(&data);
            let parsed = TimelineRoute::parse_from_tokens(&mut parser).unwrap();
            let expected = TimelineRoute::Thread(note_id);
            parsed.serialize_tokens(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }
    }
}
