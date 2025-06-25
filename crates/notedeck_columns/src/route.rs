use enostr::{NoteId, Pubkey};
use notedeck::{NoteZapTargetOwned, RootNoteIdBuf, WalletType};
use std::{
    fmt::{self},
    ops::Range,
};

use crate::{
    accounts::AccountsRoute,
    timeline::{
        kind::{AlgoTimeline, ColumnTitle, ListKind},
        ThreadSelection, TimelineKind,
    },
    ui::add_column::{AddAlgoRoute, AddColumnRoute},
};

use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Route {
    Timeline(TimelineKind),
    Thread(ThreadSelection),
    Accounts(AccountsRoute),
    Reply(NoteId),
    Quote(NoteId),
    Relays,
    ComposeNote,
    AddColumn(AddColumnRoute),
    EditProfile(Pubkey),
    Support,
    NewDeck,
    Search,
    EditDeck(usize),
    Wallet(WalletType),
    CustomizeZapAmount(NoteZapTargetOwned),
}

impl Route {
    pub fn timeline(timeline_kind: TimelineKind) -> Self {
        Route::Timeline(timeline_kind)
    }

    pub fn timeline_id(&self) -> Option<&TimelineKind> {
        if let Route::Timeline(tid) = self {
            Some(tid)
        } else {
            None
        }
    }

    pub fn relays() -> Self {
        Route::Relays
    }

    pub fn thread(thread_selection: ThreadSelection) -> Self {
        Route::Thread(thread_selection)
    }

    pub fn profile(pubkey: Pubkey) -> Self {
        Route::Timeline(TimelineKind::profile(pubkey))
    }

    pub fn reply(replying_to: NoteId) -> Self {
        Route::Reply(replying_to)
    }

    pub fn quote(quoting: NoteId) -> Self {
        Route::Quote(quoting)
    }

    pub fn accounts() -> Self {
        Route::Accounts(AccountsRoute::Accounts)
    }

    pub fn add_account() -> Self {
        Route::Accounts(AccountsRoute::AddAccount)
    }

