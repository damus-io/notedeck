use enostr::{NoteId, Pubkey};
use std::fmt::{self};

use crate::{
    accounts::AccountsRoute,
    column::Columns,
    timeline::{kind::ColumnTitle, TimelineId, TimelineRoute},
    ui::add_column::AddColumnRoute,
};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Route {
    Timeline(TimelineRoute),
    Accounts(AccountsRoute),
    Relays,
    ComposeNote,
    AddColumn(AddColumnRoute),
    EditProfile(Pubkey),
    Support,
    NewDeck,
    EditDeck(usize),
}

impl Route {
    pub fn timeline(timeline_id: TimelineId) -> Self {
        Route::Timeline(TimelineRoute::Timeline(timeline_id))
    }

    pub fn timeline_id(&self) -> Option<&TimelineId> {
        if let Route::Timeline(TimelineRoute::Timeline(tid)) = self {
            Some(tid)
        } else {
            None
        }
    }

    pub fn relays() -> Self {
        Route::Relays
    }

    pub fn thread(thread_root: NoteId) -> Self {
        Route::Timeline(TimelineRoute::Thread(thread_root))
    }

    pub fn profile(pubkey: Pubkey) -> Self {
        Route::Timeline(TimelineRoute::Profile(pubkey))
    }

    pub fn reply(replying_to: NoteId) -> Self {
        Route::Timeline(TimelineRoute::Reply(replying_to))
    }

    pub fn quote(quoting: NoteId) -> Self {
        Route::Timeline(TimelineRoute::Quote(quoting))
    }

    pub fn accounts() -> Self {
        Route::Accounts(AccountsRoute::Accounts)
    }

    pub fn add_account() -> Self {
        Route::Accounts(AccountsRoute::AddAccount)
    }

    pub fn title<'a>(&self, columns: &'a Columns) -> ColumnTitle<'a> {
        match self {
            Route::Timeline(tlr) => match tlr {
                TimelineRoute::Timeline(id) => {
                    if let Some(timeline) = columns.find_timeline(*id) {
                        timeline.kind.to_title()
                    } else {
                        ColumnTitle::simple("Unknown")
                    }
                }
                TimelineRoute::Thread(_id) => ColumnTitle::simple("Thread"),
                TimelineRoute::Reply(_id) => ColumnTitle::simple("Reply"),
                TimelineRoute::Quote(_id) => ColumnTitle::simple("Quote"),
                TimelineRoute::Profile(_pubkey) => ColumnTitle::simple("Profile"),
            },

            Route::Relays => ColumnTitle::simple("Relays"),

            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => ColumnTitle::simple("Accounts"),
                AccountsRoute::AddAccount => ColumnTitle::simple("Add Account"),
            },
            Route::ComposeNote => ColumnTitle::simple("Compose Note"),
            Route::AddColumn(c) => match c {
                AddColumnRoute::Base => ColumnTitle::simple("Add Column"),
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
        }
    }
}

// TODO: add this to egui-nav so we don't have to deal with returning
// and navigating headaches
#[derive(Clone)]
pub struct Router<R: Clone> {
    routes: Vec<R>,
    pub returning: bool,
    pub navigating: bool,
    replacing: bool,
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
        }
    }

    pub fn route_to(&mut self, route: R) {
        self.navigating = true;
        self.routes.push(route);
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
        self.prev().cloned()
    }

    /// Pop a route, should only be called on a NavRespose::Returned reseponse
    pub fn pop(&mut self) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
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

    pub fn is_replacing(&self) -> bool {
        self.replacing
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
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Timeline(tlr) => match tlr {
                TimelineRoute::Timeline(name) => write!(f, "{}", name),
                TimelineRoute::Thread(_id) => write!(f, "Thread"),
                TimelineRoute::Profile(_id) => write!(f, "Profile"),
                TimelineRoute::Reply(_id) => write!(f, "Reply"),
                TimelineRoute::Quote(_id) => write!(f, "Quote"),
            },

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
        }
    }
}
