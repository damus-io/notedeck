//! Turn a byte stream that arrives in arbitrary pieces back into the
//! protocol's lines.
//!
//! The wire is one JSON message per `\n`-terminated line, but nothing that
//! carries it promises a chunk is a line: microsandbox's
//! `ExecEvent::Stdout(Bytes)` hands over whatever the guest's pipe held, a
//! child's stdout `read` returns what it returns. A chunk may hold half a
//! line, three and a half, or one byte. Treating each chunk as a message
//! works for small messages and silently corrupts the large ones (a 512 KB
//! Read, a base64 image), which is the failure nobody sees in review.
//!
//! ```text
//!  chunks:  {"type":"res│ult","id":1,"text":"a"}\n{"ty│pe":"cancelled","id":2}\n{"type
//!  lines:   {"type":"result","id":1,"text":"a"}   {"type":"cancelled","id":2}   (held)
//! ```
//!
//! [`LineReassembler`] holds the unterminated tail between chunks and yields
//! each line once its `\n` arrives. A line longer than its limit is refused
//! rather than buffered without end, and the reassembler skips to the next
//! `\n` and carries on, so one bad line costs that line and not the session.

/// The longest line [`LineReassembler::new`] accepts by default: 64 MiB.
///
/// A Read answers at most 2000 lines of at most 2000 characters, about
/// 4 MB before JSON escaping can double it, and Bash keeps 30 KB of output,
/// so a well-formed message stays far below this. It exists to bound memory
/// when the other end misbehaves, not to shape real traffic.
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// How much of a refused line [`Line::TooLong`] keeps: enough to read the
/// message's `type` and `id`, which serde writes first.
pub const TOO_LONG_PREFIX: usize = 128;

