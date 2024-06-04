use enostr::NoteId;
use std::fmt;

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Debug)]
pub enum Route {
    Timeline(String),
    ManageAccount,
    Thread(NoteId),
    Reply(NoteId),
    Relays,
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::ManageAccount => write!(f, "Manage Account"),
            Route::Timeline(name) => write!(f, "{}", name),
            Route::Thread(_id) => write!(f, "Thread"),
            Route::Reply(_id) => write!(f, "Reply"),
            Route::Relays => write!(f, "Relays"),
        }
    }
}
