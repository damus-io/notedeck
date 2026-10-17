//! Journal segments: one import batch as canonical ledger text, named by the
//! sha256 of its bytes.
//!
//! A segment is a header of comment lines, a blank line, and a journal in
//! the canonical form [`Journal::write_ledger`] writes:
//!
//! ```text
//! ; ledgerdb-segment: 1
//! ; source: amex
//! ; imported: 1760000000
//! ; layer: cards
//!
//! 2024-01-02 * Payee
//! \t; id: amex:1042
//! \tliabilities:amex\t-4.50 CAD
//! \texpenses:unknown
//! ```
//!
//! Because the writer is canonical, a segment's bytes depend only on its
//! header and its journal's contents, so the same batch written twice, on
//! any machine, has the same name. [`Segment::encode`] makes the bytes and
//! [`Segment::parse`] reads them back; encoding a parsed segment gives the
//! same bytes again.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::{Journal, ParseError, TxnId, WriteOptions};

/// The segment format version this crate writes and reads.
pub const SEGMENT_VERSION: u32 = 1;

/// The header keys, in the order they are written.
const VERSION_KEY: &str = "ledgerdb-segment";
const SOURCE_KEY: &str = "source";
const IMPORTED_KEY: &str = "imported";
const LAYER_KEY: &str = "layer";

/// The sha256 of a segment's bytes, which is also its name.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentHash(pub [u8; 32]);

impl SegmentHash {
    /// The hash of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// Lowercase hex, the form used in file names.
impl fmt::Display for SegmentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for SegmentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SegmentHash({self})")
    }
}

/// A hash that is not 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a segment hash is 64 lowercase hex digits")]
pub struct BadSegmentHash;

/// Reads exactly 64 lowercase hex digits; uppercase is rejected so that one
/// hash has one file name.
impl FromStr for SegmentHash {
    type Err = BadSegmentHash;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.as_bytes();
        if digits.len() != 64 {
            return Err(BadSegmentHash);
        }
        let mut hash = [0u8; 32];
        let (pairs, _) = digits.as_chunks::<2>();
        for (byte, &[high, low]) in hash.iter_mut().zip(pairs) {
            *byte = (lower_hex(high).ok_or(BadSegmentHash)? << 4)
                | lower_hex(low).ok_or(BadSegmentHash)?;
        }
        Ok(Self(hash))
    }
}

/// The value of one lowercase hex digit.
fn lower_hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

/// What a segment says about itself: where its batch came from, when it was
/// imported, and which layer it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SegmentHeader {
    /// The source the batch was imported from, such as a bank or a wallet.
    pub source: String,
    /// When the batch was imported, in unix seconds. When two segments carry
    /// the same record id, the later import wins.
    pub imported: u64,
    /// The layer the segment belongs to, so a reader can include or leave out
    /// whole groups of segments.
    pub layer: String,
}

