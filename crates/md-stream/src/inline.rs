//! Inline element parsing for bold, italic, code, links, etc.

use crate::element::{InlineElement, InlineStyle};
use crate::partial::PartialKind;

/// Parses inline elements from text.
/// Returns a vector of inline elements.
///
/// Note: This is called on complete paragraph text, not streaming.
/// For streaming, we use PartialKind to track incomplete markers.
pub fn parse_inline(text: &str) -> Vec<InlineElement> {
    let mut result = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut plain_start = 0;

    while let Some((i, c)) = chars.next() {
        match c {
            // Backtick - inline code
            '`' => {
                // Flush any pending plain text
                if i > plain_start {
                    result.push(InlineElement::Text(text[plain_start..i].to_string()));
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
                    let code_content = &text[start_pos..start_pos + end_pos];
                    // Strip single leading/trailing space if present (CommonMark rule)
                    let trimmed = if code_content.starts_with(' ')
                        && code_content.ends_with(' ')
                        && code_content.len() > 1
                    {
                        &code_content[1..code_content.len() - 1]
                    } else {
                        code_content
                    };
                    result.push(InlineElement::Code(trimmed.to_string()));

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
                if let Some((content, close_len, end_pos)) =
                    find_closing_emphasis(&text[content_start..], marker, effective_count)
                {
                    // Flush pending plain text
                    if marker_start > plain_start {
                        result.push(InlineElement::Text(
                            text[plain_start..marker_start].to_string(),
                        ));
                    }

                    let style = match close_len {
                        1 => InlineStyle::Italic,
                        2 => InlineStyle::Bold,
                        _ => InlineStyle::BoldItalic,
                    };

                    result.push(InlineElement::Styled {
                        style,
                        content: content.to_string(),
                    });

                    // Advance past the content and closing marker
                    let skip_to = content_start + end_pos + close_len;
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
                        result.push(InlineElement::Text(text[plain_start..i].to_string()));
                    }

                    let content_start = i + 2;

                    // Find closing ~~
                    if let Some(end_pos) = text[content_start..].find("~~") {
                        let content = &text[content_start..content_start + end_pos];
                        result.push(InlineElement::Styled {
                            style: InlineStyle::Strikethrough,
                            content: content.to_string(),
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
                    result.push(InlineElement::Text(text[plain_start..i].to_string()));
                }

                if let Some((text_content, url, total_len)) = parse_link(&text[i..]) {
                    result.push(InlineElement::Link {
                        text: text_content,
                        url,
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
                        result.push(InlineElement::Text(text[plain_start..i].to_string()));
                    }

                    chars.next(); // consume [

                    if let Some((alt, url, link_len)) = parse_link(&text[i + 1..]) {
                        result.push(InlineElement::Image { alt, url });

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
                        result.push(InlineElement::Text(text[plain_start..text_end].to_string()));
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
        let remaining = &text[plain_start..];
        if !remaining.is_empty() {
            result.push(InlineElement::Text(remaining.to_string()));
        }
    }

    // Collapse adjacent Text elements
    collapse_text_elements(&mut result);

    result
}

/// Find closing backticks matching the opening count.
fn find_closing_backticks(text: &str, count: usize) -> Option<usize> {
    let target: String = "`".repeat(count);
    let mut i = 0;

    while i < text.len() {
        if text[i..].starts_with(&target) {
            // Make sure it's exactly this many backticks
            let after = i + count;
            if after >= text.len() || !text[after..].starts_with('`') {
                return Some(i);
            }
            // More backticks - skip them
            while i < text.len() && text[i..].starts_with('`') {
                i += 1;
            }
        } else {
            i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }
    None
}

/// Find closing emphasis marker.
/// Returns (content, actual_close_len, end_position) if found.
fn find_closing_emphasis(
    text: &str,
    marker: char,
    open_count: usize,
) -> Option<(&str, usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;

    while i < chars.len() {
        let (pos, c) = chars[i];

        if c == marker {
            // Count consecutive markers
            let mut count = 1;
            while i + count < chars.len() && chars[i + count].1 == marker {
                count += 1;
            }

            // Check if this could close (not followed by alphanumeric for _)
            let can_close = if marker == '_' {
                i + count >= chars.len() || {
                    let next_char = chars.get(i + count).map(|(_, c)| *c);
                    next_char
                        .map(|c| c.is_whitespace() || c.is_ascii_punctuation())
                        .unwrap_or(true)
                }
            } else {
                true
            };

            if can_close && count >= open_count.min(3) {
                let close_len = count.min(open_count).min(3);
                return Some((&text[..pos], close_len, pos));
            }

            i += count;
        } else {
            i += 1;
        }
    }
    None
}

/// Parse a link starting with [
/// Returns (text, url, total_bytes_consumed)
fn parse_link(text: &str) -> Option<(String, String, usize)> {
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
    let link_text = &text[1..bracket_end];

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
    let url = &rest[1..paren_end];

    // Total consumed: [ + text + ] + ( + url + )
    let total = bracket_end + 1 + paren_end + 1;

    Some((link_text.to_string(), url.to_string(), total))
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
            let combined = format!("{}{}", a, b);
            elements[write] = InlineElement::Text(combined);
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
        parse_inline(&self.buffer)
    }

    /// Extract complete inline elements from the buffer.
    fn extract_complete(&mut self) -> Vec<InlineElement> {
        // For streaming, we're conservative - only return elements when
        // we're confident they won't change.
        //
        // Strategy: Parse the whole buffer, but only return elements that
        // end before any trailing ambiguous characters.

        let result = parse_inline(&self.buffer);

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

    #[test]
    fn test_inline_code() {
        let result = parse_inline("some `code` here");
        assert!(result
            .iter()
            .any(|e| matches!(e, InlineElement::Code(s) if s == "code")));
    }

    #[test]
    fn test_bold() {
        let result = parse_inline("some **bold** text");
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Bold, content } if content == "bold"
        )));
    }

    #[test]
    fn test_italic() {
        let result = parse_inline("some *italic* text");
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Italic, content } if content == "italic"
        )));
    }

    #[test]
    fn test_link() {
        let result = parse_inline("check [this](https://example.com) out");
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Link { text, url } if text == "this" && url == "https://example.com"
        )));
    }

    #[test]
    fn test_image() {
        let result = parse_inline("see ![alt](img.png) here");
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Image { alt, url } if alt == "alt" && url == "img.png"
        )));
    }

    #[test]
    fn test_strikethrough() {
        let result = parse_inline("some ~~deleted~~ text");
        assert!(result.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Strikethrough, content } if content == "deleted"
        )));
    }

    #[test]
    fn test_mixed() {
        let result = parse_inline("**bold** and *italic* and `code`");
        assert_eq!(
            result
                .iter()
                .filter(|e| !matches!(e, InlineElement::Text(_)))
                .count(),
            3
        );
    }
}
