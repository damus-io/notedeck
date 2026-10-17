//! What the CSV importers (Amex, RBC) share: finding a bank export's files
//! and columns, reading its amounts, and giving rows without a native id
//! their record ids.
//!
//! **Occurrence ids.** A bank export rarely has a reference column, so a
//! row's id is a stem the importer builds from what it read (`<source>:
//! <date>:<cents>:<key>`, say) plus `:<n>`, `n` counting the rows of one
//! export that share the stem, from 1. Five identical rides on one day are
//! five real charges with five ids, while the same five in the next,
//! overlapping export get the same five ids again. So for each stem the
//! journal keeps the most rows any one export had, never the sum.
//! [`number`] gives the ids after sorting each export's rows, so they don't
//! depend on the order the export lists identical rows in.

use std::path::{Path, PathBuf};

use super::ImportError;

/// Why an export could not be read, at `row` (from 1, the header being 1).
#[derive(Debug)]
pub(super) struct RowError {
    pub row: usize,
    pub message: String,
}

impl RowError {
    /// The error as an [`ImportError`] naming the file it was in.
    pub fn in_file(self, path: &Path) -> ImportError {
        ImportError::Csv {
            path: path.to_owned(),
            row: self.row,
            message: self.message,
        }
    }
}

/// An export's header row, for finding columns by name.
pub(super) struct Header {
    names: Vec<String>,
    /// The columns the importer reads, for the error when one is missing.
    expected: &'static str,
}

impl Header {
    /// The header `names`, from an export whose importer reads the
    /// `expected` columns (`"Date, Description, Amount"`).
    pub fn new(names: Vec<String>, expected: &'static str) -> Self {
        Self { names, expected }
    }

    /// Where the column called `name` is, ignoring case and surrounding
    /// blanks.
    pub fn at(&self, name: &str) -> Option<usize> {
        self.names
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
    }

    /// Where the column called `name` is, or an error quoting the header the
    /// export actually has, so an export in a format nobody has seen fails
    /// loudly rather than being read by guesswork.
    pub fn need(&self, name: &str) -> Result<usize, RowError> {
        self.at(name).ok_or_else(|| RowError {
            row: 1,
            message: format!(
                "no {name:?} column; the header is {:?}, expected {}",
                self.names.join(","),
                self.expected
            ),
        })
    }
}

/// A row and the record id [`number`] gave it.
pub(super) struct Identified<R> {
    pub id: String,
    pub row: R,
}

/// Give each row of one export its occurrence id (see the module docs):
/// `stem(row)` plus `:<n>`.
///
/// `R`'s order must sort by the stem's fields first, so rows sharing a stem
/// end up next to each other; whatever it sorts by after them only decides
/// which of a run of same-stem rows gets which number.
pub(super) fn number<R: Ord>(mut rows: Vec<R>, stem: impl Fn(&R) -> String) -> Vec<Identified<R>> {
    rows.sort();
    let mut out = Vec::with_capacity(rows.len());
    let mut prev = String::new();
    let mut nth = 0u32;
    for row in rows {
        let next = stem(&row);
        nth = if next == prev { nth + 1 } else { 1 };
        out.push(Identified {
            id: format!("{next}:{nth}"),
            row,
        });
        prev = next;
    }
    out
}

/// The CSV files `paths` name: a file as it is, a directory as its `*.csv`
/// files in name order.
pub(super) fn csv_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, ImportError> {
    let mut files = Vec::new();
    for path in paths {
        if !path.is_dir() {
            files.push(path.clone());
            continue;
        }
        let read_err = |source| ImportError::Read {
            path: path.clone(),
            source,
        };
        let mut found = Vec::new();
        for entry in std::fs::read_dir(path).map_err(read_err)? {
            let file = entry.map_err(read_err)?.path();
            if is_csv(&file) {
                found.push(file);
            }
        }
        found.sort();
        files.extend(found);
    }
    Ok(files)
}

/// Whether `path` is a file ending in `.csv`, in any case.
fn is_csv(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
}

/// Read `path` as text.
pub(super) fn read(path: &Path) -> Result<String, ImportError> {
    std::fs::read_to_string(path).map_err(|source| ImportError::Read {
        path: path.to_owned(),
        source,
    })
}

/// `text` trimmed, with each run of whitespace one space.
pub(super) fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text as it goes in a record id: lower case, each run of characters other
/// than letters and digits one `-`, none at either end. `"STORE*Order 7"` is
/// `store-order-7`.
pub(super) fn id_key(text: &str) -> String {
    let mut key = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            key.extend(c.to_lowercase());
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    if key.ends_with('-') {
        key.pop();
    }
    key
}

/// `"-8,803.95"` to `-880395`: cents keep the arithmetic exact.
pub(super) fn parse_cents(text: &str) -> Option<i64> {
    let text: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(&text)),
    };
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty()) || frac.len() > 2 {
        return None;
    }
    if !all_digits(whole) || !all_digits(frac) {
        return None;
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let frac: i64 = format!("{frac:0<2}").parse().ok()?;
    let cents = whole.checked_mul(100)?.checked_add(frac)?;
    Some(if negative { -cents } else { cents })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(parse_cents("123.16"), Some(12316));
        assert_eq!(parse_cents("-8,803.95"), Some(-880395));
        assert_eq!(parse_cents("+170.0"), Some(17000));
        assert_eq!(parse_cents("5"), Some(500));
        assert_eq!(parse_cents(".5"), Some(50));
        for bad in ["", "-", "abc", "1.234", "1.-5", "1.2.3"] {
            assert_eq!(parse_cents(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn id_keys() {
        assert_eq!(id_key("CORNER BISTRO"), "corner-bistro");
        assert_eq!(id_key("SHOP*Order 7"), "shop-order-7");
        assert_eq!(id_key("  --A  B--  "), "a-b");
        assert_eq!(id_key("CAFÉ"), "café");
    }

    #[test]
    fn numbering_counts_rows_that_share_a_stem() {
        let rows = vec![(2, "b"), (1, "a"), (2, "b"), (2, "c")];
        let ids: Vec<String> = number(rows, |(n, _)| format!("x:{n}"))
            .into_iter()
            .map(|Identified { id, .. }| id)
            .collect();
        assert_eq!(ids, ["x:1:1", "x:2:1", "x:2:2", "x:2:3"]);
    }

    #[test]
    fn a_missing_column_quotes_the_header() {
        let header = Header::new(vec!["Date".into(), " amount ".into()], "Date, Amount");
        assert_eq!(header.at("AMOUNT"), Some(1));
        let err = header.need("Description").unwrap_err();
        assert_eq!(err.row, 1);
        assert_eq!(
            err.message,
            "no \"Description\" column; the header is \"Date, amount \", expected Date, Amount"
        );
    }

    #[test]
    fn a_directory_reads_its_csv_files_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("exports");
        std::fs::create_dir(&dir).unwrap();
        for name in ["b.CSV", "a.csv", "notes.txt"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        std::fs::create_dir(dir.join("old.csv")).unwrap();
        let file = tmp.path().join("one.csv");
        assert_eq!(
            csv_files(&[dir.clone(), file.clone()]).unwrap(),
            [dir.join("a.csv"), dir.join("b.CSV"), file]
        );
    }
}
