use egui::{Response, RichText, Vec2};
use enostr::NoteId;
use std::fmt::{self};

use crate::{
    app_style::NotedeckTextStyle,
    ui::{account_login_view::AccountLoginView, AccountManagementView},
    Damus,
};

/// App routing. These describe different places you can go inside Notedeck.
#[derive(Clone, Debug)]
pub enum Route {
    Timeline(String),
    ManageAccount,
    AddAccount,
    Thread(NoteId),
    Reply(NoteId),
    Relays,
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::ManageAccount => write!(f, "Manage Account"),
            Route::AddAccount => write!(f, "Add Account"),
            Route::Timeline(name) => write!(f, "{}", name),
            Route::Thread(_id) => write!(f, "Thread"),
            Route::Reply(_id) => write!(f, "Reply"),
            Route::Relays => write!(f, "Relays"),
        }
    }
}

impl Route {
    pub fn show_global_popup(&self, app: &mut Damus, ui: &mut egui::Ui) -> Option<Response> {
        match self {
            Route::ManageAccount => AccountManagementView::ui(app, ui),
            Route::AddAccount => Some(AccountLoginView::ui(app, ui)),
            _ => None,
        }
    }

    pub fn preferred_rect(&self, ui: &mut egui::Ui) -> egui::Rect {
        let screen_rect = ui.ctx().screen_rect();
        match self {
            Route::ManageAccount => screen_rect.shrink(200.0),
            Route::AddAccount => {
                let size = Vec2::new(560.0, 480.0);
                egui::Rect::from_center_size(screen_rect.center(), size)
            }
            _ => screen_rect,
        }
    }

    pub fn title(&self) -> RichText {
        match self {
            Route::ManageAccount => RichText::new("Manage Account").size(24.0),
            Route::AddAccount => RichText::new("Login")
                .text_style(NotedeckTextStyle::Heading2.text_style())
                .strong(),
            Route::Thread(_) => RichText::new("Thread"),
            Route::Reply(_) => RichText::new("Reply"),
            Route::Relays => RichText::new("Relays"),
            Route::Timeline(_) => RichText::new("Timeline"),
        }
    }
}