    pub fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            Route::Timeline(timeline_kind) => timeline_kind.serialize_tokens(writer),
            Route::Thread(selection) => {
                writer.write_token("thread");

                if let Some(reply) = selection.selected_note {
                    writer.write_token("root");
                    writer.write_token(&NoteId::new(*selection.root_id.bytes()).hex());
                    writer.write_token("reply");
                    writer.write_token(&reply.hex());
                } else {
                    writer.write_token(&NoteId::new(*selection.root_id.bytes()).hex());
                }
            }
            Route::Accounts(routes) => routes.serialize_tokens(writer),
            Route::AddColumn(routes) => routes.serialize_tokens(writer),
            Route::Search => writer.write_token("search"),
            Route::Reply(note_id) => {
                writer.write_token("reply");
                writer.write_token(&note_id.hex());
            }
            Route::Quote(note_id) => {
                writer.write_token("quote");
                writer.write_token(&note_id.hex());
            }
            Route::EditDeck(ind) => {
                writer.write_token("deck");
                writer.write_token("edit");
                writer.write_token(&ind.to_string());
            }
            Route::EditProfile(pubkey) => {
                writer.write_token("profile");
                writer.write_token("edit");
                writer.write_token(&pubkey.hex());
            }
            Route::Relays => {
                writer.write_token("relay");
            }
            Route::ComposeNote => {
                writer.write_token("compose");
            }
            Route::Support => {
                writer.write_token("support");
            }
            Route::NewDeck => {
                writer.write_token("deck");
                writer.write_token("new");
            }
            Route::Wallet(_) => {
                writer.write_token("wallet");
            }
            Route::CustomizeZapAmount(_) => writer.write_token("customize zap amount"),
        }
    }

    pub fn parse<'a>(
        parser: &mut TokenParser<'a>,
        deck_author: &Pubkey,
    ) -> Result<Self, ParseError<'a>> {
        let tlkind =
            parser.try_parse(|p| Ok(Route::Timeline(TimelineKind::parse(p, deck_author)?)));

        if tlkind.is_ok() {
            return tlkind;
        }

        TokenParser::alt(
            parser,
            &[
                |p| Ok(Route::Accounts(AccountsRoute::parse_from_tokens(p)?)),
                |p| Ok(Route::AddColumn(AddColumnRoute::parse_from_tokens(p)?)),
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("deck")?;
                        p.parse_token("edit")?;
                        let ind_str = p.pull_token()?;
                        let parsed_index = ind_str
                            .parse::<usize>()
                            .map_err(|_| ParseError::DecodeFailed)?;
                        Ok(Route::EditDeck(parsed_index))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("profile")?;
                        p.parse_token("edit")?;
                        let pubkey = Pubkey::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        Ok(Route::EditProfile(pubkey))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("relay")?;
                        Ok(Route::Relays)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("quote")?;
                        Ok(Route::Quote(NoteId::new(tokenator::parse_hex_id(p)?)))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("reply")?;
                        Ok(Route::Reply(NoteId::new(tokenator::parse_hex_id(p)?)))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("compose")?;
                        Ok(Route::ComposeNote)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("support")?;
                        Ok(Route::Support)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("deck")?;
                        p.parse_token("new")?;
                        Ok(Route::NewDeck)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("search")?;
                        Ok(Route::Search)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("thread")?;
                        p.parse_token("root")?;

                        let root = tokenator::parse_hex_id(p)?;

                        p.parse_token("reply")?;

                        let selected = tokenator::parse_hex_id(p)?;

                        Ok(Route::Thread(ThreadSelection {
                            root_id: RootNoteIdBuf::new_unsafe(root),
                            selected_note: Some(NoteId::new(selected)),
                        }))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("thread")?;
                        Ok(Route::Thread(ThreadSelection::from_root_id(
                            RootNoteIdBuf::new_unsafe(tokenator::parse_hex_id(p)?),
                        )))
                    })
                },
            ],
        )
    }

    pub fn title(&self) -> ColumnTitle<'_> {
        match self {
            Route::Timeline(kind) => kind.to_title(),
            Route::Thread(_) => ColumnTitle::simple("Thread"),
            Route::Reply(_id) => ColumnTitle::simple("Reply"),
            Route::Quote(_id) => ColumnTitle::simple("Quote"),
            Route::Relays => ColumnTitle::simple("Relays"),
            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => ColumnTitle::simple("Accounts"),
                AccountsRoute::AddAccount => ColumnTitle::simple("Add Account"),
            },
            Route::ComposeNote => ColumnTitle::simple("Compose Note"),
            Route::AddColumn(c) => match c {
                AddColumnRoute::Base => ColumnTitle::simple("Add Column"),
                AddColumnRoute::Algo(r) => match r {
                    AddAlgoRoute::Base => ColumnTitle::simple("Add Algo Column"),
                    AddAlgoRoute::LastPerPubkey => ColumnTitle::simple("Add Last Notes Column"),
                },
                AddColumnRoute::UndecidedNotification => {
                    ColumnTitle::simple("Add Notifications Column")
                }
                AddColumnRoute::ExternalNotification => {
                    ColumnTitle::simple("Add External Notifications Column")
                }
                AddColumnRoute::Hashtag => ColumnTitle::simple("Add Hashtag Column"),
                AddColumnRoute::UndecidedIndividual => {
                    ColumnTitle::simple("Subscribe to someone's notes")
                }
                AddColumnRoute::ExternalIndividual => {
                    ColumnTitle::simple("Subscribe to someone else's notes")
                }
            },
            Route::Support => ColumnTitle::simple("Damus Support"),
            Route::NewDeck => ColumnTitle::simple("Add Deck"),
            Route::EditDeck(_) => ColumnTitle::simple("Edit Deck"),
            Route::EditProfile(_) => ColumnTitle::simple("Edit Profile"),
            Route::Search => ColumnTitle::simple("Search"),
            Route::Wallet(_) => ColumnTitle::simple("Wallet"),
            Route::CustomizeZapAmount(_) => ColumnTitle::simple("Customize Zap Amount"),
        }
    }

    pub fn exportable_to_note(&self) -> bool {
        match &self {
            Route::Timeline(timeline_kind) => match timeline_kind {
                TimelineKind::List(list_kind) => match list_kind {
                    ListKind::Contact(_) => true,
                },
                TimelineKind::Algo(algo) => match algo {
                    AlgoTimeline::LastPerPubkey(list_kind) => match list_kind {
                        ListKind::Contact(_) => true,
                    },
                },
                TimelineKind::Notifications(_) => true,
                TimelineKind::Profile(_) => true,
                TimelineKind::Thread(_) => false,
                TimelineKind::Universe => true,
                TimelineKind::Generic(_) => false,
                TimelineKind::Hashtag(_) => true,
            },
            Route::Accounts(_) => false,
            Route::Reply(_) => false,
            Route::Quote(_) => false,
            Route::Relays => false,
            Route::ComposeNote => false,
            Route::AddColumn(_) => false,
            Route::EditProfile(_) => false,
            Route::Support => false,
            Route::NewDeck => false,
            Route::EditDeck(_) => false,
        }
    }
}

