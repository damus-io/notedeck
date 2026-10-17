//! The account tree: interned colon paths with parent links.

use crate::strpool::{StrId, StrPool};

/// The id of an account in an [`Accounts`] tree.
///
/// Ids are dense and assigned in the order accounts are first interned, an
/// ancestor always before its descendants.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(u32);

impl AccountId {
    /// The id's position in its tree, `0` for the first account interned.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why an account name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    /// The name is empty, or has an empty segment (`a::b`, `:a`, `a:`).
    #[error("account name has an empty segment")]
    EmptySegment,
    /// The name holds a tab or two spaces in a row, which in a posting line
    /// separate the account from its amount, so it could not be written back.
    #[error("account name contains a tab or a double space")]
    Separator,
}

/// One account in the tree.
#[derive(Debug, Clone)]
struct Node {
    /// The full name in the tree's pool.
    name: StrId,
    parent: Option<AccountId>,
    /// Byte offset of the last segment in the full name.
    leaf_start: u32,
    /// Segments in the full name: 1 for a top-level account.
    depth: u16,
    children: Vec<AccountId>,
}

/// Every account a ledger uses, as a tree.
///
/// Interning `expenses:food:coffee` also interns `expenses` and
/// `expenses:food`, so every account's parent exists and rollups can walk up
/// the tree. Full names live in a [`StrPool`] whose ids line up with the
/// account ids.
#[derive(Debug, Default)]
pub struct Accounts {
    names: StrPool,
    nodes: Vec<Node>,
    roots: Vec<AccountId>,
}

