//! Inline element parsing for bold, italic, code, links, etc.

use crate::element::{InlineElement, InlineStyle, Span};
use crate::partial::PartialKind;

/// Parses inline elements from text.
/// `base_offset` is the position of `text` within the parser's buffer.
/// All returned Spans are absolute buffer positions.
///
/// Note: This is called on complete paragraph text, not streaming.
/// For streaming, we use PartialKind to track incomplete markers.
pub fn parse_inline(text: &str, base_offset: usize) -> Vec<InlineElement> {
    let mut result = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut plain_start = 0;

    while let Some((i, c)) = chars.next() {
        match c {
            // Backtick - inline code
            '`' => {
                // Flush any pending plain text
                if i > plain_start {
                    result.push(InlineElement::Text(Span::new(
                        base_offset + plain_start,
                        base_offset + i,
                    )));
                }

                // Count backticks
                let mut backtick_count = 1;
                while chars.peek().map(|(_, c)| *c == '`').unwrap_or(false) {
                    chars.next();
                    backtick_count += 1;
                }

                let start_pos = i + backtick_count;

                // Find closing backticks (same count)
                if let Some(end_pos) = find_closing_backticks(&text[start_pos..], backtick_count) {
                    let code_start = start_pos;
                    let code_end = start_pos + end_pos;
                    let code_content = &text[code_start..code_end];
                    // Strip single leading/trailing space if present (CommonMark rule)
                    let (trim_start, trim_end) = if code_content.starts_with(' ')
                        && code_content.ends_with(' ')
                        && code_content.len() > 1
                    {
                        (code_start + 1, code_end - 1)
                    } else {
                        (code_start, code_end)
                    };
                    result.push(InlineElement::Code(Span::new(
                        base_offset + trim_start,
                        base_offset + trim_end,
                    )));

                    // Advance past closing backticks
                    let skip_to = start_pos + end_pos + backtick_count;
                    while chars.peek().map(|(idx, _)| *idx < skip_to).unwrap_or(false) {
                        chars.next();
                    }
                    plain_start = skip_to;
                } else {
                    // No closing - treat as plain text
                    plain_start = i;
                }
            }

            // Asterisk or underscore - potential bold/italic
            '*' | '_' => {
                let marker = c;
                let marker_start = i;

                // Count consecutive markers
                let mut count = 1;
                while chars.peek().map(|(_, ch)| *ch == marker).unwrap_or(false) {
                    chars.next();
                    count += 1;
                }

                // Limit to 3 for bold+italic
                let effective_count = count.min(3);

                // Check if this could be an opener (not preceded by whitespace at word boundary for _)
                let can_open = if marker == '_' {
                    // Underscore: check word boundary rules
                    i == 0
                        || text[..i]
                            .chars()
                            .last()
                            .map(|c| c.is_whitespace() || c.is_ascii_punctuation())
                            .unwrap_or(true)
                } else {
                    true // Asterisk can always open
                };

                if !can_open {
                    // Not a valid opener, treat as plain text
                    continue;
                }

                let content_start = marker_start + count;

                // Look for closing marker
                if let Some((content_end_local, close_len)) =
                    find_closing_emphasis(&text[content_start..], marker, effective_count)
                {
                    // Flush pending plain text
                    if marker_start > plain_start {
                        result.push(InlineElement::Text(Span::new(
                            base_offset + plain_start,
                            base_offset + marker_start,
                        )));
                    }

                    let style = match close_len {
                        1 => InlineStyle::Italic,
                        2 => InlineStyle::Bold,
                        _ => InlineStyle::BoldItalic,
                    };

                    result.push(InlineElement::Styled {
                        style,
                        content: Span::new(
                            base_offset + content_start,
                            base_offset + content_start + content_end_local,
                        ),
                    });

                    // Advance past the content and closing marker
                    let skip_to = content_start + content_end_local + close_len;
                    while chars.peek().map(|(idx, _)| *idx < skip_to).unwrap_or(false) {
                        chars.next();
                    }
                    plain_start = skip_to;
                }
                // If no closing found, leave as plain text (will be collected)
            }

            // Tilde - potential strikethrough
            '~' => {
                if chars.peek().map(|(_, c)| *c == '~').unwrap_or(false) {
                    chars.next(); // consume second ~

                    // Flush pending text
                    if i > plain_start {
                        result.push(InlineElement::Text(Span::new(
                            base_offset + plain_start,
                            base_offset + i,
                        )));
                    }

                    let content_start = i + 2;

                    // Find closing ~~
                    if let Some(end_pos) = text[content_start..].find("~~") {
                        result.push(InlineElement::Styled {
                            style: InlineStyle::Strikethrough,
                            content: Span::new(
                                base_offset + content_start,
                                base_offset + content_start + end_pos,
                            ),
                        });

                        let skip_to = content_start + end_pos + 2;
                        while chars.peek().map(|(idx, _)| *idx < skip_to).unwrap_or(false) {
                            chars.next();
                        }
                        plain_start = skip_to;
                    } else {
                        // No closing, revert
                        plain_start = i;
                    }
                }
            }

            // Square bracket - potential link or image
            '[' => {
                // Flush pending text
                if i > plain_start {
                    result.push(InlineElement::Text(Span::new(
                        base_offset + plain_start,
                        base_offset + i,
                    )));
                }

                if let Some((text_span, url_span, total_len)) =
                    parse_link(&text[i..], base_offset + i)
                {
                    result.push(InlineElement::Link {
                        text: text_span,
                        url: url_span,
                    });

                    let skip_to = i + total_len;
                    while chars.peek().map(|(idx, _)| *idx < skip_to).unwrap_or(false) {
                        chars.next();
                    }
                    plain_start = skip_to;
                } else {
                    // Not a valid link, treat [ as plain text
                    plain_start = i;
                }
            }

            // Exclamation - potential image
            '!' => {
                if chars.peek().map(|(_, c)| *c == '[').unwrap_or(false) {
                    // Flush pending text
                    if i > plain_start {
                        result.push(InlineElement::Text(Span::new(
                            base_offset + plain_start,
                            base_offset + i,
                        )));
                    }

                    chars.next(); // consume [

                    if let Some((alt_span, url_span, link_len)) =
                        parse_link(&text[i + 1..], base_offset + i + 1)
                    {
                        result.push(InlineElement::Image {
                            alt: alt_span,
                            url: url_span,
                        });

                        let skip_to = i + 1 + link_len;
                        while chars.peek().map(|(idx, _)| *idx < skip_to).unwrap_or(false) {
                            chars.next();
                        }
                        plain_start = skip_to;
                    } else {
                        // Not a valid image
                        plain_start = i;
                    }
                }
            }

            // Newline - could be hard break
            '\n' => {
                // Check for hard line break (two spaces before newline)
                if i >= 2 && text[..i].ends_with("  ") {
                    // Flush text without trailing spaces
                    let text_end = i - 2;
                    if text_end > plain_start {
                        result.push(InlineElement::Text(Span::new(
                            base_offset + plain_start,
                            base_offset + text_end,
                        )));
                    }
                    result.push(InlineElement::LineBreak);
                    plain_start = i + 1;
                }
                // Otherwise soft line break, keep in text
            }

            _ => {
                // Regular character, continue
            }
        }
    }

    // Flush remaining plain text
    if plain_start < text.len() {
        result.push(InlineElement::Text(Span::new(
            base_offset + plain_start,
            base_offset + text.len(),
        )));
    }

    // Collapse adjacent Text elements
    collapse_text_elements(&mut result);

    result
}

