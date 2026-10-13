//! Free-form tool output (Bash stdout/stderr, WebSearch results) together with
//! where its lines and head+tail preview lie, measured once when the output is
//! captured.
//!
//! A chat row draws a long output as its first few lines, a "N lines hidden"
//! gap, and its last few lines, and can open the whole output in a scrolling
//! view that draws only the lines in sight. Both need to know where lines
//! start, which means scanning the whole output. A UI that redraws every frame
//! must not repeat that scan, so [`ToolOutput::new`] does it once and the UI
//! slices the stored text.

use std::ops::Range;

/// How many leading lines a clamped preview keeps.
pub const PREVIEW_HEAD_LINES: usize = 4;

/// How many trailing lines a clamped preview keeps.
pub const PREVIEW_TAIL_LINES: usize = 4;

/// The most bytes the head, and separately the tail, of a preview hold. A
/// few lines can still be huge (minified JSON is one line), and the preview
/// is redrawn every frame, so it is capped by size as well as by line.
pub const PREVIEW_MAX_BYTES: usize = 2048;

/// A tool's raw output plus where its lines and preview lie.
///
/// Derefs to the full text, so readers that want a `&str` keep using
/// `output.as_deref()` on an `Option<ToolOutput>`.
///
/// One pointer wide: it rides in [`ExecutedTool`](crate::messages::ExecutedTool)
/// inside every `ToolResponses`, so the text and its index sit behind a box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput(Box<Indexed>);

/// The text and line index behind a [`ToolOutput`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Indexed {
    text: String,
    /// Byte offset where each line starts. Trailing newlines end the output
    /// rather than start empty lines, so `"a\nb\n"` has two.
    line_starts: Vec<usize>,
    /// Length of the text without its trailing newlines.
    body_end: usize,
}

/// The head and tail a long output is cut down to, and what lies between.
/// Derived from the stored line starts in constant time, not by a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewClamp {
    /// The first [`PREVIEW_HEAD_LINES`] lines, without the newline after
    /// them, and at most [`PREVIEW_MAX_BYTES`].
    pub head: Range<usize>,
    /// The last [`PREVIEW_TAIL_LINES`] lines, at most [`PREVIEW_MAX_BYTES`].
    pub tail: Range<usize>,
    /// Lines between the head and the tail.
    pub hidden_lines: usize,
}

impl ToolOutput {
    /// Wrap `text`, finding its lines and preview cut once.
    pub fn new(text: String) -> Self {
        let body_end = text.trim_end_matches(['\n', '\r']).len();
        let body = &text[..body_end];
        let line_starts: Vec<usize> = if body.is_empty() {
            Vec::new()
        } else {
            std::iter::once(0)
                .chain(
                    body.bytes()
                        .enumerate()
                        .filter(|&(_, b)| b == b'\n')
                        .map(|(i, _)| i + 1),
                )
                .collect()
        };
        Self(Box::new(Indexed {
            text,
            line_starts,
            body_end,
        }))
    }

    /// The full output text.
    pub fn as_str(&self) -> &str {
        &self.0.text
    }

    /// Number of lines in the output.
    pub fn line_count(&self) -> usize {
        self.0.line_starts.len()
    }

    /// Line `i`, without its newline.
    pub fn line(&self, i: usize) -> &str {
        let start = self.0.line_starts[i];
        let end = self
            .0
            .line_starts
            .get(i + 1)
            .map_or(self.0.body_end, |next| next - 1);
        self.0.text[start..end].trim_end_matches('\r')
    }

    /// The output without its trailing newlines.
    pub fn body(&self) -> &str {
        &self.0.text[..self.0.body_end]
    }

    /// The preview cut, when the output is too long to show whole. `None`
    /// means it fits the preview and is shown as is.
    pub fn clamp(&self) -> Option<PreviewClamp> {
        measure_clamp(self.body(), &self.0.line_starts)
    }

    /// The preview's head slice, when the output is clamped.
    pub fn head(&self) -> Option<&str> {
        Some(&self.0.text[self.clamp()?.head])
    }

    /// The preview's tail slice, when the output is clamped.
    pub fn tail(&self) -> Option<&str> {
        Some(&self.0.text[self.clamp()?.tail])
    }

    /// Lines the clamped preview leaves out; 0 when the output is shown whole.
    pub fn hidden_lines(&self) -> usize {
        self.clamp().map_or(0, |c| c.hidden_lines)
    }
}

