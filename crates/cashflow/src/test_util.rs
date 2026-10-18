//! Test fixtures shared by this crate's unit tests.

use std::collections::HashSet;
use std::time::Duration;

use ledgerdb::{Edits, Journal, JournalStore, LayerFilter, SegmentHeader};

use crate::Books;

pub use ndb_testing::TempNdb;

/// Backstop for an ingest wait. A local ingest commits in microseconds, so
/// this only trips when a note is never coming.
pub const INGEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Books loaded from `text`, a journal in one segment with CAD as home.
/// Each transaction is given a record id (`t:1`, `t:2`, …), since every one
/// needs one.
pub fn books(text: &str) -> Books {
    let tmp = tempfile::tempdir().unwrap();
    let store = JournalStore::new(tmp.path());
    let header = SegmentHeader {
        source: "t".to_owned(),
        imported: 1,
        layer: "t".to_owned(),
    };
    // Every transaction needs a record id: number them.
    let mut id = 0;
    let text: String = text
        .split_inclusive('\n')
        .map(|line| {
            if line.starts_with(' ') || line.trim().is_empty() {
                return line.to_owned();
            }
            id += 1;
            format!("{line}  ; id: t:{id}\n")
        })
        .collect();
    store
        .write_segment(&header, &Journal::parse(&text).journal)
        .unwrap();
    Books::load(
        &store,
        LayerFilter::All,
        &HashSet::new(),
        &Edits::default(),
        "CAD",
    )
    .unwrap()
}
