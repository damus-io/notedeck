//! Markdown elements - the stable output of parsing.

/// A complete, stable markdown element ready for rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum MdElement {
    /// Heading with level (1-6) and content
    Heading { level: u8, content: String },

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

    /// Thematic break (---, ***, ___)
    ThematicBreak,

    /// Raw text (when nothing else matches)
    Text(String),
}

/// A fenced code block with optional language.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub language: Option<String>,
    pub content: String,
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
    Text(String),

    /// Styled text (bold, italic, etc.)
    Styled { style: InlineStyle, content: String },

    /// Inline code (`code`)
    Code(String),

    /// Link [text](url)
    Link { text: String, url: String },

    /// Image ![alt](url)
    Image { alt: String, url: String },

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