impl Accounts {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of account `path` (colon-separated, such as
    /// `expenses:food:coffee`), interning it and any missing ancestors.
    ///
    /// # Panics
    ///
    /// If the tree would hold more than `u32::MAX` accounts.
    pub fn intern(&mut self, path: &str) -> Result<AccountId, AccountError> {
        if let Some(id) = self.lookup(path) {
            return Ok(id);
        }
        validate(path)?;

        let ends = path
            .char_indices()
            .filter(|(_, c)| *c == ':')
            .map(|(i, _)| i)
            .chain(std::iter::once(path.len()));
        let mut parent = None;
        let mut leaf_start = 0;
        for end in ends {
            let prefix = &path[..end];
            let id = match self.lookup(prefix) {
                Some(id) => id,
                None => self.push(prefix, parent, leaf_start),
            };
            parent = Some(id);
            leaf_start = end + 1;
        }
        Ok(parent.expect("a validated path has at least one segment"))
    }

    /// Append a new account whose ancestors already exist.
    fn push(&mut self, path: &str, parent: Option<AccountId>, leaf_start: usize) -> AccountId {
        let id = AccountId(u32::try_from(self.nodes.len()).expect("at most u32::MAX accounts"));
        let name = self.names.intern(path);
        // `lookup` relies on the pool and the tree assigning the same index.
        debug_assert_eq!(name.index(), id.index());
        let depth = parent.map_or(1, |p| self.nodes[p.index()].depth + 1);
        self.nodes.push(Node {
            name,
            parent,
            leaf_start: leaf_start as u32,
            depth,
            children: Vec::new(),
        });
        match parent {
            Some(p) => self.nodes[p.index()].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    /// The id of account `path` if it has been interned.
    pub fn lookup(&self, path: &str) -> Option<AccountId> {
        self.names.lookup(path).map(|s| AccountId(s.index() as u32))
    }

    /// The account's full colon-separated name.
    pub fn full_name(&self, id: AccountId) -> &str {
        self.names.get(self.nodes[id.index()].name)
    }

    /// The account's last segment: `coffee` for `expenses:food:coffee`.
    pub fn leaf_name(&self, id: AccountId) -> &str {
        &self.full_name(id)[self.nodes[id.index()].leaf_start as usize..]
    }

    /// The account's parent, `None` for a top-level account.
    pub fn parent(&self, id: AccountId) -> Option<AccountId> {
        self.nodes[id.index()].parent
    }

    /// The account's direct children, in the order they were interned.
    pub fn children(&self, id: AccountId) -> &[AccountId] {
        &self.nodes[id.index()].children
    }

    /// The top-level accounts, in the order they were interned.
    pub fn roots(&self) -> &[AccountId] {
        &self.roots
    }

    /// The number of segments in the account's name: 1 for `expenses`, 3 for
    /// `expenses:food:coffee`.
    pub fn depth(&self, id: AccountId) -> u16 {
        self.nodes[id.index()].depth
    }

    /// The account's ancestors, nearest first, not including itself.
    pub fn ancestors(&self, id: AccountId) -> impl Iterator<Item = AccountId> + '_ {
        std::iter::successors(self.parent(id), |a| self.parent(*a))
    }

    /// Whether `ancestor` is a strict ancestor of `id`: `expenses` is one of
    /// `expenses:food`, but no account is its own ancestor.
    pub fn is_ancestor(&self, ancestor: AccountId, id: AccountId) -> bool {
        // Ancestors are always interned first, so a later id cannot be one.
        ancestor < id && self.ancestors(id).any(|a| a == ancestor)
    }

    /// How many accounts the tree holds, ancestors included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree holds no accounts.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Every account id, ancestors before descendants.
    pub fn ids(&self) -> impl Iterator<Item = AccountId> + use<> {
        (0..self.nodes.len() as u32).map(AccountId)
    }
}

/// Refuse names that have an empty segment or could not be written back.
fn validate(path: &str) -> Result<(), AccountError> {
    if path.split(':').any(str::is_empty) {
        return Err(AccountError::EmptySegment);
    }
    if path.contains('\t') || path.contains("  ") {
        return Err(AccountError::Separator);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_creates_ancestors() {
        let mut accounts = Accounts::new();
        let coffee = accounts.intern("expenses:food:coffee").unwrap();
        assert_eq!(accounts.len(), 3);
        let expenses = accounts.lookup("expenses").unwrap();
        let food = accounts.lookup("expenses:food").unwrap();
        assert_eq!(accounts.parent(coffee), Some(food));
        assert_eq!(accounts.parent(food), Some(expenses));
        assert_eq!(accounts.parent(expenses), None);
        assert_eq!(accounts.depth(expenses), 1);
        assert_eq!(accounts.depth(coffee), 3);
        assert_eq!(accounts.full_name(coffee), "expenses:food:coffee");
        assert_eq!(accounts.leaf_name(coffee), "coffee");
        assert_eq!(accounts.leaf_name(expenses), "expenses");
        assert_eq!(accounts.roots(), [expenses]);
    }

    #[test]
    fn interning_is_idempotent_and_shares_ancestors() {
        let mut accounts = Accounts::new();
        let coffee = accounts.intern("expenses:food:coffee").unwrap();
        let tea = accounts.intern("expenses:food:tea").unwrap();
        let rent = accounts.intern("expenses:rent").unwrap();
        let cash = accounts.intern("assets:cash").unwrap();
        assert_eq!(accounts.intern("expenses:food:coffee"), Ok(coffee));
        assert_eq!(accounts.len(), 7);

        let expenses = accounts.lookup("expenses").unwrap();
        let food = accounts.lookup("expenses:food").unwrap();
        let assets = accounts.lookup("assets").unwrap();
        assert_eq!(accounts.children(food), [coffee, tea]);
        assert_eq!(accounts.children(expenses), [food, rent]);
        assert_eq!(accounts.children(coffee), []);
        assert_eq!(accounts.roots(), [expenses, assets]);
        assert_eq!(accounts.parent(cash), Some(assets));
    }

    #[test]
    fn ancestry() {
        let mut accounts = Accounts::new();
        let coffee = accounts.intern("expenses:food:coffee").unwrap();
        let rent = accounts.intern("expenses:rent").unwrap();
        let expenses = accounts.lookup("expenses").unwrap();
        let food = accounts.lookup("expenses:food").unwrap();

        assert!(accounts.is_ancestor(expenses, coffee));
        assert!(accounts.is_ancestor(food, coffee));
        assert!(accounts.is_ancestor(expenses, rent));
        assert!(!accounts.is_ancestor(coffee, coffee));
        assert!(!accounts.is_ancestor(coffee, expenses));
        assert!(!accounts.is_ancestor(food, rent));
        assert!(!accounts.is_ancestor(rent, coffee));
        let up: Vec<AccountId> = accounts.ancestors(coffee).collect();
        assert_eq!(up, [food, expenses]);
    }

    #[test]
    fn segments_may_hold_single_spaces_and_unicode() {
        let mut accounts = Accounts::new();
        let id = accounts.intern("Expenses:Food Court:Café").unwrap();
        assert_eq!(accounts.leaf_name(id), "Café");
        assert_eq!(accounts.depth(id), 3);
    }

    #[test]
    fn bad_names_are_refused() {
        let mut accounts = Accounts::new();
        for bad in ["", ":", "a:", ":a", "a::b"] {
            assert_eq!(
                accounts.intern(bad),
                Err(AccountError::EmptySegment),
                "{bad:?}"
            );
        }
        for bad in ["a\tb", "a  b", "a:b  c"] {
            assert_eq!(
                accounts.intern(bad),
                Err(AccountError::Separator),
                "{bad:?}"
            );
        }
        assert!(accounts.is_empty());
    }
}
