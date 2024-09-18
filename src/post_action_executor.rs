use enostr::{FilledKeypair, RelayPool};
use nostrdb::Note;
use tracing::info;

use crate::{draft::Draft, post::NewPost, ui::note::PostAction};

pub struct PostActionExecutor {}

impl PostActionExecutor {
    pub fn execute<'a>(
        poster: FilledKeypair<'_>,
        action: &'a PostAction,
        pool: &mut RelayPool,
        draft: &mut Draft,
        get_note: impl Fn(&'a NewPost, &[u8; 32]) -> Note<'a>,
    ) {
        match action {
            PostAction::Post(np) => {
                let note = get_note(np, &poster.secret_key.to_secret_bytes());

                let raw_msg = format!("[\"EVENT\",{}]", note.json().unwrap());
                info!("sending {}", raw_msg);
                pool.send(&enostr::ClientMessage::raw(raw_msg));
                draft.clear();
            }
        }
    }
}