/// Find the head and tail of `body` when it has too many lines to show whole.
///
/// The clamp only applies when it hides at least two lines: a gap marker
/// standing in for a single line would take the room the line does.
fn measure_clamp(body: &str, line_starts: &[usize]) -> Option<PreviewClamp> {
    let line_count = line_starts.len();
    if line_count <= PREVIEW_HEAD_LINES + PREVIEW_TAIL_LINES + 1 {
        return None;
    }
    // The newline ending the head's last line sits just before the next start.
    let head_end = (line_starts[PREVIEW_HEAD_LINES] - 1).min(PREVIEW_MAX_BYTES);
    let tail_start = line_starts[line_count - PREVIEW_TAIL_LINES]
        .max(body.len().saturating_sub(PREVIEW_MAX_BYTES));
    Some(PreviewClamp {
        head: 0..floor_char_boundary(body, head_end),
        tail: ceil_char_boundary(body, tail_start)..body.len(),
        hidden_lines: line_count - PREVIEW_HEAD_LINES - PREVIEW_TAIL_LINES,
    })
}

/// The largest char boundary in `s` at or below `i`.
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The smallest char boundary in `s` at or above `i`.
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::new(text.to_owned())
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn empty_output_has_no_lines() {
        let out = ToolOutput::new("\n\n".to_owned());
        assert_eq!(out.line_count(), 0);
        assert_eq!(out.body(), "");
        assert!(out.head().is_none());
    }

    #[test]
    fn trailing_newline_is_not_a_line() {
        assert_eq!(ToolOutput::new("a\nb\n".to_owned()).line_count(), 2);
        assert_eq!(ToolOutput::new("a\nb".to_owned()).line_count(), 2);
    }

    #[test]
    fn lines_index_without_newlines() {
        let out = ToolOutput::new("one\r\n\ntwo\n".to_owned());
        assert_eq!(out.line_count(), 3);
        assert_eq!(out.line(0), "one");
        assert_eq!(out.line(1), "");
        assert_eq!(out.line(2), "two");
    }

    #[test]
    fn short_output_is_shown_whole() {
        // Head + tail + 1: clamping would hide a single line, so it doesn't.
        let out = ToolOutput::new(numbered(PREVIEW_HEAD_LINES + PREVIEW_TAIL_LINES + 1));
        assert!(out.clamp().is_none());
        assert_eq!(out.hidden_lines(), 0);
        assert!(out.body().ends_with("line 9"));
    }

    #[test]
    fn long_output_keeps_head_and_tail() {
        let out = ToolOutput::new(numbered(523));
        assert_eq!(out.line_count(), 523);
        assert_eq!(out.head(), Some("line 1\nline 2\nline 3\nline 4"));
        assert_eq!(out.tail(), Some("line 520\nline 521\nline 522\nline 523"));
        assert_eq!(out.hidden_lines(), 515);
        assert_eq!(out.line(522), "line 523");
    }

    #[test]
    fn huge_lines_cap_the_preview_by_size() {
        let wide = "x".repeat(PREVIEW_MAX_BYTES * 3);
        let text: String = (0..12).map(|_| format!("{wide}\n")).collect();
        let out = ToolOutput::new(text);
        assert_eq!(out.head().unwrap().len(), PREVIEW_MAX_BYTES);
        assert_eq!(out.tail().unwrap().len(), PREVIEW_MAX_BYTES);
    }

    #[test]
    fn multibyte_lines_slice_on_char_boundaries() {
        let text: String = (0..20).map(|i| format!("🔥{i}🔥\n")).collect();
        let out = ToolOutput::new(text);
        assert_eq!(out.head().unwrap().lines().count(), PREVIEW_HEAD_LINES);
        assert_eq!(out.tail().unwrap().lines().last(), Some("🔥19🔥"));

        // A size cap landing mid-emoji rounds to a boundary instead of panicking.
        let wide = "🔥".repeat(PREVIEW_MAX_BYTES);
        let text: String = (0..12).map(|_| format!("{wide}\n")).collect();
        let out = ToolOutput::new(text);
        assert!(out.head().unwrap().len() <= PREVIEW_MAX_BYTES);
        assert!(out.tail().unwrap().len() <= PREVIEW_MAX_BYTES);
    }
}