// TODO: add this to egui-nav so we don't have to deal with returning
// and navigating headaches
#[derive(Clone, Debug)]
pub struct Router<R: Clone> {
    routes: Vec<R>,
    pub returning: bool,
    pub navigating: bool,
    replacing: bool,

    // An overlay captures a range of routes where only one will persist when going back, the most recent added
    overlay_ranges: Vec<Range<usize>>,
}

impl<R: Clone> Router<R> {
    pub fn new(routes: Vec<R>) -> Self {
        if routes.is_empty() {
            panic!("routes can't be empty")
        }
        let returning = false;
        let navigating = false;
        let replacing = false;
        Router {
            routes,
            returning,
            navigating,
            replacing,
            overlay_ranges: Vec::new(),
        }
    }

    pub fn route_to(&mut self, route: R) {
        self.navigating = true;
        self.routes.push(route);
    }

    pub fn route_to_overlaid(&mut self, route: R) {
        self.route_to(route);
        self.set_overlaying();
    }

    pub fn route_to_overlaid_new(&mut self, route: R) {
        self.route_to(route);
        self.new_overlay();
    }

    // Route to R. Then when it is successfully placed, should call `remove_previous_routes` to remove all previous routes
    pub fn route_to_replaced(&mut self, route: R) {
        self.navigating = true;
        self.replacing = true;
        self.routes.push(route);
    }

    /// Go back, start the returning process
    pub fn go_back(&mut self) -> Option<R> {
        if self.returning || self.routes.len() == 1 {
            return None;
        }
        self.returning = true;

        if let Some(range) = self.overlay_ranges.pop() {
            tracing::debug!("Going back, found overlay: {:?}", range);
            self.remove_overlay(range);
        } else {
            tracing::debug!("Going back, no overlay");
        }

        if self.routes.len() == 1 {
            return None;
        }

        self.prev().cloned()
    }

