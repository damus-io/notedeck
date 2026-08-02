use egui_nav::{Percent, ReturnType};
use enostr::{NoteId, Pubkey};
use nostrdb::Ndb;
use notedeck::{
    tr, Localization, NoteDetail, NoteZapTargetOwned, ReplacementType, ReportTarget, RootNoteIdBuf,
    Router, ScopedSubApi, WalletType,
};
use std::ops::Range;

use crate::{
    accounts::AccountsRoute,
    column::ColumnId,
    onboarding::Onboarding,
    scoped_sub_owner_keys::onboarding_owner_key,
    timeline::{kind::ColumnTitle, thread::Threads, ThreadSelection, TimelineCache, TimelineKind},
    ui::add_column::{AddAlgoRoute, AddColumnRoute},
    view_state::ViewState,
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
    RepostDecision(NoteId),
    Relays,
    Settings,
    ComposeNote,
    AddColumn(AddColumnRoute),
    EditProfile(Pubkey),
    Support,
    NewDeck,
    Search,
    EditDeck(usize),
    Wallet(WalletType),
    CustomizeZapAmount(NoteZapTargetOwned),
    Following(Pubkey),
    FollowedBy(Pubkey),
    TosAcceptance,
    Welcome,
    Report(ReportTarget),
    NoteDetails(NoteDetail),
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

    pub fn settings() -> Self {
        Route::Settings
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
            Route::Settings => {
                writer.write_token("settings");
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
            Route::RepostDecision(note_id) => {
                writer.write_token("repost_decision");
                writer.write_token(&note_id.hex());
            }
            Route::Following(pubkey) => {
                writer.write_token("following");
                writer.write_token(&pubkey.hex());
            }
            Route::FollowedBy(pubkey) => {
                writer.write_token("followed_by");
                writer.write_token(&pubkey.hex());
            }
            Route::TosAcceptance => {
                writer.write_token("tos");
            }
            Route::Welcome => {
                writer.write_token("welcome");
            }
            Route::Report(target) => {
                writer.write_token("report");
                writer.write_token(&target.pubkey.hex());
                if let Some(note_id) = &target.note_id {
                    writer.write_token(&note_id.hex());
                }
            }
            Route::NoteDetails(detail) => {
                writer.write_token("note_detail");
                match detail {
                    NoteDetail::Reactions(id) => {
                        writer.write_token("reactions");
                        writer.write_token(&id.hex());
                    }
                    NoteDetail::Reposts(id) => {
                        writer.write_token("reposts");
                        writer.write_token(&id.hex());
                    }
                    NoteDetail::Zaps(id) => {
                        writer.write_token("zaps");
                        writer.write_token(&id.hex());
                    }
                }
            }
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
                        p.parse_token("settings")?;
                        Ok(Route::Settings)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("repost_decision")?;
                        let note_id = NoteId::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        Ok(Route::RepostDecision(note_id))
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
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("following")?;
                        let pubkey = Pubkey::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        Ok(Route::Following(pubkey))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("followed_by")?;
                        let pubkey = Pubkey::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        Ok(Route::FollowedBy(pubkey))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("tos")?;
                        Ok(Route::TosAcceptance)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("welcome")?;
                        Ok(Route::Welcome)
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("report")?;
                        let pubkey = Pubkey::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        let note_id = p.pull_token().ok().and_then(|t| NoteId::from_hex(t).ok());
                        Ok(Route::Report(ReportTarget { pubkey, note_id }))
                    })
                },
                |p| {
                    p.parse_all(|p| {
                        p.parse_token("note_detail")?;
                        let kind = p.pull_token()?;
                        let note_id = NoteId::from_hex(p.pull_token()?)
                            .map_err(|_| ParseError::HexDecodeFailed)?;
                        let detail = match kind {
                            "reactions" => NoteDetail::Reactions(note_id),
                            "reposts" => NoteDetail::Reposts(note_id),
                            "zaps" => NoteDetail::Zaps(note_id),
                            _ => return Err(ParseError::DecodeFailed),
                        };
                        Ok(Route::NoteDetails(detail))
                    })
                },
            ],
        )
    }

    pub fn title(&self, i18n: &mut Localization) -> ColumnTitle<'_> {
        match self {
            Route::Timeline(kind) => kind.to_title(i18n),
            Route::Thread(_) => {
                ColumnTitle::formatted(tr!(i18n, "Thread", "Column title for note thread view"))
            }
            Route::Reply(_id) => {
                ColumnTitle::formatted(tr!(i18n, "Reply", "Column title for reply composition"))
            }
            Route::Quote(_id) => {
                ColumnTitle::formatted(tr!(i18n, "Quote", "Column title for quote composition"))
            }
            Route::Relays => {
                ColumnTitle::formatted(tr!(i18n, "Relays", "Column title for relay management"))
            }
            Route::Settings => {
                ColumnTitle::formatted(tr!(i18n, "Settings", "Column title for app settings"))
            }
            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => ColumnTitle::formatted(tr!(
                    i18n,
                    "Accounts",
                    "Column title for account management"
                )),
                AccountsRoute::AddAccount => ColumnTitle::formatted(tr!(
                    i18n,
                    "Add Account",
                    "Column title for adding new account"
                )),
                AccountsRoute::Onboarding => ColumnTitle::formatted(tr!(
                    i18n,
                    "Onboarding",
                    "Column title for finding users to follow"
                )),
            },
            Route::ComposeNote => ColumnTitle::formatted(tr!(
                i18n,
                "Compose Note",
                "Column title for note composition"
            )),
            Route::AddColumn(c) => match c {
                AddColumnRoute::Base => ColumnTitle::formatted(tr!(
                    i18n,
                    "Add Column",
                    "Column title for adding new column"
                )),
                AddColumnRoute::Algo(r) => match r {
                    AddAlgoRoute::Base => ColumnTitle::formatted(tr!(
                        i18n,
                        "Add Algo Column",
                        "Column title for adding algorithm column"
                    )),
                    AddAlgoRoute::LastPerPubkey => ColumnTitle::formatted(tr!(
                        i18n,
                        "Add Last Notes Column",
                        "Column title for adding last notes column"
                    )),
                },
                AddColumnRoute::UndecidedNotification => ColumnTitle::formatted(tr!(
                    i18n,
                    "Add Notifications Column",
                    "Column title for adding notifications column"
                )),
                AddColumnRoute::ExternalNotification => ColumnTitle::formatted(tr!(
                    i18n,
                    "Add External Notifications Column",
                    "Column title for adding external notifications column"
                )),
                AddColumnRoute::Hashtag => ColumnTitle::formatted(tr!(
                    i18n,
                    "Add Hashtag Column",
                    "Column title for adding hashtag column"
                )),
                AddColumnRoute::UndecidedIndividual => ColumnTitle::formatted(tr!(
                    i18n,
                    "Subscribe to someone's notes",
                    "Column title for subscribing to individual user"
                )),
                AddColumnRoute::ExternalIndividual => ColumnTitle::formatted(tr!(
                    i18n,
                    "Subscribe to someone else's notes",
                    "Column title for subscribing to external user"
                )),
                AddColumnRoute::PeopleList => ColumnTitle::formatted(tr!(
                    i18n,
                    "Select a People List",
                    "Column title for selecting a people list"
                )),
                AddColumnRoute::CreatePeopleList => ColumnTitle::formatted(tr!(
                    i18n,
                    "Create People List",
                    "Column title for creating a people list"
                )),
            },
            Route::Support => {
                ColumnTitle::formatted(tr!(i18n, "Damus Support", "Column title for support page"))
            }
            Route::NewDeck => {
                ColumnTitle::formatted(tr!(i18n, "Add Deck", "Column title for adding new deck"))
            }
            Route::EditDeck(_) => {
                ColumnTitle::formatted(tr!(i18n, "Edit Deck", "Column title for editing deck"))
            }
            Route::EditProfile(_) => ColumnTitle::formatted(tr!(
                i18n,
                "Edit Profile",
                "Column title for profile editing"
            )),
            Route::Search => {
                ColumnTitle::formatted(tr!(i18n, "Search", "Column title for search page"))
            }
            Route::Wallet(_) => {
                ColumnTitle::formatted(tr!(i18n, "Wallet", "Column title for wallet management"))
            }
            Route::CustomizeZapAmount(_) => ColumnTitle::formatted(tr!(
                i18n,
                "Customize Zap Amount",
                "Column title for zap amount customization"
            )),
            Route::RepostDecision(_) => ColumnTitle::formatted(tr!(
                i18n,
                "Repost",
                "Column title for deciding the type of repost"
            )),
            Route::Following(_) => ColumnTitle::formatted(tr!(
                i18n,
                "Following",
                "Column title for users being followed"
            )),
            Route::FollowedBy(_) => {
                ColumnTitle::formatted(tr!(i18n, "Followed by", "Column title for followers"))
            }
            Route::TosAcceptance => ColumnTitle::formatted(tr!(
                i18n,
                "Terms of Service",
                "Column title for TOS acceptance screen"
            )),
            Route::Welcome => {
                ColumnTitle::formatted(tr!(i18n, "Welcome", "Column title for welcome screen"))
            }
            Route::Report(_) => {
                ColumnTitle::formatted(tr!(i18n, "Report", "Column title for report screen"))
            }
            Route::NoteDetails(detail) => match detail {
                NoteDetail::Reactions(_) => ColumnTitle::formatted(tr!(
                    i18n,
                    "Reactions",
                    "Column title for note reactions list"
                )),
                NoteDetail::Reposts(_) => ColumnTitle::formatted(tr!(
                    i18n,
                    "Reposts",
                    "Column title for note reposts list"
                )),
                NoteDetail::Zaps(_) => {
                    ColumnTitle::formatted(tr!(i18n, "Zaps", "Column title for note zaps list"))
                }
            },
        }
    }
}

