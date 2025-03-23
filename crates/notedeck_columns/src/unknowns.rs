/*
use crate::{timeline::TimelineCache, Result};
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::{CachedNote, NoteCache, UnknownIds};
use tracing::error;

pub fn update_from_columns(
    txn: &Transaction,
    unknown_ids: &mut UnknownIds,
    timeline_cache: &TimelineCache,
    ndb: &Ndb,
    note_cache: &mut NoteCache,
) -> bool {
    let before = unknown_ids.ids_iter().len();
    if let Err(e) = get_unknown_ids(txn, unknown_ids, timeline_cache, ndb, note_cache) {
        error!("UnknownIds::update {e}");
    }
    let after = unknown_ids.ids_iter().len();

    if before != after {
        unknown_ids.mark_updated();
        true
    } else {
        false
    }
}

#[profiling::function]
pub fn get_unknown_ids(
    txn: &Transaction,
    unknown_ids: &mut UnknownIds,
    timeline_cache: &TimelineCache,
    ndb: &Ndb,
    note_cache: &mut NoteCache,
) -> Result<()> {
    let mut new_cached_notes: Vec<(NoteKey, CachedNote)> = vec![];

    for (_kind, timeline) in timeline_cache.timelines.iter() {
        for noteref in timeline.all_or_any_notes() {
            let note = ndb.get_note_by_key(txn, noteref.key)?;
            let note_key = note.key().unwrap();
            let cached_note = note_cache.cached_note(noteref.key);
            let cached_note = if let Some(cn) = cached_note {
                cn.clone()
            } else {
                let new_cached_note = CachedNote::new(&note);
                new_cached_notes.push((note_key, new_cached_note.clone()));
                new_cached_note
            };

            let _ = notedeck::get_unknown_note_ids(
                ndb,
                &cached_note,
                txn,
                &note,
                unknown_ids.ids_mut(),
            );
        }
    }

    // This is mainly done to avoid the double mutable borrow that would happen
    // if we tried to update the note_cache mutably in the loop above
    for (note_key, note) in new_cached_notes {
        note_cache.cache_mut().insert(note_key, note);
    }

    Ok(())
}
*/
