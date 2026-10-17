//! The journal store: a directory of write-once segments, each filed under
//! the hash of its bytes.
//!
//! ```text
//! <dir>/2c/2cf24dba…9824.ledger
//! <dir>/9f/9f86d081…0a08.ledger
//! ```
//!
//! A file is written to a temporary name beside its final one, synced, and
//! renamed into place, so a reader never sees half a segment, and it is
//! never changed afterwards. Since a name is a hash of the contents, two
//! stores can be combined by copying files in either direction: the same
//! name always means the same bytes, so there is nothing to conflict. Every
//! read checks the hash, so a corrupted or hand-edited file is an error that
//! names it rather than wrong numbers.
//!
//! Files that are not named `<64 hex>.ledger` inside the matching two-digit
//! shard, such as temporary files, rsync partials, a `.git` directory or the
//! `.trash/` that [`JournalStore::trash`] retires segments into, are ignored.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{EncodeError, HeaderError, Journal, Segment, SegmentHash, SegmentHeader};

/// The extension every segment file has.
const EXTENSION: &str = ".ledger";

/// The directory under the store that [`JournalStore::trash`] moves
/// segments into. It is not a shard, so nothing in it is listed or read.
const TRASH: &str = ".trash";

/// A directory of content-addressed journal segments.
#[derive(Clone, Debug)]
pub struct JournalStore {
    dir: PathBuf,
}

/// Something that went wrong reading or writing the store. Every variant
/// about a file names it.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Reading or writing a file or directory failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What failed.
        source: io::Error,
    },
    /// The batch cannot be written as a segment.
    #[error("cannot write segment: {0}")]
    Encode(#[from] EncodeError),
    /// A file's bytes do not hash to its name: it was corrupted or edited.
    #[error("{}: contents hash to {actual}, not the name; the file is corrupt or was edited", path.display())]
    HashMismatch {
        /// The file.
        path: PathBuf,
        /// What the bytes actually hash to.
        actual: SegmentHash,
    },
    /// A file is not UTF-8 text.
    #[error("{}: not UTF-8 text", path.display())]
    NotUtf8 {
        /// The file.
        path: PathBuf,
    },
    /// A file's header cannot be read.
    #[error("{}: {source}", path.display())]
    Header {
        /// The file.
        path: PathBuf,
        /// What is wrong with the header.
        source: HeaderError,
    },
}

/// What [`JournalStore::verify_all`] found.
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// Segments whose bytes match their names and whose headers read.
    pub ok: Vec<SegmentHash>,
    /// Every segment that failed, with why.
    pub bad: Vec<StoreError>,
}

/// What [`JournalStore::admit_from`] did with a staging directory.
#[derive(Debug, Default)]
pub struct AdmitReport {
    /// Segments that were new to the store and are now in it.
    pub admitted: Vec<SegmentHash>,
    /// Segments the store already had; their staging copies were not read.
    pub present: Vec<SegmentHash>,
    /// Staged files that were refused, with why. They are left where they
    /// are.
    pub rejected: Vec<StoreError>,
}

/// What [`JournalStore::put_segment`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PutSegment {
    /// The segment's hash, its name in the store.
    pub hash: SegmentHash,
    /// False when the store already had these exact bytes, so nothing was
    /// written.
    pub new: bool,
}

/// Whether [`JournalStore::put`] wrote a file.
enum Put {
    /// The segment was new and is now written.
    Written,
    /// The store already had it.
    Present,
}

/// A hash-named file found while scanning a directory.
struct Found {
    hash: SegmentHash,
    path: PathBuf,
}

impl JournalStore {
    /// The store in `dir`. Nothing is created until the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory the store lives in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the segment `hash` is filed: `<dir>/<first two hex>/<hash>.ledger`.
    pub fn path_of(&self, hash: SegmentHash) -> PathBuf {
        let name = hash.to_string();
        self.dir.join(&name[..2]).join(name + EXTENSION)
    }

