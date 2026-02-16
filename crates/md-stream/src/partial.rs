//! Partial state tracking for incomplete markdown elements.

/// Tracks an in-progress markdown element that might be completed
/// when more tokens arrive.
#[derive(Debug, Clone)]
pub struct Partial {
    /// What kind of element we're building
    pub kind: PartialKind,

    /// Byte offset into the buffer where this element starts
    pub start_pos: usize,

    /// Accumulated content so far (for elements that need it)
    pub content: String,
}

impl Partial {
    pub fn new(kind: PartialKind, start_pos: usize) -> Self {
        Self {
            kind,
            start_pos,
            content: String::new(),
        }
    }
}

/// The kind of partial element being tracked.
#[derive(Debug, Clone, PartialEq)]
pub enum PartialKind {
    /// Fenced code block waiting for closing ```
    /// Stores the fence info (backticks count, language)
    CodeFence {
        fence_char: char, // ` or ~
        fence_len: usize, // typically 3
        language: Option<String>,
    },

    /// Inline code waiting for closing backtick(s)
    InlineCode { backtick_count: usize },

    /// Bold text waiting for closing ** or __
    Bold {
        marker: char, // * or _
    },

    /// Italic text waiting for closing * or _
    Italic { marker: char },

    /// Bold+italic waiting for closing *** or ___
    BoldItalic { marker: char },

    /// Strikethrough waiting for closing ~~
    Strikethrough,

    /// Link: seen [, waiting for ](url)
    /// States: text, post-bracket, url
    Link { state: LinkState, text: String },

    /// Image: seen ![, waiting for ](url)
    Image { state: LinkState, alt: String },

    /// Heading started with # at line start, collecting content
    Heading { level: u8 },

    /// List item started, collecting content
    ListItem {
        ordered: bool,
        number: Option<u32>,
        indent: usize,
    },

    /// Blockquote started with >, collecting content
    BlockQuote { depth: usize },

    /// Paragraph being accumulated (waiting for double newline)
    Paragraph,
}

/// State machine for link/image parsing.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    /// Collecting text between [ and ]
    Text,
    /// Seen ], expecting (
    PostBracket,
    /// Collecting URL between ( and )
    Url(String),
}