// TODO: add this to egui-nav so we don't have to deal with returning
// and navigating headaches
#[derive(Clone, Debug)]
pub struct ColumnsRouter<R: Clone> {
    router_internal: Router<R>,
    forward_stack: Vec<R>,

    // An overlay captures a range of routes where only one will persist when going back, the most recent added
    overlay_ranges: Vec<Range<usize>>,
}

impl<R: Clone> ColumnsRouter<R> {
    pub fn new(routes: Vec<R>) -> Self {
        if routes.is_empty() {
            panic!("routes can't be empty")
        }
        let router_internal = Router::new(routes);
        ColumnsRouter {
            router_internal,
            forward_stack: Vec::new(),
            overlay_ranges: Vec::new(),
        }
    }

    pub fn route_to(&mut self, route: R) {
        self.router_internal.route_to(route);
        self.forward_stack.clear();
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
        self.router_internal
            .route_to_replaced(route, ReplacementType::All);
    }

    /// Go back, start the returning process
    pub fn go_back(&mut self) -> Option<R> {
        if self.router_internal.returning || self.router_internal.len() == 1 {
            return None;
        }

        if let Some(range) = self.overlay_ranges.pop() {
            tracing::debug!("Going back, found overlay: {:?}", range);
            self.remove_overlay(range);
        } else {
            tracing::debug!("Going back, no overlay");
        }

        self.router_internal.go_back()
    }

