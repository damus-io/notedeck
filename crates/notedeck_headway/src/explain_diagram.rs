//! What explain's diagram fences (` ```flow `, ` ```sequence `) share:
//! reading a fence's options and its edge lines, measuring and wrapping
//! monospace text as explain does, and the [`Diagram`] a fence draws to,
//! a wide drawing and, where that won't fit a phone, a narrow one.

use crate::review_figures::Tone;

/// The usable width of a figure on a 390px phone: a wide layout no wider
/// needs no narrow one.
pub(crate) const PHONE: f64 = 320.0;

/// Every glyph of the diagram's monospace text advances this many ems, as
/// explain's `text_w` measures it.
pub(crate) const CHAR: f64 = 0.6;

/// How far a glyph of Inconsolata, the face the pane draws diagram text
/// in, advances, in ems.
const INCONSOLATA_ADVANCE: f64 = 0.5;

/// The size Inconsolata is drawn at for explain's `size`px diagram text:
/// one where its glyphs advance as far as the layout measured them
/// ([`CHAR`] ems), so a line fills its box as on the page. 12px of
/// explain's mono comes out 14.4px, at much the same x-height.
pub(crate) fn mono_size(size: f64) -> f64 {
    size * CHAR / INCONSOLATA_ADVANCE
}

/// The fill under an edge's label: the pane's own background, which the
/// pane substitutes for this before the drawing loads; explain's surface
/// otherwise.
pub(crate) const BACKDROP: &str = "var(--backdrop, var(--surface))";

/// A diagram fence, drawn: a flow or a sequence.
pub(crate) struct Diagram {
    /// The layout asked for, at its own wrap.
    pub wide: Drawing,
    /// A narrower one, when `wide` is wider than a phone.
    pub narrow: Option<Drawing>,
    /// Its accessible label: the fence's `label=`, else its edges or steps
    /// described.
    pub label: String,
}

impl Diagram {
    /// The drawing to show in `width` points: the wide one where it fits,
    /// else the narrow one (which may still need to scroll).
    pub(crate) fn fitting(&self, width: f32) -> &Drawing {
        match &self.narrow {
            Some(narrow) if self.wide.width > width => narrow,
            _ => &self.wide,
        }
    }
}

/// One layout of a diagram, as an SVG at 1:1.
pub(crate) struct Drawing {
    /// In explain's `var(--…)` tokens, with [`BACKDROP`] under a flow's
    /// labels.
    pub svg: String,
    pub width: f32,
    pub height: f32,
}

/// The value of `key=` in a fence's info string, quoted or bare, as
/// explain's `parse_attrs` reads it; a bare flag has none.
pub(crate) fn fence_attr<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    let bytes = info.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_attr_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_attr_byte(bytes[i]) {
            i += 1;
        }
        let name = &info[start..i];
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        let value = match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let open = i + 1;
                let Some(len) = info[open..].find(q as char) else {
                    // An unclosed quote: explain's regex matches the name alone.
                    continue;
                };
                i = open + len + 1;
                &info[open..open + len]
            }
            _ => {
                let open = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &info[open..i]
            }
        };
        if name == key && !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn is_attr_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// JavaScript's whitespace, which explain's regexes and `trim` use.
pub(crate) fn is_js_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// The length of the node id `s` starts with.
pub(crate) fn id_len(s: &str) -> usize {
    s.bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        .count()
}

/// An edge operator a diagram reads, and what it means there.
pub(crate) struct EdgeOp<K> {
    pub text: &'static str,
    pub kind: K,
}

/// An edge line, as read.
pub(crate) struct EdgeLine<'a, K> {
    pub from: &'a str,
    pub kind: K,
    pub to: &'a str,
    pub label: &'a str,
}

/// `line` read as an edge, `a -> b: label`, its operator one of `ops`
/// (tried in order), as explain's regex reads it: an id can hold `-` and
/// `.`, so a shorter one is tried when the longest leaves no operator after
/// it.
pub(crate) fn edge_line<'a, K: Copy>(line: &'a str, ops: &[EdgeOp<K>]) -> Option<EdgeLine<'a, K>> {
    let longest = id_len(line);
    for end in (1..=longest).rev() {
        let rest = line[end..].trim_start_matches(is_js_ws);
        for op in ops {
            let Some(after) = rest.strip_prefix(op.text) else {
                continue;
            };
            let after = after.trim_start_matches(is_js_ws);
            let to = id_len(after);
            if to == 0 {
                continue;
            }
            let tail = after[to..].trim_start_matches(is_js_ws);
            let label = if tail.is_empty() {
                ""
            } else if let Some(label) = tail.strip_prefix(':') {
                label.trim_start_matches(is_js_ws)
            } else {
                continue;
            };
            return Some(EdgeLine {
                from: &line[..end],
                kind: op.kind,
                to: &after[..to],
                label,
            });
        }
    }
    None
}