    /// Whether the store has the segment `hash`.
    pub fn contains(&self, hash: SegmentHash) -> bool {
        self.path_of(hash).is_file()
    }

    /// Write `journal` under `header` as a segment and return its hash. If
    /// the store already has it, nothing is written. Safe to call
    /// concurrently with the same or other segments, from threads or
    /// processes.
    ///
    /// See [`Segment::encode`] for what the batch must satisfy.
    pub fn write_segment(
        &self,
        header: &SegmentHeader,
        journal: &Journal,
    ) -> Result<SegmentHash, StoreError> {
        Ok(self.put_segment(header, journal)?.hash)
    }

    /// [`Self::write_segment`], also saying whether the segment was new to
    /// the store, so a caller can report "already in the journal" without
    /// encoding the batch a second time to hash it.
    #[profiling::function]
    pub fn put_segment(
        &self,
        header: &SegmentHeader,
        journal: &Journal,
    ) -> Result<PutSegment, StoreError> {
        let bytes = Segment::encode(header, journal)?;
        let hash = SegmentHash::of(bytes.as_bytes());
        let new = matches!(self.put(hash, bytes.as_bytes())?, Put::Written);
        Ok(PutSegment { hash, new })
    }

    /// Every segment in the store, sorted by hash. An empty or missing
    /// directory has none.
    pub fn list(&self) -> Result<Vec<SegmentHash>, StoreError> {
        let mut hashes: Vec<SegmentHash> = scan(&self.dir, true)?
            .into_iter()
            .map(|found| found.hash)
            .collect();
        hashes.sort_unstable();
        Ok(hashes)
    }

    /// Read the segment `hash`, checking that its bytes still hash to its
    /// name.
    #[profiling::function]
    pub fn read(&self, hash: SegmentHash) -> Result<Segment, StoreError> {
        read_checked(hash, &self.path_of(hash))
    }

    /// Move the segment `hash` out of the store into `<dir>/.trash/`, where
    /// [`Self::list`] no longer sees it, and return where it went.
    ///
    /// This is the one exception to segments being write-once, for retiring
    /// an import that a better one replaces. It is local: a sync transport
    /// unions files, so a copy of the segment anywhere else comes back on
    /// the next sync.
    pub fn trash(&self, hash: SegmentHash) -> Result<PathBuf, StoreError> {
        let path = self.path_of(hash);
        let trash = self.dir.join(TRASH);
        fs::create_dir_all(&trash).map_err(|source| io_error(&trash, source))?;
        let to = trash.join(hash.to_string() + EXTENSION);
        fs::rename(&path, &to).map_err(|source| io_error(&path, source))?;
        sync_dir(path.parent().expect("a segment path has a shard"))?;
        sync_dir(&trash)?;
        Ok(to)
    }

    /// Read every segment, reporting the ones that are corrupt or unreadable
    /// instead of stopping at the first.
    pub fn verify_all(&self) -> Result<VerifyReport, StoreError> {
        let mut report = VerifyReport::default();
        for hash in self.list()? {
            match self.read(hash) {
                Ok(_) => report.ok.push(hash),
                Err(err) => report.bad.push(err),
            }
        }
        Ok(report)
    }

    /// Admit the hash-named files in `staging`, at its top level or in
    /// two-digit shards like the store's own, checking each one's hash and
    /// header and writing it in the same way [`Self::write_segment`] does.
    ///
    /// **This is the only way anything fetched from elsewhere gets into the
    /// store.** A sync transport fetches into a staging directory and calls
    /// this; it never writes into the store itself. Staged files are only
    /// read, never moved or deleted, whatever the outcome.
    #[profiling::function]
    pub fn admit_from(&self, staging: &Path) -> Result<AdmitReport, StoreError> {
        let mut found = scan(staging, false)?;
        found.sort_unstable_by_key(|found| found.hash);
        found.dedup_by_key(|found| found.hash);

        let mut report = AdmitReport::default();
        for Found { hash, path } in found {
            if self.contains(hash) {
                report.present.push(hash);
                continue;
            }
            let checked = fs::read(&path)
                .map_err(|source| io_error(&path, source))
                .and_then(|bytes| {
                    check(hash, &path, &bytes)?;
                    self.put(hash, &bytes)
                });
            match checked {
                Ok(Put::Written) => report.admitted.push(hash),
                Ok(Put::Present) => report.present.push(hash),
                Err(err) => report.rejected.push(err),
            }
        }
        Ok(report)
    }