    pub fn go_forward(&mut self) -> bool {
        if let Some(route) = self.forward_stack.pop() {
            self.router_internal.route_to(route);
            true
        } else {
            false
        }
    }

    /// Pop a route, should only be called on a `NavResponse::Returned` response.
    pub fn pop(&mut self) -> Option<R> {
        self.remove_top_route(true)
    }

    /// Remove the top route outside a rendered nav return.
    ///
    /// This is for owner-driven cleanup paths, such as account switching, where
    /// the source route must be removed before another account becomes selected.
    pub fn remove_top_route_for_disposal(&mut self) -> Option<R> {
        let removed = self.remove_top_route(false);
        if removed.is_some() {
            self.router_internal.returning = false;
            self.router_internal.navigating = false;
        }
        removed
    }

    fn remove_top_route(&mut self, keep_forward_route: bool) -> Option<R> {
        if self.router_internal.len() == 1 {
            return None;
        }

        let is_overlay = 's: {
            let Some(last_range) = self.overlay_ranges.last_mut() else {
                break 's false;
            };

            if last_range.end != self.router_internal.len() {
                break 's false;
            }

            if last_range.end - 1 <= last_range.start {
                self.overlay_ranges.pop();
            } else {
                last_range.end -= 1;
            }

            true
        };