/// `s`'s width in a `size`px monospace face, as explain measures it.
pub(crate) fn text_w(s: &str, size: f64) -> f64 {
    s.chars().count() as f64 * size * CHAR
}

/// `s` wrapped greedily at spaces to `max` characters, as explain's `wrap`:
/// a word longer than a line is cut after an identifier boundary
/// (`fold_tool_` | `result`, `Data::` | `fold`) where one exists.
pub(crate) fn wrap(s: &str, max: f64) -> Vec<String> {
    const BREAK_AFTER: &[char] = &[':', '_', '.', '/', '-', '(', ',', '='];
    let mut out = Vec::new();
    let mut line: Vec<char> = Vec::new();
    for word in s.split(is_js_ws).filter(|w| !w.is_empty()) {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() as f64 > max {
            if !line.is_empty() {
                out.push(line.drain(..).collect());
            }
            let mut cut = max as usize;
            let mut i = max as usize;
            while i as f64 > max / 3.0 {
                let prev = word[i - 1];
                let next = word.get(i).copied();
                let camel =
                    prev.is_ascii_lowercase() && next.is_some_and(|c| c.is_ascii_uppercase());
                if BREAK_AFTER.contains(&prev) || camel {
                    cut = i;
                    break;
                }
                i -= 1;
            }
            // `max` under 1 still has to move on.
            let cut = cut.max(1);
            out.push(word[..cut].iter().collect());
            word.drain(..cut);
        }
        if line.is_empty() {
            line = word;
        } else if (line.len() + 1 + word.len()) as f64 <= max {
            line.push(' ');
            line.extend(word);
        } else {
            out.push(line.drain(..).collect());
            line = word;
        }
    }
    if !line.is_empty() {
        out.push(line.into_iter().collect());
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// The palette tokens a diagram's tone draws with, as explain's `.tone-*`:
/// its line, its soft fill and its ink.
pub(crate) struct DiagramTokens {
    pub line: &'static str,
    pub soft: &'static str,
    pub ink: &'static str,
}

pub(crate) fn diagram_tokens(tone: Tone) -> DiagramTokens {
    let (line, soft, ink) = match tone {
        Tone::Brass => ("brass", "brass-soft", "brass-ink"),
        Tone::Slate => ("slate", "slate-soft", "slate-ink"),
        Tone::Moss => ("moss", "moss-soft", "moss-ink"),
        Tone::Alert => ("alert", "alert-soft", "alert-ink"),
        Tone::Dim => ("rule", "surface", "ink-2"),
    };
    DiagramTokens { line, soft, ink }
}

/// `s` escaped for SVG text or an attribute.
pub(crate) fn esc(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fence's options, quoted or bare; a flag or an empty value is none.
    #[test]
    fn fence_attrs() {
        let info = "flow dir=right wrap=30 label=\"a b\" id='x y' wide empty=";
        assert_eq!(fence_attr(info, "dir"), Some("right"));
        assert_eq!(fence_attr(info, "wrap"), Some("30"));
        assert_eq!(fence_attr(info, "label"), Some("a b"));
        assert_eq!(fence_attr(info, "id"), Some("x y"));
        assert_eq!(fence_attr(info, "wide"), None);
        assert_eq!(fence_attr(info, "empty"), None);
        assert_eq!(fence_attr(info, "flow"), None);
    }

    /// Words wrap at spaces, and a long one is cut after an identifier
    /// boundary, as explain's `wrap`.
    #[test]
    fn wrap_cuts_at_identifier_boundaries() {
        assert_eq!(wrap("one two three", 8.0), ["one two", "three"]);
        assert_eq!(wrap("fold_tool_result", 12.0), ["fold_tool_", "result"]);
        assert_eq!(wrap("Data::foldEvents", 10.0), ["Data::fold", "Events"]);
        assert_eq!(wrap("abcdefghij", 4.0), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4.0), [""]);
    }
}