    /// File `bytes`, which hash to `hash`, unless the store has them: write a
    /// temporary file in the shard, sync it, rename it into place, and sync
    /// the shard so the rename survives a crash.
    fn put(&self, hash: SegmentHash, bytes: &[u8]) -> Result<Put, StoreError> {
        let path = self.path_of(hash);
        if path.is_file() {
            return Ok(Put::Present);
        }
        let shard = path.parent().expect("a segment path has a shard");
        fs::create_dir_all(shard).map_err(|source| io_error(shard, source))?;

        let mut temp =
            tempfile::NamedTempFile::new_in(shard).map_err(|source| io_error(shard, source))?;
        temp.write_all(bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|source| io_error(temp.path(), source))?;
        // Renaming over a copy another writer just put there is harmless: it
        // has the same bytes.
        if let Err(err) = temp.persist(&path)
            && !path.is_file()
        {
            return Err(io_error(&path, err.error));
        }
        sync_dir(shard)?;
        Ok(Put::Written)
    }
}

/// Read the file at `path`, which should be the segment `hash`; see
/// [`check`].
fn read_checked(hash: SegmentHash, path: &Path) -> Result<Segment, StoreError> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    check(hash, path, &bytes)
}

/// Read `bytes`, from the file at `path`, as the segment `hash`, failing
/// unless they hash to `hash` and have a segment header.
fn check(hash: SegmentHash, path: &Path, bytes: &[u8]) -> Result<Segment, StoreError> {
    let actual = SegmentHash::of(bytes);
    if actual != hash {
        return Err(StoreError::HashMismatch {
            path: path.to_owned(),
            actual,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| StoreError::NotUtf8 {
        path: path.to_owned(),
    })?;
    Segment::parse(hash, text).map_err(|source| StoreError::Header {
        path: path.to_owned(),
        source,
    })
}

/// The hash-named files in `dir`'s two-digit shards, and, unless `sharded`,
/// at its top level too. A shard file only counts when its hash starts with
/// the shard's name. A missing `dir` has none.
fn scan(dir: &Path, sharded: bool) -> Result<Vec<Found>, StoreError> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(source) => return Err(io_error(dir, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| io_error(dir, source))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if path.is_file() {
            if let Some(hash) = hash_named(&name).filter(|_| !sharded) {
                found.push(Found { hash, path });
            }
            continue;
        }
        if !is_shard_name(&name) || !path.is_dir() {
            continue;
        }
        let files = fs::read_dir(&path).map_err(|source| io_error(&path, source))?;
        for file in files {
            let file = file.map_err(|source| io_error(&path, source))?;
            let Some(hash) = file.file_name().to_str().and_then(hash_named) else {
                continue;
            };
            if hash.to_string().starts_with(&name) && file.path().is_file() {
                found.push(Found {
                    hash,
                    path: file.path(),
                });
            }
        }
    }
    Ok(found)
}

/// The hash in a `<64 hex>.ledger` file name.
fn hash_named(name: &str) -> Option<SegmentHash> {
    name.strip_suffix(EXTENSION)?.parse().ok()
}

/// Whether `name` is two lowercase hex digits.
fn is_shard_name(name: &str) -> bool {
    name.len() == 2
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Make a rename in `dir` durable. Directories cannot be opened for syncing
/// outside unix, where the rename is left to the filesystem.
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|source| io_error(dir, source))?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// An I/O failure on `path`.
fn io_error(path: &Path, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_owned(),
        source,
    }
}
