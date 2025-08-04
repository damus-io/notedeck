use super::context::ContextSelection;
use crate::{zaps::NoteZapTargetOwned, MediaAction};
use egui::Vec2;
use enostr::{NoteId, Pubkey};

#[derive(Debug)]
pub struct ScrollInfo {
    pub velocity: Vec2,
    pub offset: Vec2,
}

#[derive(Debug)]
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
    Note {
        note_id: NoteId,
        preview: bool,
        scroll_offset: f32,
    },

    /// User has selected some context option
    Context(ContextSelection),

    /// User has clicked the zap action
    Zap(ZapAction),

    /// User clicked on media
    Media(MediaAction),

    /// User scrolled the timeline
    Scroll(ScrollInfo),
}

impl NoteAction {
    pub fn note(id: NoteId) -> NoteAction {
        NoteAction::Note {
            note_id: id,
            preview: false,
            scroll_offset: 0.0,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ZapAction {
    Send(ZapTargetAmount),
    CustomizeAmount(NoteZapTargetOwned),
    ClearError(NoteZapTargetOwned),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ZapTargetAmount {
    pub target: NoteZapTargetOwned,
    pub specified_msats: Option<u64>, // if None use default amount
}
