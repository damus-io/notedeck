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
    books_with(text, &Edits::default())
}

/// [`books`], derived with `edits`.
pub fn books_with(text: &str, edits: &Edits) -> Books {
    books_of(
        &[TestSegment {
            source: "t",
            imported: 1,
            text,
        }],
        edits,
    )
}

/// One segment of a test journal.
pub struct TestSegment<'a> {
    /// Its source, which also prefixes its record ids.
    pub source: &'a str,
    /// When it was imported, in unix seconds.
    pub imported: u64,
    /// Its transactions.
    pub text: &'a str,
}

/// Books loaded from `segments`, derived with `edits`, CAD as home. The
/// transactions are given record ids numbered across every segment
/// (`<source>:1`, `<source>:2`, …), since every one needs one.
pub fn books_of(segments: &[TestSegment<'_>], edits: &Edits) -> Books {
    let tmp = tempfile::tempdir().unwrap();
    let store = JournalStore::new(tmp.path());
    let mut id = 0;
    for segment in segments {
        let header = SegmentHeader {
            source: segment.source.to_owned(),
            imported: segment.imported,
            layer: "t".to_owned(),
        };
        let text: String = segment
            .text
            .split_inclusive('\n')
            .map(|line| {
                if line.starts_with(' ') || line.trim().is_empty() {
                    return line.to_owned();
                }
                id += 1;
                format!("{line}  ; id: {}:{id}\n", segment.source)
            })
            .collect();
        store
            .write_segment(&header, &Journal::parse(&text).journal)
            .unwrap();
    }
    Books::load(&store, LayerFilter::All, &HashSet::new(), edits, "CAD").unwrap()
}