        let popped = self.router_internal.pop()?;
        if keep_forward_route && !is_overlay {
            self.forward_stack.push(popped.clone());
        }
        Some(popped)
    }

    pub fn remove_previous_routes(&mut self) {
        self.router_internal.complete_replacement();
    }

    /// Removes all routes in the overlay besides the last.
    ///
    /// Do not treat the drained routes as missing cleanup work. In Columns, a
    /// multi-route overlay is a thread stack, not a list of independent route
    /// owners: `route_to_overlaid` appends another route to the same
    /// `ThreadSubs` scope, while `route_to_overlaid_new` is the call that starts
    /// a separate overlay/scope. On click/back, the retained top overlay route is
    /// returned through normal nav handling with `ReturnType::Click`; then
    /// `ThreadSubs::unsubscribe_click` drops the whole current thread scope,
    /// including the stack entries represented by routes drained here.
    ///
    /// Returning the drained routes from this generic router would make callers
    /// dispose them as separate owners, which is the wrong model for thread
    /// overlays and regresses the master behavior. If a future overlay route
    /// type really needs per-route ownership inside one overlay range, model that
    /// ownership explicitly at the route owner layer instead of reusing these
    /// drained router entries as disposal events. Drag returns do not call
    /// `go_back`; they pop one route and use `ReturnType::Drag`.
    fn remove_overlay(&mut self, overlay_range: Range<usize>) {
        let num_routes = self.router_internal.routes.len();
        if num_routes <= 1 {
            return;
        }

        if overlay_range.len() <= 1 {
            return;
        }

        self.router_internal
            .routes
            .drain(overlay_range.start..overlay_range.end - 1);
    }

    pub fn is_replacing(&self) -> bool {
        self.router_internal.is_replacing()
    }

    fn set_overlaying(&mut self) {
        let mut overlaying_active = None;
        let mut binding = self.overlay_ranges.last_mut();
        if let Some(range) = &mut binding {
            if range.end == self.router_internal.len() - 1 {
                overlaying_active = Some(range);
            }
        };

        if let Some(range) = overlaying_active {
            range.end = self.router_internal.len();
        } else {
            let new_range = self.router_internal.len() - 1..self.router_internal.len();
            self.overlay_ranges.push(new_range);
        }
    }

    fn new_overlay(&mut self) {
        let new_range = self.router_internal.len() - 1..self.router_internal.len();
        self.overlay_ranges.push(new_range);
    }

    pub fn routes(&self) -> &Vec<R> {
        self.router_internal.routes()
    }

    /// Snapshot the visible route stack for forced disposal.
    ///
    /// This must stay aligned with `remove_overlay`: forced disposal owns the
    /// visible routes only. Routes already drained from a thread overlay are
    /// represented by the retained top route's `ThreadSubs` scope and must not be
    /// synthesized back into a disposal list.
    pub fn routes_for_disposal(&self) -> Vec<R> {
        self.router_internal.routes().clone()
    }

    pub fn navigating(&self) -> bool {
        self.router_internal.navigating
    }

    pub fn navigating_mut(&mut self, new: bool) {
        self.router_internal.navigating = new;
    }

    pub fn returning(&self) -> bool {
        self.router_internal.returning
    }

    pub fn returning_mut(&mut self, new: bool) {
        self.router_internal.returning = new;
    }

    pub fn top(&self) -> &R {
        self.router_internal.top()
    }

    pub fn prev(&self) -> Option<&R> {
        self.router_internal.prev()
    }
}

