use enostr::{NoteId, Pubkey};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    fmt::{self},
};

use crate::{
    accounts::AccountsRoute,
    column::Columns,
    timeline::{TimelineId, TimelineRoute},
    ui::add_column::AddColumnRoute,
};

pub type Router = egui_nav::Router<Vec<Route>>;

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

    pub fn title(&self, columns: &Columns) -> Cow<'static, str> {
        match self {
            Route::Timeline(tlr) => match tlr {
                TimelineRoute::Timeline(id) => {
                    let timeline = columns
                        .find_timeline(*id)
                        .expect("expected to find timeline");
                    timeline.kind.to_title()
                }
                TimelineRoute::Thread(_id) => Cow::Borrowed("Thread"),
                TimelineRoute::Reply(_id) => Cow::Borrowed("Reply"),
                TimelineRoute::Quote(_id) => Cow::Borrowed("Quote"),
                TimelineRoute::Profile(_pubkey) => Cow::Borrowed("Profile"),
            },

            Route::Relays => Cow::Borrowed("Relays"),

            Route::Accounts(amr) => match amr {
                AccountsRoute::Accounts => Cow::Borrowed("Accounts"),
                AccountsRoute::AddAccount => Cow::Borrowed("Add Account"),
            },
            Route::ComposeNote => Cow::Borrowed("Compose Note"),
            Route::AddColumn(c) => match c {
                AddColumnRoute::Base => Cow::Borrowed("Add Column"),
                AddColumnRoute::UndecidedNotification => Cow::Borrowed("Add Notifications Column"),
                AddColumnRoute::ExternalNotification => {
                    Cow::Borrowed("Add External Notifications Column")
                }
                AddColumnRoute::Hashtag => Cow::Borrowed("Add Hashtag Column"),
            },
            Route::Support => Cow::Borrowed("Damus Support"),
        }
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
        }
    }
}
