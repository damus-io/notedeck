use enostr::{FilledKeypair, RelayPool};
use nostrdb::Note;
use tracing::info;

use crate::{draft::Drafts, post::NewPost, ui::note::PostAction};

pub struct PostActionExecutor {}

impl PostActionExecutor {
    pub fn execute<'a>(
        poster: &FilledKeypair<'_>,
        action: &'a PostAction,
        pool: &mut RelayPool,
        drafts: &mut Drafts,
        get_note: impl Fn(&'a NewPost, &[u8; 32]) -> Note<'a>,
        clear_draft: impl Fn(&mut Drafts),
    ) {
        match action {
            PostAction::Post(np) => {
                let note = get_note(np, &poster.secret_key.to_secret_bytes());

                let raw_msg = format!("[\"EVENT\",{}]", note.json().unwrap());
                info!("sending {}", raw_msg);
                pool.send(&enostr::ClientMessage::raw(raw_msg));
                clear_draft(drafts);
            }
        }
    }
}
