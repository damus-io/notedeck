//! Event kind constants for NKBIP-01 publications

/// Publication Index - table of contents referencing content events
pub const KIND_PUBLICATION_INDEX: u32 = 30040;

/// Publication Content - individual sections with readable text
pub const KIND_PUBLICATION_CONTENT: u32 = 30041;

/// Wiki Note - wiki-style content sections
pub const KIND_WIKI_NOTE: u32 = 30818;

/// Long-form Article - markdown content
pub const KIND_LONG_FORM: u32 = 30023;

/// All supported content kinds for publication sections
pub const CONTENT_KINDS: [u32; 3] = [KIND_PUBLICATION_CONTENT, KIND_WIKI_NOTE, KIND_LONG_FORM];

/// Check if a kind is a publication index
pub fn is_index_kind(kind: u32) -> bool {
    kind == KIND_PUBLICATION_INDEX
}

/// Check if a kind can be publication content
pub fn is_content_kind(kind: u32) -> bool {
    CONTENT_KINDS.contains(&kind)
}
