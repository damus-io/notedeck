use super::context::ContextSelection;
use crate::zaps::NoteZapTargetOwned;
use enostr::{NoteId, Pubkey};

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum NoteAction {
    /// User has clicked the quote reply action
    Reply(NoteId),

    /// User has clicked the quote repost action
    Quote(NoteId),

    /// User has clicked a hashtag
    Hashtag(String),

    /// User has clicked a profile
    Profile(Pubkey),

    /// User has clicked a note link
    Note(NoteId),

    /// User has selected some context option
    Context(ContextSelection),

    /// User has clicked the zap action
    Zap(ZapAction),

    /// User clicked on media
    Media(MediaAction),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ZapAction {
    Send(NoteZapTargetOwned),
    ClearError(NoteZapTargetOwned),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum MediaAction {
    Unblur(String), // URL to unblur
}

impl MediaAction {
    pub fn process(&self, ui: &egui::Ui) {
        match &self {
            MediaAction::Unblur(url) => send_unblur_signal(ui.ctx(), url),
        }
    }
}

fn send_unblur_signal(ctx: &egui::Context, url: &str) {
    let id = egui::Id::new(("blur", url));
    ctx.data_mut(|d| d.insert_temp(id, false))
}
