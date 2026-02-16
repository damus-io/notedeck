//! Markdown elements - the stable output of parsing.

/// A byte range into the parser's source buffer. Zero-copy reference to content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end);
        Self { start, end }
    }

    pub fn resolve<'a>(&self, buffer: &'a str) -> &'a str {
        &buffer[self.start..self.end]
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }
}

/// A complete, stable markdown element ready for rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum MdElement {
    /// Heading with level (1-6) and content
    Heading { level: u8, content: Span },

    /// Paragraph of text (may contain inline elements)
    Paragraph(Vec<InlineElement>),

    /// Fenced code block
    CodeBlock(CodeBlock),

    /// Blockquote (contains nested elements)
    BlockQuote(Vec<MdElement>),

    /// Unordered list
    UnorderedList(Vec<ListItem>),

    /// Ordered list (starting number)
    OrderedList { start: u32, items: Vec<ListItem> },

    /// Markdown table with headers and data rows
    Table {
        headers: Vec<Span>,
        rows: Vec<Vec<Span>>,
    },

    /// Thematic break (---, ***, ___)
    ThematicBreak,

    /// Raw text (when nothing else matches)
    Text(Span),
}

/// A fenced code block with optional language.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub language: Option<Span>,
    pub content: Span,
}

/// A list item (may contain nested elements).
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub content: Vec<InlineElement>,
    pub nested: Option<Box<MdElement>>, // Nested list
}

/// Inline elements within a paragraph or list item.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineElement {
    /// Plain text
    Text(Span),

    /// Styled text (bold, italic, etc.)
    Styled { style: InlineStyle, content: Span },

    /// Inline code (`code`)
    Code(Span),

    /// Link [text](url)
    Link { text: Span, url: Span },

    /// Image ![alt](url)
    Image { alt: Span, url: Span },

    /// Hard line break
    LineBreak,
}

/// Inline text styles (can be combined).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Bold,
    Italic,
    BoldItalic,
    Strikethrough,
}