    /// Pop a route, should only be called on a NavRespose::Returned reseponse
    pub fn pop(&mut self) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
        }

        's: {
            let Some(last_range) = self.overlay_ranges.last_mut() else {
                break 's;
            };

            if last_range.end != self.routes.len() {
                break 's;
            }

            if last_range.end - 1 <= last_range.start {
                self.overlay_ranges.pop();
                break 's;
            }

            last_range.end -= 1;
        }

        self.returning = false;
        self.routes.pop()
    }

    pub fn remove_previous_routes(&mut self) {
        let num_routes = self.routes.len();
        if num_routes <= 1 {
            return;
        }

        self.returning = false;
        self.replacing = false;
        self.routes.drain(..num_routes - 1);
    }

    /// Removes all routes in the overlay besides the last
    fn remove_overlay(&mut self, overlay_range: Range<usize>) {
        let num_routes = self.routes.len();
        if num_routes <= 1 {
            return;
        }

        if overlay_range.len() <= 1 {
            return;
        }

        self.routes
            .drain(overlay_range.start..overlay_range.end - 1);
    }

    pub fn is_replacing(&self) -> bool {
        self.replacing
    }

    fn set_overlaying(&mut self) {
        let mut overlaying_active = None;
        let mut binding = self.overlay_ranges.last_mut();
        if let Some(range) = &mut binding {
            if range.end == self.routes.len() - 1 {
                overlaying_active = Some(range);
            }
        };

        if let Some(range) = overlaying_active {
            range.end = self.routes.len();
        } else {
            let new_range = self.routes.len() - 1..self.routes.len();
            self.overlay_ranges.push(new_range);
        }
    }

    fn new_overlay(&mut self) {
        let new_range = self.routes.len() - 1..self.routes.len();
        self.overlay_ranges.push(new_range);
    }

    pub fn top(&self) -> &R {
        self.routes.last().expect("routes can't be empty")
    }

    pub fn prev(&self) -> Option<&R> {
        self.routes.get(self.routes.len() - 2)
    }

    pub fn routes(&self) -> &Vec<R> {
        &self.routes
    }

    pub fn first(&self) -> Option<&R> {
        self.routes.first()
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Timeline(kind) => match kind {
                TimelineKind::List(ListKind::Contact(_pk)) => write!(f, "Contacts"),
                TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::Contact(_))) => {
                    write!(f, "Last Per Pubkey (Contact)")
                }
                TimelineKind::Notifications(_) => write!(f, "Notifications"),
                TimelineKind::Universe => write!(f, "Universe"),
                TimelineKind::Generic(_) => write!(f, "Custom"),
                TimelineKind::Search(_) => write!(f, "Search"),
                TimelineKind::Hashtag(ht) => write!(f, "Hashtags ({})", ht.join(" ")),
                TimelineKind::Profile(_id) => write!(f, "Profile"),
            },
            Route::Thread(_) => write!(f, "Thread"),
            Route::Reply(_id) => write!(f, "Reply"),
            Route::Quote(_id) => write!(f, "Quote"),
            Route::Relays => write!(f, "Relays"),
            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => write!(f, "Accounts"),
                AccountsRoute::AddAccount => write!(f, "Add Account"),
            },
            Route::ComposeNote => write!(f, "Compose Note"),
            Route::AddColumn(_) => write!(f, "Add Column"),
            Route::Support => write!(f, "Support"),
            Route::NewDeck => write!(f, "Add Deck"),
            Route::EditDeck(_) => write!(f, "Edit Deck"),
            Route::EditProfile(_) => write!(f, "Edit Profile"),
            Route::Search => write!(f, "Search"),
            Route::Wallet(_) => write!(f, "Wallet"),
            Route::CustomizeZapAmount(_) => write!(f, "Customize Zap Amount"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SingletonRouter<R: Clone> {
    route: Option<R>,
    pub returning: bool,
    pub navigating: bool,
}

impl<R: Clone> SingletonRouter<R> {
    pub fn route_to(&mut self, route: R) {
        self.navigating = true;
        self.route = Some(route);
    }

    pub fn go_back(&mut self) {
        self.returning = true;
    }

    pub fn clear(&mut self) {
        self.route = None;
        self.returning = false;
    }

    pub fn route(&self) -> &Option<R> {
        &self.route
    }
}

impl<R: Clone> Default for SingletonRouter<R> {
    fn default() -> Self {
        Self {
            route: None,
            returning: false,
            navigating: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use enostr::NoteId;
    use tokenator::{TokenParser, TokenWriter};

    use crate::{timeline::ThreadSelection, Route};
    use enostr::Pubkey;
    use notedeck::RootNoteIdBuf;

    #[test]
    fn test_thread_route_serialize() {
        let note_id_hex = "1c54e5b0c386425f7e017d9e068ddef8962eb2ce1bb08ed27e24b93411c12e60";
        let note_id = NoteId::from_hex(note_id_hex).unwrap();
        let data_str = format!("thread:{}", note_id_hex);
        let data = &data_str.split(":").collect::<Vec<&str>>();
        let mut token_writer = TokenWriter::default();
        let mut parser = TokenParser::new(&data);
        let parsed = Route::parse(&mut parser, &Pubkey::new(*note_id.bytes())).unwrap();
        let expected = Route::Thread(ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe(
            *note_id.bytes(),
        )));
        parsed.serialize_tokens(&mut token_writer);
        assert_eq!(expected, parsed);
        assert_eq!(token_writer.str(), data_str);
    }
}