/*
impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Timeline(kind) => match kind {
                TimelineKind::List(ListKind::Contact(_pk)) => {
                    write!(f, "{}", i18n, "Home", "Display name for home feed"))
                }
                TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::Contact(_))) => {
                    write!(
                        f,
                        "{}",
                        tr!(
                            "Last Per Pubkey (Contact)",
                            "Display name for last notes per contact"
                        )
                    )
                }
                TimelineKind::Notifications(_) => write!(
                    f,
                    "{}",
                    tr!("Notifications", "Display name for notifications")
                ),
                TimelineKind::Universe => {
                    write!(f, "{}", tr!("Universe", "Display name for universe feed"))
                }
                TimelineKind::Generic(_) => {
                    write!(f, "{}", tr!("Custom", "Display name for custom timelines"))
                }
                TimelineKind::Search(_) => {
                    write!(f, "{}", tr!("Search", "Display name for search results"))
                }
                TimelineKind::Hashtag(ht) => write!(
                    f,
                    "{} ({})",
                    tr!("Hashtags", "Display name for hashtag feeds"),
                    ht.join(" ")
                ),
                TimelineKind::Profile(_id) => {
                    write!(f, "{}", tr!("Profile", "Display name for user profiles"))
                }
            },
            Route::Thread(_) => write!(f, "{}", tr!("Thread", "Display name for thread view")),
            Route::Reply(_id) => {
                write!(f, "{}", tr!("Reply", "Display name for reply composition"))
            }
            Route::Quote(_id) => {
                write!(f, "{}", tr!("Quote", "Display name for quote composition"))
            }
            Route::Relays => write!(f, "{}", tr!("Relays", "Display name for relay management")),
            Route::Settings => write!(f, "{}", tr!("Settings", "Display name for settings management")),
            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => write!(
                    f,
                    "{}",
                    tr!("Accounts", "Display name for account management")
                ),
                AccountsRoute::AddAccount => write!(
                    f,
                    "{}",
                    tr!("Add Account", "Display name for adding account")
                ),
            },
            Route::ComposeNote => write!(
                f,
                "{}",
                tr!("Compose Note", "Display name for note composition")
            ),
            Route::AddColumn(_) => {
                write!(f, "{}", tr!("Add Column", "Display name for adding column"))
            }
            Route::Support => write!(f, "{}", tr!("Support", "Display name for support page")),
            Route::NewDeck => write!(f, "{}", tr!("Add Deck", "Display name for adding deck")),
            Route::EditDeck(_) => {
                write!(f, "{}", tr!("Edit Deck", "Display name for editing deck"))
            }
            Route::EditProfile(_) => write!(
                f,
                "{}",
                tr!("Edit Profile", "Display name for profile editing")
            ),
            Route::Search => write!(f, "{}", tr!("Search", "Display name for search page")),
            Route::Wallet(_) => {
                write!(f, "{}", tr!("Wallet", "Display name for wallet management"))
            }
            Route::CustomizeZapAmount(_) => write!(
                f,
                "{}",
                tr!("Customize Zap Amount", "Display name for zap customization")
            ),
        }
    }
}
*/

#[derive(Clone, Debug)]
pub struct SingletonRouter<R: Clone> {
    route: Option<R>,
    pub returning: bool,
    pub navigating: bool,
    pub after_action: Option<R>,
    pub split: egui_nav::Split,
}

impl<R: Clone> SingletonRouter<R> {
    pub fn route_to(&mut self, route: R, split: egui_nav::Split) {
        self.navigating = true;
        self.route = Some(route);
        self.split = split;
    }

    pub fn go_back(&mut self) {
        self.returning = true;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
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
            after_action: None,
            split: egui_nav::Split::PercentFromTop(Percent::new(35).expect("35 <= 100")),
        }
    }
}

enum RouteCleanup {
    Returned { return_type: ReturnType },
    Disposed { account_pk: Pubkey },
}

/// Centralized resource cleanup for popped routes.
/// This handles cleanup for routes with owned caches, subscriptions, or UI state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cleanup_popped_route(
    route: &Route,
    timeline_cache: &mut TimelineCache,
    threads: &mut Threads,
    onboarding: &mut Onboarding,
    view_state: &mut ViewState,
    ndb: &mut Ndb,
    scoped_subs: &mut ScopedSubApi,
    return_type: ReturnType,
    column_id: ColumnId,
) {
    cleanup_route(
        route,
        column_id,
        timeline_cache,
        threads,
        onboarding,
        view_state,
        ndb,
        scoped_subs,
        RouteCleanup::Returned { return_type },
    );
}

