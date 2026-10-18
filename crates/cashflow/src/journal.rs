//! The journal directory under `$CASHFLOW_DIR`: what is in it, and whether
//! every file is still what its name says.
//!
//! The store itself is ledgerdb's [`JournalStore`]; this module is the
//! cashflow-shaped view of it the CLI's `journal ls`/`journal verify`/
//! `journal rm` and the app's journal status print. What the vault knows
//! about each segment is [`crate::manifest`].

use std::path::PathBuf;

use ledgerdb::{DamagedSegment, Date, JournalStore, SegmentHash, SegmentHeader, StoreError};

use crate::CashflowDir;

/// The journal store in `dir`'s `journal/`.
pub fn store(dir: &CashflowDir) -> JournalStore {
    JournalStore::new(dir.journal())
}

/// One segment, as `journal ls` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentSummary {
    pub hash: SegmentHash,
    pub header: SegmentHeader,
    /// Transactions that parsed.
    pub txns: usize,
    /// `P` directives.
    pub prices: usize,
    /// Lines that did not parse; a segment cashflow wrote has none.
    pub errors: usize,
    /// The earliest transaction or price date, `None` for an empty segment.
    pub first: Option<Date>,
    /// The latest.
    pub last: Option<Date>,
    /// The file's size.
    pub bytes: u64,
}

/// What [`list`] read.
#[derive(Debug, Default)]
pub struct Listing {
    /// Readable segments, oldest import first (then by hash).
    pub segments: Vec<SegmentSummary>,
    /// Files that could not be read or whose bytes don't match their name.
    pub bad: Vec<StoreError>,
}

/// Read every segment's header and count what it holds. Only listing the
/// directory can fail; a bad file is reported in [`Listing::bad`].
pub fn list(store: &JournalStore) -> Result<Listing, StoreError> {
    let mut listing = Listing::default();
    for hash in store.list()? {
        match summarize(store, hash) {
            Ok(summary) => listing.segments.push(summary),
            Err(err) => listing.bad.push(err),
        }
    }
    listing
        .segments
        .sort_by_key(|s| (s.header.imported, s.hash));
    Ok(listing)
}

/// Read segment `hash` and sum up what it holds, as [`list`] does for each.
pub fn summarize(store: &JournalStore, hash: SegmentHash) -> Result<SegmentSummary, StoreError> {
    let segment = store.read(hash)?;
    let bytes = std::fs::metadata(store.path_of(hash)).map_or(0, |m| m.len());
    let journal = &segment.journal;
    let dates = journal
        .txns
        .iter()
        .map(|t| t.date)
        .chain(journal.prices.iter().map(|p| p.date));
    Ok(SegmentSummary {
        hash,
        txns: journal.txns.len(),
        prices: journal.prices.len(),
        errors: segment.errors.len(),
        first: dates.clone().min(),
        last: dates.max(),
        bytes,
        header: segment.header,
    })
}

/// What [`verify`] found.
#[derive(Debug, Default)]
pub struct Verified {
    /// Segments whose bytes match their names and that parse cleanly.
    pub ok: Vec<SegmentHash>,
    /// Segments that match their names but have lines that don't parse
    /// (written by something other than cashflow).
    pub damaged: Vec<DamagedSegment>,
    /// Files that are unreadable, corrupt or edited, or whose header doesn't
    /// read.
    pub bad: Vec<StoreError>,
}

impl Verified {
    /// Whether every segment is intact and parses.
    pub fn is_clean(&self) -> bool {
        self.damaged.is_empty() && self.bad.is_empty()
    }
}

/// Read every segment, checking its hash, header and body.
pub fn verify(store: &JournalStore) -> Result<Verified, StoreError> {
    let mut verified = Verified::default();
    for hash in store.list()? {
        match store.read(hash) {
            Ok(segment) if segment.errors.is_empty() => verified.ok.push(hash),
            Ok(segment) => verified.damaged.push(DamagedSegment {
                hash,
                errors: segment.errors,
            }),
            Err(err) => verified.bad.push(err),
        }
    }
    Ok(verified)
}

