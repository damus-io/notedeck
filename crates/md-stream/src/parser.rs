//! Core streaming parser implementation.

use crate::element::{CodeBlock, MdElement};
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
        self.partial.as_ref().map(|p| p.content.as_str())
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

    /// Process newly added content.
    fn process_new_content(&mut self) {
        while self.process_pos < self.buffer.len() {
            let remaining = self.remaining().to_string();

            // Handle based on current partial state
            let partial_kind = self.partial.as_ref().map(|p| p.kind.clone());
            if let Some(kind) = partial_kind {
                match kind {
                    PartialKind::CodeFence {
                        fence_char,
                        fence_len,
                        ..
                    } => {
                        if self.process_code_fence(fence_char, fence_len, &remaining) {
                            continue;
                        }
                        return; // Need more input
                    }
                    PartialKind::Heading { level } => {
                        if self.process_heading(level, &remaining) {
                            continue;
                        }
                        return;
                    }
                    PartialKind::Paragraph => {
                        // For paragraphs, check if we're at a line start that could be a block element
                        if self.at_line_start {
                            // Take the paragraph partial first — try_block_start may
                            // replace self.partial with the new block element
                            let para_partial = self.partial.take();

                            if let Some(consumed) = self.try_block_start(&remaining) {
                                // Emit the saved paragraph before the new block
                                if let Some(partial) = para_partial {
                                    let trimmed = partial.content.trim();
                                    if !trimmed.is_empty() {
                                        let inline_elements = parse_inline(trimmed);
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
                            if self.could_be_block_start(&remaining) {
                                return;
                            }
                        }
                        // Continue with inline processing
                        if self.process_inline(&remaining) {
                            continue;
                        }
                        return;
                    }
                    _ => {
                        // For other inline elements, process character by character
                        if self.process_inline(&remaining) {
                            continue;
                        }
                        return;
                    }
                }
            }

            // No partial state - detect new elements
            if self.at_line_start {
                if let Some(consumed) = self.try_block_start(&remaining) {
                    self.advance(consumed);
                    continue;
                }
                if self.could_be_block_start(&remaining) {
                    return;
                }
            }

            // Fall back to inline processing
            if self.process_inline(&remaining) {
                continue;
            }
            return;
        }
    }

    /// Check if text could be the start of a block element but we don't
    /// have enough characters to confirm yet. Used to defer consuming
    /// ambiguous prefixes like "`" or "``" that might become "```".
    fn could_be_block_start(&self, text: &str) -> bool {
        let trimmed = text.trim_start();
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

        false
    }

    /// Try to detect a block-level element at line start.
    /// Returns bytes consumed if successful.
    fn try_block_start(&mut self, text: &str) -> Option<usize> {
        let trimmed = text.trim_start();
        let leading_space = text.len() - trimmed.len();

        // Heading: # ## ### etc
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|&c| c == '#').count();
            if level <= 6 {
                if let Some(rest) = trimmed.get(level..) {
                    if rest.starts_with(' ') || rest.is_empty() {
                        self.partial = Some(Partial::new(
                            PartialKind::Heading { level: level as u8 },
                            self.process_pos,
                        ));
                        self.at_line_start = false;
                        return Some(leading_space + level + rest.starts_with(' ') as usize);
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
                    (
                        if lang.is_empty() {
                            None
                        } else {
                            Some(lang.to_string())
                        },
                        nl_pos + 1,
                    )
                } else {
                    // No newline yet - language might be incomplete
                    let lang = after_fence.trim();
                    (
                        if lang.is_empty() {
                            None
                        } else {
                            Some(lang.to_string())
                        },
                        after_fence.len(),
                    )
                };

                self.partial = Some(Partial::new(
                    PartialKind::CodeFence {
                        fence_char,
                        fence_len,
                        language,
                    },
                    self.process_pos,
                ));
                self.at_line_start = false;
                return Some(leading_space + fence_len + consumed_lang);
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

        None
    }

    /// Process content inside a code fence.
    /// Returns true if we should continue processing, false if we need more input.
    fn process_code_fence(&mut self, fence_char: char, fence_len: usize, text: &str) -> bool {
        let partial = self.partial.as_mut().unwrap();

        for line in text.split_inclusive('\n') {
            // Check if we're at a line start within the code fence
            let at_content_line_start =
                partial.content.is_empty() || partial.content.ends_with('\n');

            if at_content_line_start {
                let trimmed = line.trim_start();

                // Check for closing fence
                if trimmed.len() >= fence_len
                    && trimmed.as_bytes().iter().take(fence_len).all(|&b| b == fence_char as u8)
                {
                    let after_fence = &trimmed[fence_len..];
                    if after_fence.trim().is_empty() || after_fence.starts_with('\n') {
                        // Found closing fence! Complete the code block
                        let language =
                            if let PartialKind::CodeFence { language, .. } = &partial.kind {
                                language.clone()
                            } else {
                                None
                            };

                        let content = std::mem::take(&mut partial.content);
                        self.parsed
                            .push(MdElement::CodeBlock(CodeBlock { language, content }));
                        self.partial = None;
                        self.at_line_start = true;

                        // Advance past the closing fence line
                        let consumed = text.find(line).unwrap() + line.len();
                        self.advance(consumed);
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

            // Not a closing fence - add to content
            partial.content.push_str(line);
        }

        // Consumed all available text, need more
        self.advance(text.len());
        false
    }

    /// Process heading content until newline.
    fn process_heading(&mut self, level: u8, text: &str) -> bool {
        if let Some(nl_pos) = text.find('\n') {
            let partial = self.partial.as_mut().unwrap();
            partial.content.push_str(&text[..nl_pos]);

            let content = std::mem::take(&mut partial.content).trim().to_string();
            self.parsed.push(MdElement::Heading { level, content });
            self.partial = None;
            self.at_line_start = true;
            self.advance(nl_pos + 1);
            true
        } else {
            // No newline yet - accumulate
            let partial = self.partial.as_mut().unwrap();
            partial.content.push_str(text);
            self.advance(text.len());
            false
        }
    }

    /// Process inline content.
    fn process_inline(&mut self, text: &str) -> bool {
        // Check for paragraph break split across tokens:
        // partial content ends with \n and new text starts with \n
        if text.starts_with('\n') {
            if let Some(ref partial) = self.partial {
                if partial.content.ends_with('\n') {
                    // Double newline split across token boundary — emit paragraph
                    let para_text = std::mem::take(&mut self.partial.as_mut().unwrap().content);
                    self.partial = None;

                    if !para_text.trim().is_empty() {
                        let inline_elements = parse_inline(para_text.trim());
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                    self.at_line_start = true;
                    self.advance(1); // consume the \n
                    return true;
                }
            }
        }

        if let Some(nl_pos) = text.find("\n\n") {
            // Double newline = paragraph break
            // Combine accumulated partial content with text before \n\n
            let para_text = if let Some(ref mut partial) = self.partial {
                partial.content.push_str(&text[..nl_pos]);
                std::mem::take(&mut partial.content)
            } else {
                text[..nl_pos].to_string()
            };
            self.partial = None;

            if !para_text.trim().is_empty() {
                // Parse inline elements from the full paragraph text
                let inline_elements = parse_inline(para_text.trim());
                self.parsed.push(MdElement::Paragraph(inline_elements));
            }
            self.at_line_start = true;
            self.advance(nl_pos + 2);
            return true;
        }

        if let Some(nl_pos) = text.find('\n') {
            let after_nl = &text[nl_pos + 1..];

            // Check if text after the newline starts a block element (code fence, heading, etc.)
            // If so, emit the current paragraph and let the block parser handle the rest.
            if !after_nl.is_empty() {
                let trimmed_after = after_nl.trim_start();
                let is_block_start = trimmed_after.starts_with("```")
                    || trimmed_after.starts_with("~~~")
                    || trimmed_after.starts_with('#');
                if is_block_start {
                    // Accumulate text before the newline into the paragraph
                    let para_text = if let Some(ref mut partial) = self.partial {
                        partial.content.push_str(&text[..nl_pos]);
                        std::mem::take(&mut partial.content)
                    } else {
                        text[..nl_pos].to_string()
                    };
                    self.partial = None;

                    if !para_text.trim().is_empty() {
                        let inline_elements = parse_inline(para_text.trim());
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                    self.at_line_start = true;
                    self.advance(nl_pos + 1);
                    return true;
                }
            }

            // Single newline - continue accumulating but track position
            if let Some(ref mut partial) = self.partial {
                partial.content.push_str(&text[..=nl_pos]);
            } else {
                // Start accumulating paragraph
                let content = text[..=nl_pos].to_string();
                self.partial = Some(Partial {
                    kind: PartialKind::Paragraph,
                    start_pos: self.process_pos,
                    content,
                });
            }
            self.at_line_start = true;
            self.advance(nl_pos + 1);
            return true;
        }

        // No newline - accumulate
        if let Some(ref mut partial) = self.partial {
            partial.content.push_str(text);
        } else {
            self.partial = Some(Partial {
                kind: PartialKind::Paragraph,
                start_pos: self.process_pos,
                content: text.to_string(),
            });
        }
        self.at_line_start = false;
        self.advance(text.len());
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
                        content: partial.content,
                    }));
                }
                PartialKind::Heading { level } => {
                    self.parsed.push(MdElement::Heading {
                        level,
                        content: partial.content.trim().to_string(),
                    });
                }
                PartialKind::Paragraph => {
                    if !partial.content.trim().is_empty() {
                        let inline_elements = parse_inline(partial.content.trim());
                        self.parsed.push(MdElement::Paragraph(inline_elements));
                    }
                }
                _ => {
                    // Other partial kinds (lists, blockquotes, etc.) - emit as paragraph for now
                    if !partial.content.trim().is_empty() {
                        let inline_elements = parse_inline(partial.content.trim());
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
