//! A minimal RFC 4180 CSV reader, shared by the CSV importers (Amex, and
//! the RBC and App Store ones after it). Ported from amex2ledger.
//!
//! It handles a UTF-8 BOM, quoted fields holding commas or newlines, `""`
//! escapes, CRLF line ends, blank lines and trailing empty fields. A bank
//! export is simple, but a description can be quoted when it holds a comma,
//! e.g. `"FERRY - TERMINAL, NORTH DOCK"`.
//!
//! Nothing is buffered and nothing is copied: [`Reader`] iterates records, each
//! [`Record`] iterates its own fields, and a field borrows straight out of the
//! input. The one exception is a field containing a `""` escape, which has to be
//! rebuilt to drop the doubled quote.
//!
//! ```ignore
//! for mut record in Reader::new(text) {
//!     let Some(first) = record.next() else { continue };
//!     ...
//! }
//! ```
//!
//! A [`Record`] holds its own slice of the input rather than borrowing the
//! [`Reader`], so the two nest without any lending-iterator trouble and a record
//! left half-read costs nothing.

use std::borrow::Cow;

/// Iterates the records of a CSV text.
pub struct Reader<'a> {
    /// The unparsed remainder of the input.
    rest: &'a str,
}

impl<'a> Reader<'a> {
    /// A reader over `input`, past a leading UTF-8 BOM, which Amex sometimes
    /// emits.
    pub fn new(input: &'a str) -> Reader<'a> {
        Reader {
            rest: input.strip_prefix('\u{feff}').unwrap_or(input),
        }
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Record<'a>;

    /// Yields the next record, skipping blank lines.
    fn next(&mut self) -> Option<Record<'a>> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            let line = self.split_line();
            if !line.trim().is_empty() {
                return Some(Record {
                    rest: line,
                    done: false,
                });
            }
        }
    }
}

impl<'a> Reader<'a> {
    /// Takes everything up to the next newline that is not inside quotes, so a
    /// quoted field may legally contain one.
    fn split_line(&mut self) -> &'a str {
        // Every character we look for is ASCII, so a byte index into UTF-8 text
        // always lands on a character boundary.
        let bytes = self.rest.as_bytes();
        let mut quoted = false;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'"' => quoted = !quoted,
                b'\n' if !quoted => {
                    let line = &self.rest[..i];
                    self.rest = &self.rest[i + 1..];
                    return line.strip_suffix('\r').unwrap_or(line);
                }
                _ => {}
            }
        }
        std::mem::take(&mut self.rest)
    }
}

/// One record, which iterates its own fields.
pub struct Record<'a> {
    /// The fields not yet yielded.
    rest: &'a str,
    /// Set once a field is yielded with no comma after it. Without this an
    /// exhausted record would be indistinguishable from a trailing empty field,
    /// so `a,b,` would yield two fields instead of three.
    done: bool,
}

impl<'a> Iterator for Record<'a> {
    type Item = Cow<'a, str>;

    fn next(&mut self) -> Option<Cow<'a, str>> {
        if self.done {
            return None;
        }
        let field = match self.rest.strip_prefix('"') {
            Some(rest) => {
                self.rest = rest;
                self.read_quoted()
            }
            None => self.read_plain(),
        };
        self.done = !field.more;
        Some(field.text)
    }
}

/// One field read off a [`Record`].
struct Field<'a> {
    text: Cow<'a, str>,
    /// Whether a comma followed it, so another field comes after.
    more: bool,
}

impl<'a> Record<'a> {
    /// Reads a field and its trailing comma, reporting whether there was one.
    fn read_plain(&mut self) -> Field<'a> {
        match self.rest.find(',') {
            Some(i) => {
                let text = &self.rest[..i];
                self.rest = &self.rest[i + 1..];
                Field {
                    text: Cow::Borrowed(text),
                    more: true,
                }
            }
            None => Field {
                text: Cow::Borrowed(std::mem::take(&mut self.rest)),
                more: false,
            },
        }
    }

    /// The opening quote is already consumed. The field borrows unless it holds
    /// a `""` escape, in which case the runs either side are stitched together.
    fn read_quoted(&mut self) -> Field<'a> {
        let mut owned: Option<String> = None;
        let mut start = 0;
        let mut scan = 0;
        loop {
            let Some(offset) = self.rest[scan..].find('"') else {
                // Unterminated quote: take what's left rather than lose it.
                let tail = &self.rest[start..];
                self.rest = "";
                return Field {
                    text: join(owned, tail),
                    more: false,
                };
            };
            let quote = scan + offset;
            if self.rest[quote + 1..].starts_with('"') {
                // Keep the first quote of the pair, skip the second.
                owned
                    .get_or_insert_with(String::new)
                    .push_str(&self.rest[start..=quote]);
                scan = quote + 2;
                start = scan;
                continue;
            }
            let text = join(owned, &self.rest[start..quote]);
            // Skip past the closing quote to the comma, if any.
            self.rest = &self.rest[quote + 1..];
            let more = match self.rest.find(',') {
                Some(i) => {
                    self.rest = &self.rest[i + 1..];
                    true
                }
                None => {
                    self.rest = "";
                    false
                }
            };
            return Field { text, more };
        }
    }
}