/// Centralized resource cleanup for routes removed outside normal nav returns.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispose_removed_route(
    route: &Route,
    account_pk: Pubkey,
    column_id: ColumnId,
    timeline_cache: &mut TimelineCache,
    threads: &mut Threads,
    onboarding: &mut Onboarding,
    view_state: &mut ViewState,
    ndb: &mut Ndb,
    scoped_subs: &mut ScopedSubApi,
) {
    cleanup_route(
        route,
        column_id,
        timeline_cache,
        threads,
        onboarding,
        view_state,
        ndb,
        scoped_subs,
        RouteCleanup::Disposed { account_pk },
    );
}

#[allow(clippy::too_many_arguments)]
fn cleanup_route(
    route: &Route,
    column_id: ColumnId,
    timeline_cache: &mut TimelineCache,
    threads: &mut Threads,
    onboarding: &mut Onboarding,
    view_state: &mut ViewState,
    ndb: &mut Ndb,
    scoped_subs: &mut ScopedSubApi,
    cleanup: RouteCleanup,
) {
    match route {
        Route::Timeline(kind) => {
            let account_pk = match cleanup {
                RouteCleanup::Returned { .. } => scoped_subs.selected_account_pubkey(),
                RouteCleanup::Disposed { account_pk } => account_pk,
            };
            if let Err(err) = timeline_cache.pop_for_account(kind, account_pk, ndb, scoped_subs) {
                tracing::error!("popping timeline had an error: {err} for {:?}", kind);
            }
        }
        Route::Thread(selection) => match cleanup {
            RouteCleanup::Returned { return_type } => {
                threads.close(ndb, scoped_subs, selection, return_type, column_id);
            }
            RouteCleanup::Disposed { account_pk } => {
                threads.dispose_route_for_account(
                    ndb,
                    scoped_subs,
                    account_pk,
                    column_id,
                    selection,
                );
            }
        },
        Route::EditProfile(pk) => {
            view_state.pubkey_to_profile_state.remove(pk);
        }
        Route::Accounts(AccountsRoute::Onboarding) => {
            onboarding.end_onboarding(ndb);
            view_state.follow_packs = Default::default();
            let _ = scoped_subs.drop_owner(onboarding_owner_key(column_id));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use enostr::NoteId;
    use tokenator::{TokenParser, TokenWriter};

    use crate::{route::ColumnsRouter, timeline::ThreadSelection, Route};
    use enostr::Pubkey;
    use notedeck::RootNoteIdBuf;

    #[test]
    fn test_thread_route_serialize() {
        let note_id_hex = "1c54e5b0c386425f7e017d9e068ddef8962eb2ce1bb08ed27e24b93411c12e60";
        let note_id = NoteId::from_hex(note_id_hex).unwrap();
        let data_str = format!("thread:{}", note_id_hex);
        let data = &data_str.split(":").collect::<Vec<&str>>();
        let mut token_writer = TokenWriter::default();
        let mut parser = TokenParser::new(data);
        let parsed = Route::parse(&mut parser, &Pubkey::new(*note_id.bytes())).unwrap();
        let expected = Route::Thread(ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe(
            *note_id.bytes(),
        )));
        parsed.serialize_tokens(&mut token_writer);
        assert_eq!(expected, parsed);
        assert_eq!(token_writer.str(), data_str);
    }

    #[test]
    fn click_back_collapses_overlay_and_pop_returns_retained_top_route() {
        let mut router = ColumnsRouter::new(vec![0]);
        router.route_to_overlaid(1);
        router.route_to_overlaid(2);

        assert_eq!(router.routes(), &[0, 1, 2]);
        router.go_back();
        assert_eq!(router.routes(), &[0, 2]);

        let removed = router.pop();

        assert_eq!(removed, Some(2));
        assert_eq!(router.routes(), &[0]);
    }

    #[test]
    fn direct_route_disposal_clears_pending_return_state_without_forwarding() {
        let mut router = ColumnsRouter::new(vec![0]);
        router.route_to(1);
        assert_eq!(router.go_back(), Some(0));
        assert!(router.returning());

        let removed = router.remove_top_route_for_disposal();

        assert_eq!(removed, Some(1));
        assert_eq!(router.routes(), &[0]);
        assert!(!router.returning());
        assert!(!router.navigating());
        assert!(!router.go_forward());
    }
}
