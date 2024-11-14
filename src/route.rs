use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use serde::{Deserialize, Serialize};
use std::fmt::{self};

use crate::{
    account_manager::AccountsRoute,
    column::Columns,
    note::RootNoteId,
    notecache::NoteCache,
    subscriptions::SubRefs,
    timeline::{TimelineCache, TimelineId, TimelineRoute},
    timeline::{TimelineId, TimelineRoute},
    ui::profile::preview::{get_note_users_displayname_string, get_profile_displayname_string},
    ui::{
        add_column::AddColumnRoute,
        profile::preview::{get_note_users_displayname_string, get_profile_displayname_string},
    },
};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum Route {
    Timeline(TimelineRoute),
    Accounts(AccountsRoute),
    Relays,
    ComposeNote,
    AddColumn(AddColumnRoute),
    Support,
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

    pub fn subscriptions<'a>(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        columns: &'a Columns,
        timeline_cache: &'a TimelineCache,
    ) -> Option<SubRefs<'a>> {
        match self {
            Route::Timeline(tlr) => {
                tlr.subscriptions(ndb, note_cache, txn, columns, timeline_cache)
            }
            Route::Accounts(_) => None,
            Route::Relays => None,
            Route::ComposeNote => None,
            Route::AddColumn => None,
            Route::Support => None,
        }
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

    pub fn thread(thread_root: RootNoteId) -> Self {
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
                    format!(
                        "{}'s Thread",
                        get_note_users_displayname_string(ndb, &id.to_note_id())
                    )
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
            Route::AddColumn(c) => match c {
                AddColumnRoute::Base => "Add Column".to_owned(),
                AddColumnRoute::UndecidedNotification => "Add Notifications Column".to_owned(),
                AddColumnRoute::ExternalNotification => {
                    "Add External Notifications Column".to_owned()
                }
            },
            Route::Profile(pubkey) => {
                format!("{}'s Profile", get_profile_displayname_string(ndb, pubkey))
            }
            Route::Support => "Damus Support".to_owned(),
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
                TimelineRoute::Profile(_) => write!(f, "Profile"),
            },

            Route::Relays => write!(f, "Relays"),

            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => write!(f, "Accounts"),
                AccountsRoute::AddAccount => write!(f, "Add Account"),
            },
            Route::ComposeNote => write!(f, "Compose Note"),

            Route::AddColumn(_) => write!(f, "Add Column"),
            Route::Profile(_) => write!(f, "Profile"),
            Route::Support => write!(f, "Support"),
        }
    }
}
