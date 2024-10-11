use enostr::{NoteId, Pubkey};
use nostrdb::Ndb;
use std::fmt::{self};

use crate::{
    account_manager::AccountsRoute,
    column::Columns,
    timeline::{TimelineId, TimelineRoute},
    ui::profile::preview::get_note_users_displayname_string,
};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Route {
    Timeline(TimelineRoute),
    Accounts(AccountsRoute),
    Relays,
    ComposeNote,
    AddColumn,
    Profile(Pubkey, TimelineId),
}

#[derive(Clone)]
pub struct TitledRoute {
    pub route: Route,
    pub title: String,
}

impl fmt::Display for TitledRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.title)
    }
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

    pub fn get_titled_route(&self, columns: &Columns, ndb: &Ndb) -> TitledRoute {
        let title = match self {
            Route::Timeline(tlr) => match tlr {
                TimelineRoute::Timeline(id) => {
                    let timeline = columns
                        .find_timeline(*id)
                        .expect("expected to find timeline");
                    timeline.kind.to_title(ndb)
                }
                TimelineRoute::Thread(id) => {
                    format!("{}'s Thread", get_note_users_displayname_string(ndb, id))
                }
                TimelineRoute::Reply(id) => {
                    format!("{}'s Reply", get_note_users_displayname_string(ndb, id))
                }
                TimelineRoute::Quote(id) => {
                    format!("{}'s Quote", get_note_users_displayname_string(ndb, id))
                }
            },

            Route::Relays => "Relays".to_owned(),

            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => "Accounts".to_owned(),
                AccountsRoute::AddAccount => "Add Account".to_owned(),
            },
            Route::ComposeNote => "Compose Note".to_owned(),
            Route::AddColumn => "Add Column".to_owned(),
            Route::Profile(_, id) => {
                let timeline = columns
                    .find_timeline(*id)
                    .expect("expected to find timeline");
                timeline.kind.to_title(ndb)
            }
        };

        TitledRoute {
            title,
            route: *self,
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

    // Route to R. Then when it is successfully placed, should call `remove_previous_route`
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
        self.routes.get(self.routes.len() - 2).cloned()
    }

    /// Pop a route, should only be called on a NavRespose::Returned reseponse
    pub fn pop(&mut self) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
        }
        self.returning = false;
        self.routes.pop()
    }

    pub fn remove_previous_route(&mut self) -> Option<R> {
        let num_routes = self.routes.len();
        if num_routes <= 1 {
            return None;
        }
        self.returning = false;
        self.replacing = false;
        Some(self.routes.remove(num_routes - 2))
    }

    pub fn is_replacing(&self) -> bool {
        self.replacing
    }

    pub fn top(&self) -> &R {
        self.routes.last().expect("routes can't be empty")
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
                TimelineRoute::Reply(_id) => write!(f, "Reply"),
                TimelineRoute::Quote(_id) => write!(f, "Quote"),
            },

            Route::Relays => write!(f, "Relays"),

            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => write!(f, "Accounts"),
                AccountsRoute::AddAccount => write!(f, "Add Account"),
            },
            Route::ComposeNote => write!(f, "Compose Note"),

            Route::AddColumn => write!(f, "Add Column"),
            Route::Profile(_, _) => write!(f, "Profile"),
        }
    }
}