/// Append `tail` to a partially unescaped field, or borrow it if there was no
/// escape to unescape.
fn join(owned: Option<String>, tail: &str) -> Cow<'_, str> {
    match owned {
        Some(mut s) => {
            s.push_str(tail);
            Cow::Owned(s)
        }
        None => Cow::Borrowed(tail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect every field, for tests that only care about the text.
    fn read_all(input: &str) -> Vec<Vec<String>> {
        Reader::new(input)
            .map(|r| r.map(|f| f.to_string()).collect())
            .collect()
    }

    #[test]
    fn plain_rows() {
        assert_eq!(read_all("a,b\n1,2\n"), vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn quoted_field_with_comma() {
        let got = read_all("14 Aug 2026,\"FERRY - TERMINAL, NORTH DOCK\",29.00\n");
        assert_eq!(
            got[0],
            vec!["14 Aug 2026", "FERRY - TERMINAL, NORTH DOCK", "29.00"]
        );
    }

    #[test]
    fn doubled_quotes_and_crlf() {
        assert_eq!(
            read_all("\"say \"\"hi\"\"\",2\r\n"),
            vec![vec!["say \"hi\"", "2"]]
        );
    }

    #[test]
    fn bom_and_missing_trailing_newline() {
        assert_eq!(
            read_all("\u{feff}a,b\n1,2"),
            vec![vec!["a", "b"], vec!["1", "2"]]
        );
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert_eq!(
            read_all("a,b\n\n\n1,2\n\n"),
            vec![vec!["a", "b"], vec!["1", "2"]]
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(read_all("").is_empty());
        assert!(read_all("\n\n").is_empty());
    }

    #[test]
    fn a_trailing_comma_is_a_trailing_empty_field() {
        // The Amex "Flexible" column is empty on payment rows.
        assert_eq!(read_all("a,b,\n"), vec![vec!["a", "b", ""]]);
        assert_eq!(read_all(",\n"), vec![vec!["", ""]]);
    }

    #[test]
    fn a_quoted_field_may_contain_a_newline() {
        assert_eq!(
            read_all("a,\"two\nlines\"\nb,c\n"),
            vec![vec!["a", "two\nlines"], vec!["b", "c"],]
        );
    }

    /// The point of the exercise: field text is the input, not a copy of it.
    #[test]
    fn fields_borrow_from_the_input() {
        let input = "15 Apr 2026,\"CORNER BISTRO, Springfield\",123.16\n";
        let fields: Vec<Cow<str>> = Reader::new(input).next().unwrap().collect();

        for field in &fields {
            assert!(matches!(field, Cow::Borrowed(_)), "copied {field:?}");
        }
        // Borrowed is not enough on its own - check it points into `input`.
        let range = input.as_bytes().as_ptr_range();
        assert!(fields.iter().all(|f| range.contains(&f.as_ptr())));
    }

    /// A `""` escape is the one case that has to allocate.
    #[test]
    fn only_an_escape_forces_a_copy() {
        let fields: Vec<Cow<str>> = Reader::new("\"plain\",\"esc\"\"aped\"\n")
            .next()
            .unwrap()
            .collect();
        assert!(matches!(fields[0], Cow::Borrowed(_)));
        assert_eq!(fields[1], Cow::Owned::<str>("esc\"aped".into()));
    }

    /// Abandoning a record part-read must not disturb the next one.
    #[test]
    fn a_half_read_record_is_free_to_drop() {
        let mut reader = Reader::new("a,b,c\n1,2,3\n");
        assert_eq!(reader.next().unwrap().next().unwrap(), "a");
        assert_eq!(
            reader.next().unwrap().collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn an_unterminated_quote_keeps_the_tail() {
        assert_eq!(read_all("a,\"runaway"), vec![vec!["a", "runaway"]]);
    }
}