/// A header that cannot be written, or a segment whose header cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    /// A header line is missing or out of order; carries the key expected.
    #[error("missing `; {0}:` header line")]
    Missing(&'static str),
    /// The segment was written in a format version this crate cannot read.
    #[error("unsupported segment version {0:?}")]
    UnsupportedVersion(String),
    /// `imported` is not a whole number of seconds.
    #[error("`imported` is not unix seconds: {0:?}")]
    BadImported(String),
    /// A `source` or `layer` value that is empty, has surrounding
    /// whitespace, or spans lines, and so would not read back as written.
    #[error("header `{0}` must be one non-empty line with no surrounding whitespace")]
    BadValue(&'static str),
}

/// A batch that cannot be written as a segment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The header would not read back as written.
    #[error("bad header: {0}")]
    Header(#[from] HeaderError),
    /// A transaction has no `; id:` metadata, so a later import of the same
    /// record could not replace it.
    #[error("transaction {} has no `; id:`", .0.0)]
    MissingId(TxnId),
}

/// A segment read back: its header, its journal, and anything in the
/// journal that did not parse.
#[derive(Debug)]
pub struct Segment {
    /// The hash of the bytes it was read from.
    pub hash: SegmentHash,
    /// The header.
    pub header: SegmentHeader,
    /// The batch, with the header's comment lines removed from its file
    /// comments.
    pub journal: Journal,
    /// Parse errors in the journal. A segment this crate wrote has none; one
    /// written elsewhere might, and its bad transactions are left out as
    /// [`Journal::parse`] always does.
    pub errors: Vec<ParseError>,
}

impl Segment {
    /// The bytes of the segment holding `journal` under `header`.
    ///
    /// Every transaction must carry an `; id:`. Amounts are written at the
    /// precision of `journal`'s own commodity table, so build each batch in
    /// a journal of its own: a finer amount of the same commodity elsewhere
    /// in a shared journal would add digits here and change the hash.
    pub fn encode(header: &SegmentHeader, journal: &Journal) -> Result<String, EncodeError> {
        check_value(SOURCE_KEY, &header.source)?;
        check_value(LAYER_KEY, &header.layer)?;
        if let Some(at) = journal.txns.iter().position(|txn| txn.id.is_none()) {
            return Err(EncodeError::MissingId(TxnId(at as u32)));
        }

        let mut out = format!(
            "; {VERSION_KEY}: {SEGMENT_VERSION}\n\
             ; {SOURCE_KEY}: {}\n\
             ; {IMPORTED_KEY}: {}\n\
             ; {LAYER_KEY}: {}\n",
            header.source, header.imported, header.layer
        );
        let body = journal.to_ledger(WriteOptions::default());
        if !body.is_empty() {
            out.push('\n');
            out.push_str(&body);
        }
        Ok(out)
    }

    /// Read a segment's text. `hash` is not checked against `text`; that is
    /// the store's job, which knows the name the bytes were filed under.
    #[profiling::function]
    pub fn parse(hash: SegmentHash, text: &str) -> Result<Segment, HeaderError> {
        let crate::Parsed {
            mut journal,
            errors,
        } = Journal::parse(text);
        let header = take_header(&mut journal)?;
        Ok(Segment {
            hash,
            header,
            journal,
            errors,
        })
    }
}

/// Fail unless `value` reads back unchanged as a header value.
fn check_value(key: &'static str, value: &str) -> Result<(), HeaderError> {
    let one_line = !value.contains(['\n', '\r']);
    if value.is_empty() || value.trim() != value || !one_line {
        return Err(HeaderError::BadValue(key));
    }
    Ok(())
}

/// Read the four header lines off the front of `journal`'s file comments and
/// remove them.
fn take_header(journal: &mut Journal) -> Result<SegmentHeader, HeaderError> {
    let keys = [VERSION_KEY, SOURCE_KEY, IMPORTED_KEY, LAYER_KEY];
    let mut values: [&str; 4] = [""; 4];
    for (at, key) in keys.iter().enumerate() {
        let comment = journal
            .file_comments
            .get(at)
            .filter(|comment| comment.txns_before == 0)
            .ok_or(HeaderError::Missing(key))?;
        let text = journal.strings.get(comment.text);
        let value = text
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or(HeaderError::Missing(key))?;
        values[at] = value.trim();
    }
    let [version, source, imported, layer] = values;

    if version != SEGMENT_VERSION.to_string() {
        return Err(HeaderError::UnsupportedVersion(version.to_owned()));
    }
    let imported = imported
        .parse()
        .map_err(|_| HeaderError::BadImported(imported.to_owned()))?;
    check_value(SOURCE_KEY, source)?;
    check_value(LAYER_KEY, layer)?;
    let header = SegmentHeader {
        source: source.to_owned(),
        imported,
        layer: layer.to_owned(),
    };
    journal.file_comments.drain(..keys.len());
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_hex_round_trips() {
        let hash = SegmentHash::of(b"hello");
        let text = hash.to_string();
        assert_eq!(
            text,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(text.parse::<SegmentHash>(), Ok(hash));
        assert!(text.to_uppercase().parse::<SegmentHash>().is_err());
        assert!(text[1..].parse::<SegmentHash>().is_err());
        assert!(format!("{text}0").parse::<SegmentHash>().is_err());
    }

    #[test]
    fn header_values_must_be_one_trimmed_line() {
        assert!(check_value("source", "amex").is_ok());
        assert!(check_value("source", "amex card").is_ok());
        assert!(check_value("source", "").is_err());
        assert!(check_value("source", " amex").is_err());
        assert!(check_value("source", "a\nb").is_err());
    }
}