/// One thing the reassembler found in the stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// A whole line, without its `\n` (or `\r\n`). May be empty.
    Complete(&'a [u8]),
    /// A line that ran past the limit and was dropped. `prefix` is its first
    /// [`TOO_LONG_PREFIX`] bytes, so the reader can still say which call it
    /// belonged to; `len` is how long it had got when it was refused.
    TooLong { prefix: &'a [u8], len: usize },
}

/// Buffers a byte stream and splits it on `\n`. See the module docs.
#[derive(Debug)]
pub struct LineReassembler {
    /// The current line's bytes so far: no `\n` in here.
    tail: Vec<u8>,
    /// The longest line accepted.
    max: usize,
    /// Set once the current line has been refused as too long: the rest of
    /// it, up to the next `\n`, is dropped, and `tail` holds its prefix.
    skipping: Option<usize>,
}

impl Default for LineReassembler {
    fn default() -> Self {
        Self::new(MAX_LINE)
    }
}

impl LineReassembler {
    /// A reassembler that refuses lines longer than `max` bytes.
    pub fn new(max: usize) -> Self {
        Self {
            tail: Vec::new(),
            max,
            skipping: None,
        }
    }

    /// Feed `chunk`, calling `on_line` for each line it completes, in order.
    /// Whatever follows the chunk's last `\n` is held for the next call.
    pub fn push(&mut self, mut chunk: &[u8], mut on_line: impl FnMut(Line<'_>)) {
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|&b| b == b'\n');
            let (piece, rest) = match newline {
                Some(at) => (&chunk[..at], Some(&chunk[at + 1..])),
                None => (chunk, None),
            };
            self.extend(piece);
            let Some(rest) = rest else {
                return;
            };
            self.end_line(&mut on_line);
            chunk = rest;
        }
    }

    /// Bytes held that no `\n` has ended yet: what a stream that closed now
    /// would leave unread. Zero between whole lines.
    pub fn pending(&self) -> usize {
        self.skipping.unwrap_or(self.tail.len())
    }

    /// Add part of the current line, refusing it once it is too long.
    fn extend(&mut self, piece: &[u8]) {
        if let Some(len) = self.skipping.as_mut() {
            *len += piece.len();
            return;
        }
        if self.tail.len() + piece.len() <= self.max {
            self.tail.extend_from_slice(piece);
            return;
        }
        let len = self.tail.len() + piece.len();
        let keep = TOO_LONG_PREFIX.min(self.max);
        if self.tail.len() < keep {
            let more = (keep - self.tail.len()).min(piece.len());
            self.tail.extend_from_slice(&piece[..more]);
        }
        self.tail.truncate(keep);
        self.tail.shrink_to(keep);
        self.skipping = Some(len);
    }

    /// The current line has met its `\n`: hand it over and start the next.
    fn end_line(&mut self, on_line: &mut impl FnMut(Line<'_>)) {
        match self.skipping.take() {
            Some(len) => on_line(Line::TooLong {
                prefix: &self.tail,
                len,
            }),
            None => {
                let line = self.tail.strip_suffix(b"\r").unwrap_or(&self.tail);
                on_line(Line::Complete(line));
            }
        }
        self.tail.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the reassembler made of `chunks`, each line as an owned string
    /// or `TooLong(len)`, plus what it still holds.
    fn feed(r: &mut LineReassembler, chunks: &[&[u8]]) -> Vec<String> {
        let mut out = Vec::new();
        for chunk in chunks {
            r.push(chunk, |line| {
                out.push(match line {
                    Line::Complete(l) => String::from_utf8(l.to_vec()).unwrap(),
                    Line::TooLong { len, .. } => format!("TooLong({len})"),
                })
            });
        }
        out
    }

    #[test]
    fn whole_lines_in_one_chunk() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"a\nbb\nccc\n"]), ["a", "bb", "ccc"]);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn a_line_split_across_chunks() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"hel", b"lo wo", b"rld\n"]), ["hello world"]);
    }

    /// An unterminated tail is held, not yielded, until its newline comes.
    #[test]
    fn a_partial_tail_is_withheld() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"one\ntw"]), ["one"]);
        assert_eq!(r.pending(), 2);
        assert_eq!(feed(&mut r, &[b"o\n"]), ["two"]);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn two_hundred_lines_in_one_blob() {
        let blob: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut r = LineReassembler::default();
        let lines = feed(&mut r, &[blob.as_bytes()]);
        assert_eq!(lines.len(), 200);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[199], "line 199");
    }

    #[test]
    fn one_byte_at_a_time() {
        let text = b"{\"a\":1}\n{\"b\":2}\n";
        let chunks: Vec<&[u8]> = text.chunks(1).collect();
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &chunks), ["{\"a\":1}", "{\"b\":2}"]);
    }

    /// A 512 KB line delivered in 8 KB chunks comes back byte for byte.
    #[test]
    fn a_large_line_in_8k_chunks() {
        let big: Vec<u8> = (0..512 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
        let mut wire = big.clone();
        wire.push(b'\n');
        let mut r = LineReassembler::default();
        let mut got = Vec::new();
        for chunk in wire.chunks(8192) {
            r.push(chunk, |line| match line {
                Line::Complete(l) => got.push(l.to_vec()),
                Line::TooLong { .. } => panic!("refused a line under the limit"),
            });
        }
        assert_eq!(got.len(), 1);
        assert!(got[0] == big, "the line came back changed");
    }

    #[test]
    fn empty_lines_are_lines() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"\n\na\n\n"]), ["", "", "a", ""]);
    }

    #[test]
    fn crlf_loses_its_cr() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"a\r\nb\r", b"\n"]), ["a", "b"]);
    }

    #[test]
    fn a_bare_newline_chunk_ends_the_held_line() {
        let mut r = LineReassembler::default();
        assert_eq!(feed(&mut r, &[b"abc", b"\n", b"\n"]), ["abc", ""]);
    }

    /// A line over the limit is refused, not buffered: memory stays at the
    /// prefix, and the line after it reads normally.
    #[test]
    fn an_overlong_line_is_refused_and_the_stream_recovers() {
        let mut r = LineReassembler::new(256);
        let long = vec![b'x'; 1000];
        let lines = feed(&mut r, &[b"ok\n", &long[..400], &long[400..], b"\nnext\n"]);
        assert_eq!(lines, ["ok", "TooLong(1000)", "next"]);
        assert_eq!(r.pending(), 0);
        assert!(r.tail.capacity() <= 256, "held {} bytes", r.tail.capacity());
    }

    /// The refused line's prefix still names the call it belonged to.
    #[test]
    fn a_refused_line_keeps_its_prefix() {
        let mut r = LineReassembler::new(200);
        let mut line = br#"{"type":"result","id":42,"text":""#.to_vec();
        line.extend(std::iter::repeat_n(b'y', 500));
        line.extend(b"\"}\n");
        let mut prefix = Vec::new();
        r.push(&line, |l| {
            if let Line::TooLong { prefix: p, .. } = l {
                prefix = p.to_vec();
            }
        });
        assert_eq!(prefix.len(), TOO_LONG_PREFIX);
        assert!(prefix.starts_with(br#"{"type":"result","id":42,"#));
    }

    /// JSON cut mid-token (inside a string, inside an escape, inside a
    /// multi-byte character) parses once whole.
    #[test]
    fn json_split_mid_token() {
        let message = serde_json::json!({"type": "result", "id": 7, "text": "é \"quoted\"\n☃"});
        let mut wire = serde_json::to_vec(&message).unwrap();
        wire.push(b'\n');
        for cut in 1..wire.len() {
            let mut r = LineReassembler::default();
            let mut parsed = Vec::new();
            for chunk in [&wire[..cut], &wire[cut..]] {
                r.push(chunk, |line| {
                    let Line::Complete(l) = line else {
                        panic!("too long")
                    };
                    parsed.push(serde_json::from_slice::<serde_json::Value>(l).unwrap());
                });
            }
            assert_eq!(parsed, std::slice::from_ref(&message), "cut at {cut}");
        }
    }
}
