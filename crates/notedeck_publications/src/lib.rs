//! NKBIP-01 Publication Tree Support
//!
//! This crate provides data structures and algorithms for working with
//! Nostr curated publications (kinds 30040 and 30041) as defined in NKBIP-01.
//!
//! # Event Kinds
//!
//! - `30040`: Publication Index - ordered list of references to content events
//! - `30041`: Publication Content - actual readable text sections
//! - `30818`: Wiki Note - wiki-style content (also supported)
//! - `30023`: Long-form Article - markdown content (also supported)
//!
//! # Example
//!
//! ```ignore
//! use notedeck_publications::{PublicationTree, EventAddress};
//!
//! // Create tree from root event
//! let tree = PublicationTree::from_root_note(root_note)?;
//!
//! // Get pending addresses to fetch
//! for addr in tree.pending_addresses() {
//!     // Fetch from relays...
//! }
//!
//! // Iterate over resolved leaves (content sections)
//! for (idx, node) in tree.resolved_leaves() {
//!     println!("{}: {}", idx, node.display_title());
//! }
//! ```

pub mod address;
pub mod constants;
pub mod dtag;
pub mod fetcher;
pub mod node;
pub mod tree;

pub use address::{AddressError, EventAddress};
pub use constants::*;
pub use dtag::{generate_dtag, slugify, title_abbreviation};
pub use fetcher::{FetchState, PublicationFetcher, PublicationRequest};
pub use node::{NodeStatus, NodeType, PublicationTreeNode};
pub use tree::PublicationTree;
