use enostr::Pubkey;
use std::fmt::Display;

#[derive(Clone, Debug)]
pub enum PubkeySource {
    Explicit(Pubkey),
    DeckAuthor,
}

#[derive(Debug)]
pub enum ListKind {
    Contact(PubkeySource),
}

///
/// What kind of column is it?
///   - Follow List
///   - Notifications
///   - DM
///   - filter
///   - ... etc
#[derive(Debug)]
pub enum ColumnKind {
    List(ListKind),
    Universe,

    /// Generic filter
    Generic,
}

impl Display for ColumnKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            ColumnKind::Generic => f.write_str("Timeline"),
            ColumnKind::Universe => f.write_str("Universe"),
        }
    }
}

impl ColumnKind {
    pub fn contact_list(pk: PubkeySource) -> Self {
        ColumnKind::List(ListKind::Contact(pk))
    }
}
