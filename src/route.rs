use egui::RichText;
use enostr::NoteId;
use std::fmt::{self};
use strum_macros::Display;

use crate::ui::{
    account_login_view::AccountLoginResponse, account_management::AccountManagementViewResponse,
};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Debug)]
pub enum Route {
    Timeline(String),
    Thread(NoteId),
    Reply(NoteId),
    Relays,
}

#[derive(Clone, Debug, Default, Display)]
pub enum ManageAccountRoute {
    #[default]
    AccountManagement,
    AddAccount,
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Timeline(name) => write!(f, "{}", name),
            Route::Thread(_id) => write!(f, "Thread"),
            Route::Reply(_id) => write!(f, "Reply"),
            Route::Relays => write!(f, "Relays"),
        }
    }
}

impl Route {
    pub fn title(&self) -> RichText {
        match self {
            Route::Thread(_) => RichText::new("Thread"),
            Route::Reply(_) => RichText::new("Reply"),
            Route::Relays => RichText::new("Relays"),
            Route::Timeline(_) => RichText::new("Timeline"),
        }
    }
}

pub enum ManageAcountRouteResponse {
    AccountManagement(AccountManagementViewResponse),
    AddAccount(AccountLoginResponse),
}