/// Why [`remove`] removed nothing.
#[derive(Debug, thiserror::Error)]
pub enum RemoveError {
    /// The prefix is empty or not lowercase hex.
    #[error("{0:?} is not a segment hash or a prefix of one")]
    NotHex(String),
    /// No segment's hash starts with the prefix.
    #[error("no segment {0}…")]
    NoMatch(String),
    /// More than one segment's hash starts with the prefix.
    #[error("{prefix}… names {count} segments; give more of the hash")]
    Ambiguous { prefix: String, count: usize },
    /// The store could not be listed or the file moved.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What [`remove`] did.
#[derive(Debug)]
pub struct Removed {
    pub hash: SegmentHash,
    /// Where the file is now, under `journal/.trash/`.
    pub to: PathBuf,
}

/// Retire the one segment whose hash starts with `prefix` (the whole hash,
/// or as much of it as `journal ls` makes unique) by moving it to the
/// store's `.trash/`. Its records drop out of every later load.
///
/// The move is local only: sync is a union of files, so the segment comes
/// back from any other copy of the journal that still has it.
pub fn remove(store: &JournalStore, prefix: &str) -> Result<Removed, RemoveError> {
    let hash = resolve(store.list()?, prefix)?;
    let to = store.trash(hash)?;
    Ok(Removed { hash, to })
}

/// The one hash in `hashes` that starts with `prefix`. `hashes` may repeat
/// one, as when it chains the local store with the vault's manifests.
pub fn resolve(
    hashes: impl IntoIterator<Item = SegmentHash>,
    prefix: &str,
) -> Result<SegmentHash, RemoveError> {
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if prefix.is_empty() || !prefix.bytes().all(is_hex) {
        return Err(RemoveError::NotHex(prefix.to_owned()));
    }
    let mut matches: Vec<SegmentHash> = hashes
        .into_iter()
        .filter(|hash| hash.to_string().starts_with(prefix))
        .collect();
    matches.sort_unstable();
    matches.dedup();
    match matches.as_slice() {
        [] => Err(RemoveError::NoMatch(prefix.to_owned())),
        [hash] => Ok(*hash),
        _ => Err(RemoveError::Ambiguous {
            prefix: prefix.to_owned(),
            count: matches.len(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledgerdb::{Journal, SegmentHeader};

    fn header(source: &str, imported: u64) -> SegmentHeader {
        SegmentHeader {
            source: source.to_owned(),
            imported,
            layer: "test".to_owned(),
        }
    }

    fn batch(id: &str) -> Journal {
        let text = format!(
            "2024-01-02 * Shop\n  ; id: {id}\n  a:b  -1 CAD\n  c:d\n\nP 2024-01-01 X 2 CAD\n"
        );
        Journal::parse(&text).journal
    }

    #[test]
    fn list_and_verify_report_each_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        let store = store(&dir);
        assert!(list(&store).unwrap().segments.is_empty());

        let late = store
            .write_segment(&header("b", 20), &batch("b:1"))
            .unwrap();
        let early = store
            .write_segment(&header("a", 10), &batch("a:1"))
            .unwrap();
        let listing = list(&store).unwrap();
        let hashes: Vec<_> = listing.segments.iter().map(|s| s.hash).collect();
        assert_eq!(hashes, [early, late]);
        assert_eq!(listing.segments[0].txns, 1);
        assert_eq!(listing.segments[0].prices, 1);
        assert_eq!(listing.segments[0].first, Date::parse("2024-01-01").ok());
        assert_eq!(listing.segments[0].last, Date::parse("2024-01-02").ok());
        let len = std::fs::metadata(store.path_of(early)).unwrap().len();
        assert_eq!(listing.segments[0].bytes, len);
        assert!(verify(&store).unwrap().is_clean());

        std::fs::write(store.path_of(late), "; edited\n").unwrap();
        let verified = verify(&store).unwrap();
        assert_eq!(verified.ok, [early]);
        assert!(matches!(
            verified.bad.as_slice(),
            [StoreError::HashMismatch { .. }]
        ));
        assert_eq!(list(&store).unwrap().bad.len(), 1);
    }

    #[test]
    fn remove_takes_a_unique_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&CashflowDir::new(tmp.path()));
        let hashes: Vec<_> = (0..40)
            .map(|i| {
                store
                    .write_segment(&header("a", i), &batch(&format!("a:{i}")))
                    .unwrap()
            })
            .collect();
        let hex = hashes[0].to_string();
        // Forty hashes share some one-digit prefix.
        let shared = (0..16u8)
            .map(|d| format!("{d:x}"))
            .find(|d| {
                hashes
                    .iter()
                    .filter(|h| h.to_string().starts_with(d))
                    .count()
                    > 1
            })
            .unwrap();

        assert!(matches!(remove(&store, ""), Err(RemoveError::NotHex(_))));
        assert!(matches!(remove(&store, "XY"), Err(RemoveError::NotHex(_))));
        assert!(matches!(
            remove(&store, &shared),
            Err(RemoveError::Ambiguous { count, .. }) if count > 1
        ));
        let removed = remove(&store, &hex[..12]).unwrap();
        assert_eq!(removed.hash, hashes[0]);
        assert!(removed.to.starts_with(store.dir().join(".trash")));
        assert_eq!(list(&store).unwrap().segments.len(), 39);
        assert!(matches!(remove(&store, &hex), Err(RemoveError::NoMatch(_))));
    }
}
