//! Core streaming parser implementation.

use crate::element::{CodeBlock, MdElement, Span};
use crate::inline::parse_inline;
use crate::partial::{Partial, PartialKind};

/// Incremental markdown parser for streaming input.
///
/// Maintains a single contiguous buffer of incoming text and tracks
/// a processing cursor to allow progressive rendering as content streams in.
pub struct StreamParser {
    /// Contiguous buffer of all pushed text
    buffer: String,

    /// Completed markdown elements
    parsed: Vec<MdElement>,

    /// Current in-progress element (if any)
    partial: Option<Partial>,

    /// Byte offset of first unprocessed content in buffer
    process_pos: usize,

    /// Are we at the start of a line? (for block-level detection)
    at_line_start: bool,
}

/// Lightweight dispatch tag for partial state, avoiding Clone on PartialKind
/// which contains Vecs (table headers/rows).
#[derive(Clone, Copy)]
enum PartialDispatch {
    CodeFence { fence_char: char, fence_len: usize },
    Heading { level: u8 },
    Table,
    Paragraph,
    Other,
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            parsed: Vec::new(),
            partial: None,
            process_pos: 0,
            at_line_start: true,
        }
    }

    /// Push a new token chunk and process it.
    pub fn push(&mut self, token: &str) {
        if token.is_empty() {
            return;
        }

        self.buffer.push_str(token);
        self.process_new_content();
    }

    /// Get completed elements for rendering.
    pub fn parsed(&self) -> &[MdElement] {
        &self.parsed
    }

    /// Get the parser's buffer for resolving spans.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Consume the parser and return the completed elements and buffer.
    pub fn into_parts(self) -> (Vec<MdElement>, String) {
        (self.parsed, self.buffer)
    }

    /// Consume the parser and return the completed elements.
    pub fn into_parsed(self) -> Vec<MdElement> {
        self.parsed
    }

    /// Get the current partial state (for speculative rendering).
    pub fn partial(&self) -> Option<&Partial> {
        self.partial.as_ref()
    }

    /// Get the speculative content that would render from partial state.
    /// Returns the raw accumulated text that isn't yet a complete element.
    pub fn partial_content(&self) -> Option<&str> {
        self.partial.as_ref().map(|p| p.content(&self.buffer))
    }

    /// Check if we're currently inside a code block.
    pub fn in_code_block(&self) -> bool {
        matches!(
            self.partial.as_ref().map(|p| &p.kind),
            Some(PartialKind::CodeFence { .. })
        )
    }

    /// Get the unprocessed portion of the buffer.
    fn remaining(&self) -> &str {
        &self.buffer[self.process_pos..]
    }

    /// Compute a trimmed span (strip leading/trailing whitespace).
    fn trim_span(&self, span: Span) -> Span {
        let s = &self.buffer[span.start..span.end];
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Span::new(span.start, span.start);
        }
        let ltrim = s.len() - s.trim_start().len();
        Span::new(span.start + ltrim, span.start + ltrim + trimmed.len())
    }

    /// Extract the dispatch info from the current partial state.
    /// Returns only small Copy data to avoid cloning Vecs in PartialKind::Table.
    fn partial_dispatch(&self) -> Option<PartialDispatch> {
        self.partial.as_ref().map(|p| match &p.kind {
            PartialKind::CodeFence {
                fence_char,
                fence_len,
                ..
            } => PartialDispatch::CodeFence {
                fence_char: *fence_char,
                fence_len: *fence_len,
            },
            PartialKind::Heading { level } => PartialDispatch::Heading { level: *level },
            PartialKind::Table { .. } => PartialDispatch::Table,
            PartialKind::Paragraph => PartialDispatch::Paragraph,
            _ => PartialDispatch::Other,
        })
    }

    /// Process newly added content.
    fn process_new_content(&mut self) {
        while self.process_pos < self.buffer.len() {
            // Handle based on current partial state
            if let Some(dispatch) = self.partial_dispatch() {
                match dispatch {
                    PartialDispatch::CodeFence {
                        fence_char,
                        fence_len,
                    } => {
                        if self.process_code_fence(fence_char, fence_len) {
                            continue;
                        }
                        return; // Need more input
                    }
                    PartialDispatch::Heading { level } => {
                        if self.process_heading(level) {
                            continue;
                        }
                        return;
                    }
                    PartialDispatch::Table => {
                        if self.process_table() {
                            continue;
                        }
                        return;
                    }
                    PartialDispatch::Paragraph => {
                        // For paragraphs, check if we're at a line start that could be a block element
                        if self.at_line_start {
                            // Take the paragraph partial first — try_block_start may
                            // replace self.partial with the new block element
                            let para_partial = self.partial.take();

                            if let Some(consumed) = self.try_block_start() {
                                // Emit the saved paragraph before the new block
                                if let Some(partial) = para_partial {
                                    let span = partial.content_span();
                                    let trimmed = self.trim_span(span);
                                    if !trimmed.is_empty() {
                                        let content = trimmed.resolve(&self.buffer);
                                        let inline_elements = parse_inline(content, trimmed.start);
                                        self.parsed.push(MdElement::Paragraph(inline_elements));
                                    }
                                }
                                self.advance(consumed);
                                continue;
                            }

                            // Block start failed — restore the paragraph partial
                            self.partial = para_partial;
                            // If remaining could be the start of a block element but we
                            // don't have enough chars yet, wait for more input rather than
                            // consuming into the paragraph (e.g. "`" could become "```")
                            if self.could_be_block_start() {
                                return;
                            }
                        }
                        // Continue with inline processing
                        if self.process_inline() {
                            continue;
                        }
                        return;
                    }
                    PartialDispatch::Other => {
                        // For other inline elements, process character by character
                        if self.process_inline() {
                            continue;
                        }
                        return;
                    }
                }
            }

            // No partial state - detect new elements
            if self.at_line_start {
                if let Some(consumed) = self.try_block_start() {
                    self.advance(consumed);
                    continue;
                }
                if self.could_be_block_start() {
                    return;
                }
            }

            // Fall back to inline processing
            if self.process_inline() {
                continue;
            }
            return;
        }
    }

    /// Check if remaining text could be the start of a block element but we don't
    /// have enough characters to confirm yet. Used to defer consuming
    /// ambiguous prefixes like "`" or "``" that might become "```".
    fn could_be_block_start(&self) -> bool {
        let trimmed = self.remaining().trim_start();
        if trimmed.is_empty() {
            return false;
        }

        // Could be a code fence: need at least 3 backticks or tildes
        if trimmed.len() < 3 {
            let first = trimmed.as_bytes()[0];
            if first == b'`' || first == b'~' {
                // All chars so far are the same fence char
                return trimmed.bytes().all(|b| b == first);
            }
        }

        // Could be a thematic break: need "---", "***", or "___"
        if trimmed.len() < 3 {
            let first = trimmed.as_bytes()[0];
            if first == b'-' || first == b'*' || first == b'_' {
                return trimmed.bytes().all(|b| b == first);
            }
        }

        // Could be a table row: starts with | but no newline yet
        if trimmed.starts_with('|') && !trimmed.contains('\n') {
            return true;
        }

        false
    }

    /// Try to detect a block-level element at line start.
    /// Returns bytes consumed if successful.
    fn try_block_start(&mut self) -> Option<usize> {
        let text = self.remaining();
        let trimmed = text.trim_start();
        let leading_space = text.len() - trimmed.len();

        // Heading: # ## ### etc
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|&c| c == '#').count();
            if level <= 6 {
                if let Some(rest) = trimmed.get(level..) {
                    if rest.starts_with(' ') || rest.is_empty() {
                        let consumed = leading_space + level + rest.starts_with(' ') as usize;
                        let content_start = self.process_pos + consumed;
                        let mut partial = Partial::new(
                            PartialKind::Heading { level: level as u8 },
                            self.process_pos,
                        );
                        partial.content_start = content_start;
                        partial.content_end = content_start;
                        self.partial = Some(partial);
                        self.at_line_start = false;
                        return Some(consumed);
                    }
                }
            }
        }

        // Code fence: ``` or ~~~
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fence_char = trimmed.chars().next().unwrap();
            let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();

            if fence_len >= 3 {
                let after_fence = &trimmed[fence_len..];
                let (language, consumed_lang) = if let Some(nl_pos) = after_fence.find('\n') {
                    let lang = after_fence[..nl_pos].trim();
                    let lang_span = if lang.is_empty() {
                        None
                    } else {
                        // Compute absolute span for the language
                        let lang_start_in_after = after_fence[..nl_pos].as_ptr() as usize
                            - after_fence.as_ptr() as usize
                            + (after_fence[..nl_pos].len()
                                - after_fence[..nl_pos].trim_start().len());
                        let abs_start =
                            self.process_pos + leading_space + fence_len + lang_start_in_after;
                        Some(Span::new(abs_start, abs_start + lang.len()))
                    };
                    (lang_span, nl_pos + 1)
                } else {
                    // No newline yet - language might be incomplete
                    let lang = after_fence.trim();
                    let lang_span = if lang.is_empty() {
                        None
                    } else {
                        let lang_start_in_after =
                            after_fence.len() - after_fence.trim_start().len();
                        let abs_start =
                            self.process_pos + leading_space + fence_len + lang_start_in_after;
                        Some(Span::new(abs_start, abs_start + lang.len()))
                    };
                    (lang_span, after_fence.len())
                };

                let consumed = leading_space + fence_len + consumed_lang;
                let content_start = self.process_pos + consumed;
                let mut partial = Partial::new(
                    PartialKind::CodeFence {
                        fence_char,
                        fence_len,
                        language,
                    },
                    self.process_pos,
                );
                partial.content_start = content_start;
                partial.content_end = content_start;
                self.partial = Some(partial);
                self.at_line_start = false;
                return Some(consumed);
            }
        }

        // Thematic break: --- *** ___
        if (trimmed.starts_with("---") || trimmed.starts_with("***") || trimmed.starts_with("___"))
            && trimmed.chars().filter(|&c| !c.is_whitespace()).count() >= 3
        {
            let break_char = trimmed.chars().next().unwrap();
            if trimmed
                .chars()
                .all(|c| c == break_char || c.is_whitespace())
            {
                if let Some(nl_pos) = text.find('\n') {
                    self.parsed.push(MdElement::ThematicBreak);
                    self.at_line_start = true;
                    return Some(nl_pos + 1);
                }
            }
        }

        // Table row: starts with |
        if trimmed.starts_with('|') {
            if let Some(nl_pos) = trimmed.find('\n') {
                let line = &trimmed[..nl_pos];
                let line_abs_offset = self.process_pos + leading_space;
                let cells = parse_table_row(line, line_abs_offset);
                if !cells.is_empty() {
                    let mut partial = Partial::new(
                        PartialKind::Table {
                            headers: cells,
                            rows: Vec::new(),
                            seen_separator: false,
                        },
                        self.process_pos,
                    );
                    partial.content_start = self.process_pos;
                    partial.content_end = self.process_pos + leading_space + nl_pos;
                    self.partial = Some(partial);
                    self.at_line_start = true;
                    return Some(leading_space + nl_pos + 1);
                }
            }
        }

        None
    }

    /// Process content inside a code fence.
    /// Returns true if we should continue processing, false if we need more input.
    fn process_code_fence(&mut self, fence_char: char, fence_len: usize) -> bool {
        let text_start = self.process_pos;
        let text_end = self.buffer.len();
        let mut pos = text_start;

        while pos < text_end {
            // Find next line boundary
            let line_end = self.buffer[pos..text_end]
                .find('\n')
                .map(|i| pos + i + 1)
                .unwrap_or(text_end);
            let line = &self.buffer[pos..line_end];

            let partial = self.partial.as_mut().unwrap();

            // Check if we're at a line start within the code fence
            let at_content_line_start =
                partial.content_is_empty() || self.buffer[..partial.content_end].ends_with('\n');

            if at_content_line_start {
                let trimmed = line.trim_start();

                // Check for closing fence
                if trimmed.len() >= fence_len
                    && trimmed
                        .as_bytes()
                        .iter()
                        .take(fence_len)
                        .all(|&b| b == fence_char as u8)
                {
                    let after_fence = &trimmed[fence_len..];
                    if after_fence.trim().is_empty() || after_fence.starts_with('\n') {
                        // Found closing fence! Complete the code block
                        let language =
                            if let PartialKind::CodeFence { language, .. } = &partial.kind {
                                *language
                            } else {
                                None
                            };

                        let content_span = partial.content_span();
                        self.parsed.push(MdElement::CodeBlock(CodeBlock {
                            language,
                            content: content_span,
                        }));
                        self.partial = None;
                        self.at_line_start = true;

                        // Advance past the closing fence line
                        self.advance(line_end - text_start);
                        return true;
                    }
                }

                // If this could be the start of a closing fence but we don't
                // have enough chars yet, wait for more input
                if !trimmed.is_empty()
                    && trimmed.len() < fence_len
                    && trimmed.bytes().all(|b| b == fence_char as u8)
                    && !line.contains('\n')
                {
                    // Don't advance — wait for more chars
                    return false;
                }
            }

            // Not a closing fence - extend content span to include this line
            partial.content_end += line.len();
            pos = line_end;
        }

        // Consumed all available text, need more
        self.advance(text_end - text_start);
        false
    }

    /// Process heading content until newline.
    fn process_heading(&mut self, level: u8) -> bool {
        let remaining = self.remaining();
        if let Some(nl_pos) = remaining.find('\n') {
            let partial = self.partial.as_mut().unwrap();
            partial.content_end += nl_pos;

            let content_span = partial.content_span();
            let trimmed = self.trim_span(content_span);
            self.parsed.push(MdElement::Heading {
                level,
                content: trimmed,
            });
            self.partial = None;
            self.at_line_start = true;
            self.advance(nl_pos + 1);
            true
        } else {
            // No newline yet - accumulate
            let len = remaining.len();
            let partial = self.partial.as_mut().unwrap();
            partial.content_end += len;
            self.advance(len);
            false
        }
    }

    /// Process table content line by line.
    /// Returns true if we should continue processing, false if we need more input.
    fn process_table(&mut self) -> bool {
        let remaining = self.remaining();
        // We need at least one complete line to process
        if let Some(nl_pos) = remaining.find('\n') {
            let line = &remaining[..nl_pos];
            let trimmed = line.trim();

            // Check if this line continues the table
            if trimmed.starts_with('|') {
                // Capture everything we need from remaining before dropping the borrow
                let is_sep = is_separator_row(trimmed);
                let line_abs_offset = self.process_pos;
                let trim_offset = line.len() - trimmed.len();
                let trimmed_span = Span::new(
                    self.process_pos + trim_offset,
                    self.process_pos + trim_offset + trimmed.len(),
                );
                let cells = parse_table_row(trimmed, line_abs_offset + trim_offset);
                let partial = self.partial.as_mut().unwrap();
                if let PartialKind::Table {
                    ref mut rows,
                    ref mut seen_separator,
                    ref headers,
                    ..
                } = partial.kind
                {
                    if !*seen_separator {
                        // Expecting separator row
                        if is_sep {
                            *seen_separator = true;
                        } else {
                            // Not a valid table — emit header as paragraph
                            let header_text = format!(
                                "| {} |",
                                headers
                                    .iter()
                                    .map(|s| s.resolve(&self.buffer))
                                    .collect::<Vec<_>>()
                                    .join(" | ")
                            );
                            let row_text = trimmed_span.resolve(&self.buffer);
                            self.partial = None;
                            let combined = format!("{}\n{}", header_text, row_text);
                            let inlines = parse_inline(&combined, 0);
                            self.parsed.push(MdElement::Paragraph(inlines));
                            self.at_line_start = true;
                            self.advance(nl_pos + 1);
                            return true;
                        }
                    } else {
                        // Data row
                        rows.push(cells);
                    }
                }
                self.advance(nl_pos + 1);
                return true;
            }

            // Line doesn't start with | — table is complete
            let partial = self.partial.take().unwrap();
            if let PartialKind::Table {
                headers,
                rows,
                seen_separator,
            } = partial.kind
            {
                if seen_separator {
                    self.parsed.push(MdElement::Table { headers, rows });
                } else {
                    // Never saw separator — emit as paragraph
                    let text = format!(
                        "| {} |",
                        headers
                            .iter()
                            .map(|s| s.resolve(&self.buffer))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                    let inlines = parse_inline(&text, 0);
                    self.parsed.push(MdElement::Paragraph(inlines));
                }
            }
            self.at_line_start = true;
            // Don't advance — let the non-table line be re-processed
            return true;
        }

        // No newline yet — check if we have a partial line starting with |
        // If so, wait for more input. If not, table is done.
        let trimmed = remaining.trim();
        if trimmed.starts_with('|') || trimmed.is_empty() {
            // Could be another table row, wait for newline
            return false;
        }

        // Non-pipe content without newline — table is complete
        let partial = self.partial.take().unwrap();
        if let PartialKind::Table {
            headers,
            rows,
            seen_separator,
        } = partial.kind
        {
            if seen_separator {
                self.parsed.push(MdElement::Table { headers, rows });
            } else {
                let text = format!(
                    "| {} |",
                    headers
                        .iter()
                        .map(|s| s.resolve(&self.buffer))
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
                let inlines = parse_inline(&text, 0);
                self.parsed.push(MdElement::Paragraph(inlines));
            }
        }
        self.at_line_start = true;
        true
    }

    /// Process inline content.
    fn process_inline(&mut self) -> bool {
        let remaining = self.remaining();

        // Check for paragraph break split across tokens:
        // partial content ends with \n and new text starts with \n
        if remaining.starts_with('\n') {
            if let Some(ref partial) = self.partial {
                if self.buffer[..partial.content_end].ends_with('\n') {
                    // Double newline split across token boundary — emit paragraph
                    let span = partial.content_span();
                    let trimmed = self.trim_span(span);
                    self.partial = None;

                    if !trimmed.is_empty() {
                        let content = trimmed.resolve(&self.buffer);
                        let inline_elements = parse_inline(content, trimmed.start);
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                    self.at_line_start = true;
                    self.advance(1); // consume the \n
                    return true;
                }
            }
        }

        if let Some(nl_pos) = remaining.find('\n') {
            let after_nl = &remaining[nl_pos + 1..];

            // Check if text after the newline starts a block element (code fence, heading, etc.)
            // If so, emit the current paragraph and let the block parser handle the rest.
            if !after_nl.is_empty() {
                let trimmed_after = after_nl.trim_start();
                let is_block_start = trimmed_after.starts_with("```")
                    || trimmed_after.starts_with("~~~")
                    || trimmed_after.starts_with('#')
                    || trimmed_after.starts_with('|');
                if is_block_start {
                    // Accumulate text before the newline into the paragraph
                    if let Some(ref mut partial) = self.partial {
                        partial.content_end += nl_pos;
                        let span = partial.content_span();
                        let trimmed = self.trim_span(span);
                        self.partial = None;

                        if !trimmed.is_empty() {
                            let content = trimmed.resolve(&self.buffer);
                            let inline_elements = parse_inline(content, trimmed.start);
                            self.parsed.push(MdElement::Paragraph(inline_elements));
                        }
                    } else {
                        let start = self.process_pos;
                        let end = self.process_pos + nl_pos;
                        let span = Span::new(start, end);
                        let trimmed = self.trim_span(span);

                        if !trimmed.is_empty() {
                            let content = trimmed.resolve(&self.buffer);
                            let inline_elements = parse_inline(content, trimmed.start);
                            self.parsed.push(MdElement::Paragraph(inline_elements));
                        }
                    }
                    self.at_line_start = true;
                    self.advance(nl_pos + 1);
                    return true;
                }
            }
        }

        // Re-borrow remaining since prior branches may not have taken
        let remaining = self.remaining();

        if let Some(nl_pos) = remaining.find("\n\n") {
            // Double newline = paragraph break
            // Combine accumulated partial content with text before \n\n
            if let Some(ref mut partial) = self.partial {
                partial.content_end += nl_pos;
                let span = partial.content_span();
                let trimmed = self.trim_span(span);
                self.partial = None;

                if !trimmed.is_empty() {
                    let content = trimmed.resolve(&self.buffer);
                    let inline_elements = parse_inline(content, trimmed.start);
                    self.parsed.push(MdElement::Paragraph(inline_elements));
                }
            } else {
                let start = self.process_pos;
                let end = self.process_pos + nl_pos;
                let span = Span::new(start, end);
                let trimmed = self.trim_span(span);

                if !trimmed.is_empty() {
                    let content = trimmed.resolve(&self.buffer);
                    let inline_elements = parse_inline(content, trimmed.start);
                    self.parsed.push(MdElement::Paragraph(inline_elements));
                }
            }
            self.at_line_start = true;
            self.advance(nl_pos + 2);
            return true;
        }

        if let Some(nl_pos) = remaining.find('\n') {
            // Single newline - continue accumulating but track position
            if let Some(ref mut partial) = self.partial {
                partial.content_end += nl_pos + 1;
            } else {
                // Start accumulating paragraph
                let content_start = self.process_pos;
                let content_end = self.process_pos + nl_pos + 1;
                self.partial = Some(Partial {
                    kind: PartialKind::Paragraph,
                    start_pos: self.process_pos,
                    content_start,
                    content_end,
                });
            }
            self.at_line_start = true;
            self.advance(nl_pos + 1);
            return true;
        }

        // No newline - accumulate
        let len = remaining.len();
        if let Some(ref mut partial) = self.partial {
            partial.content_end += len;
        } else {
            let content_start = self.process_pos;
            let content_end = self.process_pos + len;
            self.partial = Some(Partial {
                kind: PartialKind::Paragraph,
                start_pos: self.process_pos,
                content_start,
                content_end,
            });
        }
        self.at_line_start = false;
        self.advance(len);
        false
    }

    /// Advance the processing position by n bytes.
    fn advance(&mut self, n: usize) {
        self.process_pos += n;
    }

    /// Finalize parsing (call when stream ends).
    /// Converts any remaining partial state to complete elements.
    pub fn finalize(&mut self) {
        if let Some(partial) = self.partial.take() {
            match partial.kind {
                PartialKind::CodeFence { language, .. } => {
                    // Unclosed code block - emit what we have
                    self.parsed.push(MdElement::CodeBlock(CodeBlock {
                        language,
                        content: partial.content_span(),
                    }));
                }
                PartialKind::Heading { level } => {
                    let trimmed = self.trim_span(partial.content_span());
                    self.parsed.push(MdElement::Heading {
                        level,
                        content: trimmed,
                    });
                }
                PartialKind::Table {
                    headers,
                    rows,
                    seen_separator,
                } => {
                    if seen_separator {
                        self.parsed.push(MdElement::Table { headers, rows });
                    } else {
                        // Never saw separator — not a real table, emit as paragraph
                        let text = format!(
                            "| {} |",
                            headers
                                .iter()
                                .map(|s| s.resolve(&self.buffer))
                                .collect::<Vec<_>>()
                                .join(" | ")
                        );
                        let inlines = parse_inline(&text, 0);
                        self.parsed.push(MdElement::Paragraph(inlines));
                    }
                }
                PartialKind::Paragraph => {
                    let trimmed = self.trim_span(partial.content_span());
                    if !trimmed.is_empty() {
                        let content = trimmed.resolve(&self.buffer);
                        let inline_elements = parse_inline(content, trimmed.start);
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                }
                _ => {
                    // Other partial kinds (lists, blockquotes, etc.) - emit as paragraph for now
                    let trimmed = self.trim_span(partial.content_span());
                    if !trimmed.is_empty() {
                        let content = trimmed.resolve(&self.buffer);
                        let inline_elements = parse_inline(content, trimmed.start);
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                }
            }
        }
    }
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a table row into cell spans by splitting on `|`.
/// `line_offset` is the absolute buffer position of `line`.
fn parse_table_row(line: &str, line_offset: usize) -> Vec<Span> {
    let trimmed = line.trim();
    let trim_start = line.len() - line.trim_start().len();
    let base = line_offset + trim_start;

    let inner_start;
    let inner;
    if let Some(stripped) = trimmed.strip_prefix('|') {
        inner_start = base + 1;
        inner = stripped.strip_suffix('|').unwrap_or(stripped);
    } else {
        inner_start = base;
        inner = trimmed.strip_suffix('|').unwrap_or(trimmed);
    };

    let mut result = Vec::new();
    let mut pos = 0;
    for cell in inner.split('|') {
        let cell_start = inner_start + pos;
        let cell_trimmed = cell.trim();
        if cell_trimmed.is_empty() {
            // Empty cell — use a zero-length span at the position
            result.push(Span::new(cell_start, cell_start));
        } else {
            let ltrim = cell.len() - cell.trim_start().len();
            let span_start = cell_start + ltrim;
            let span_end = span_start + cell_trimmed.len();
            result.push(Span::new(span_start, span_end));
        }
        pos += cell.len() + 1; // +1 for the | delimiter
    }
    result
}

/// Check if a line is a table separator row (e.g. `|---|---|`).
fn is_separator_row(line: &str) -> bool {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cells: Vec<&str> = inner.split('|').map(|c| c.trim()).collect();
    !cells.is_empty()
        && cells.iter().all(|c| {
            let t = c.trim_matches(':');
            !t.is_empty() && t.chars().all(|ch| ch == '-')
        })
}