/// Find closing backticks matching the opening count.
fn find_closing_backticks(text: &str, count: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'`' {
            // Count consecutive backticks at this position
            let run_start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            let run_len = i - run_start;
            if run_len == count {
                return Some(run_start);
            }
            // Not the right count, continue
        } else {
            // Skip non-backtick character (handle UTF-8)
            i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }
    None
}

/// Find closing emphasis marker.
/// Returns (end_position, actual_close_len) if found.
fn find_closing_emphasis(text: &str, marker: char, open_count: usize) -> Option<(usize, usize)> {
    let mut chars = text.char_indices().peekable();

    while let Some((pos, c)) = chars.next() {
        if c == marker {
            // Count consecutive markers
            let mut count = 1;
            while chars.peek().map(|(_, ch)| *ch == marker).unwrap_or(false) {
                chars.next();
                count += 1;
            }

            // Check if this could close (not followed by alphanumeric for _)
            let can_close = if marker == '_' {
                chars.peek().is_none_or(|(_, next_c)| {
                    next_c.is_whitespace() || next_c.is_ascii_punctuation()
                })
            } else {
                true
            };

            if can_close && count >= open_count.min(3) {
                let close_len = count.min(open_count).min(3);
                return Some((pos, close_len));
            }
        }
    }
    None
}

/// Parse a link starting with [
/// Returns (text_span, url_span, total_bytes_consumed)
fn parse_link(text: &str, base_offset: usize) -> Option<(Span, Span, usize)> {
    if !text.starts_with('[') {
        return None;
    }

    // Find closing ]
    let mut bracket_depth = 0;
    let mut bracket_end = None;

    for (i, c) in text.char_indices() {
        match c {
            '[' => bracket_depth += 1,
            ']' => {
                bracket_depth -= 1;
                if bracket_depth == 0 {
                    bracket_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let bracket_end = bracket_end?;

    // Check for ( immediately after ]
    let rest = &text[bracket_end + 1..];
    if !rest.starts_with('(') {
        return None;
    }

    // Find closing )
    let mut paren_depth = 0;
    let mut paren_end = None;

    for (i, c) in rest.char_indices() {
        match c {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    paren_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let paren_end = paren_end?;

    // text_span: content between [ and ]
    let text_span = Span::new(base_offset + 1, base_offset + bracket_end);
    // url_span: content between ( and )
    let url_start = bracket_end + 1 + 1; // ] + (
    let url_end = bracket_end + 1 + paren_end; // position of )
    let url_span = Span::new(base_offset + url_start, base_offset + url_end);

    // Total consumed: [ + text + ] + ( + url + )
    let total = bracket_end + 1 + paren_end + 1;

    Some((text_span, url_span, total))
}

/// Collapse adjacent Text elements into one.
fn collapse_text_elements(elements: &mut Vec<InlineElement>) {
    if elements.len() < 2 {
        return;
    }

    let mut write = 0;
    for read in 1..elements.len() {
        if let (InlineElement::Text(a), InlineElement::Text(b)) =
            (&elements[write], &elements[read])
        {
            // Merge spans — contiguous or not, just extend to cover both
            let merged = Span::new(a.start, b.end);
            elements[write] = InlineElement::Text(merged);
        } else {
            write += 1;
            if write != read {
                elements.swap(write, read);
            }
        }
    }
    elements.truncate(write + 1);
}

/// Streaming inline parser state.
/// Tracks partial inline elements across token boundaries.
pub struct InlineState {
    /// Accumulated text waiting to be parsed
    buffer: String,
    /// Current partial element being built
    partial: Option<PartialKind>,
}

impl InlineState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            partial: None,
        }
    }

    /// Push new text and try to extract complete inline elements.
    /// Returns elements that are definitely complete.
    pub fn push(&mut self, text: &str) -> Vec<InlineElement> {
        self.buffer.push_str(text);
        self.extract_complete()
    }

    /// Get current buffer content for speculative rendering.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Check if we might be in the middle of an inline element.
    pub fn has_potential_partial(&self) -> bool {
        self.partial.is_some()
            || self.buffer.ends_with('`')
            || self.buffer.ends_with('*')
            || self.buffer.ends_with('_')
            || self.buffer.ends_with('~')
            || self.buffer.ends_with('[')
            || self.buffer.ends_with('!')
    }

    /// Finalize - return whatever we have as parsed elements.
    pub fn finalize(self) -> Vec<InlineElement> {
        parse_inline(&self.buffer, 0)
    }

    /// Extract complete inline elements from the buffer.
    fn extract_complete(&mut self) -> Vec<InlineElement> {
        let result = parse_inline(&self.buffer, 0);

        // Check if the buffer might have incomplete markers at the end
        if self.has_incomplete_tail() {
            // Keep the buffer, don't return anything yet
            return Vec::new();
        }

        // Buffer is stable, clear it and return parsed result
        self.buffer.clear();
        result
    }

    /// Check if the buffer ends with potentially incomplete markers.
    fn has_incomplete_tail(&self) -> bool {
        let s = &self.buffer;

        // Check for unclosed backticks
        let backtick_count = s.chars().filter(|&c| c == '`').count();
        if backtick_count % 2 != 0 {
            return true;
        }

        // Check for unclosed brackets
        let open_brackets = s.chars().filter(|&c| c == '[').count();
        let close_brackets = s.chars().filter(|&c| c == ']').count();
        if open_brackets > close_brackets {
            return true;
        }

        // Check for trailing asterisks/underscores that might start formatting
        if s.ends_with('*') || s.ends_with('_') || s.ends_with('~') {
            return true;
        }

        false
    }
}

impl Default for InlineState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve<'a>(span: &Span, text: &'a str) -> &'a str {
        span.resolve(text)
    }

    #[test]
    fn test_inline_code() {
        let text = "some `code` here";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Code(s) if resolve(s, text) == "code"
        )));
    }

    #[test]
    fn test_bold() {
        let text = "some **bold** text";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Bold, content } if resolve(content, text) == "bold"
        )));
    }

    #[test]
    fn test_italic() {
        let text = "some *italic* text";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Italic, content } if resolve(content, text) == "italic"
        )));
    }

    #[test]
    fn test_link() {
        let text = "check [this](https://example.com) out";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Link { text: t, url } if resolve(t, text) == "this" && resolve(url, text) == "https://example.com"
        )));
    }

    #[test]
    fn test_image() {
        let text = "see ![alt](img.png) here";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Image { alt, url } if resolve(alt, text) == "alt" && resolve(url, text) == "img.png"
        )));
    }

    #[test]
    fn test_strikethrough() {
        let text = "some ~~deleted~~ text";
        let result = parse_inline(text, 0);
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Strikethrough, content } if resolve(content, text) == "deleted"
        )));
    }

    #[test]
    fn test_mixed() {
        let text = "**bold** and *italic* and `code`";
        let result = parse_inline(text, 0);
        assert_eq!(
            result
                .iter()
                .filter(|e| !matches!(e, InlineElement::Text(_)))
                .count(),
            3
        );
    }
}
