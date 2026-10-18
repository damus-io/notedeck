//! The chart of accounts in the vault: what each account is called, how it
//! is drawn, which project it earns for, and the aliases that restructure
//! the tree.
//!
//! - **Account metadata**, `d = acct:<account>` ([`kinds::ACCOUNT`]): one
//!   note per account, each field its own tag, any of them absent:
//!   `["name", …]` (display name), `["icon", …]` (an emoji), `["color",
//!   "#rrggbb"]`, `["project", …]`, `["hidden", "1"|"0"]` and `["kind", …]`
//!   (an [`AccountKind`] hint for an account whose top-level name doesn't
//!   say). The card asked for JSON content; these are tags because every
//!   vault record is (see [`crate::vault`]).
//! - **Aliases**, `d = alias:<from>` ([`kinds::ALIAS`]), tag `["to", …]`:
//!   rename the subtree at `from` to `to` at derive time, after every rule
//!   and override has decided ([`ledgerdb::Alias`]), so the chart can be
//!   restructured without editing what names the old accounts. An alias is
//!   a per-transaction-style edit ([`EditNote`]): it clears with a
//!   tombstone and carries the agentium session that wrote it.
//!
//! ## Inheritance
//!
//! Metadata inherits down the tree: an account's icon, colour, project,
//! hidden flag and kind are its own if it sets them, else its nearest
//! ancestor's that does ([`Chart::resolve`]). Tagging `income:damus` with
//! project `damus` puts everything under it in the project, and
//! `income:damus:personal` can say otherwise. `["hidden", "0"]` shows an
//! account under a hidden one. The display name is the one field that
//! doesn't inherit: a child named after its parent would only be wrong.
//!
//! Metadata is keyed by the account's name in the *derived* ledger, after
//! aliases: an alias moves the postings, and the metadata describes where
//! they landed.
//!
//! ## Spaces in account names
//!
//! A ledger account may hold a single space (`expenses:coffee shop`), which
//! a vault record id can't. Record ids are the account name with `%` and
//! space escaped ([`record_id`]); the common name with neither is used
//! as-is, so reading one allocates nothing.

use std::borrow::Cow;

use ledgerdb::{AccountId, Accounts, Alias, Edits};
use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use crate::kinds;
use crate::projects::{ProjectRoot, Projects};
use crate::vault::{self, Record, SCHEMA_VERSION, Stored, TagWriter};
use crate::vault_edits::{EditBody, EditError, EditNote, check_account};

/// What an account is, for one whose top-level name doesn't say: the hint a
/// dashboard reads to put it with income, expenses and so on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AccountKind {
    /// Something owned: a bank account, a wallet.
    Asset,
    /// Something owed: a credit card, a loan.
    Liability,
    /// Opening balances and the owner's stake.
    Equity,
    /// Money earned.
    Income,
    /// Money spent.
    Expense,
}

impl AccountKind {
    /// Every kind, in the order a balance sheet lists them.
    pub const ALL: [AccountKind; 5] = [
        AccountKind::Asset,
        AccountKind::Liability,
        AccountKind::Equity,
        AccountKind::Income,
        AccountKind::Expense,
    ];

    /// The kind's tag value: `asset`, `liability`, `equity`, `income`,
    /// `expense`.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountKind::Asset => "asset",
            AccountKind::Liability => "liability",
            AccountKind::Equity => "equity",
            AccountKind::Income => "income",
            AccountKind::Expense => "expense",
        }
    }

    /// The kind [`Self::as_str`] names.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// The kind ledger convention gives `account` by its top-level name
    /// (`assets`, `liabilities`, `equity`, `income` or `revenue`,
    /// `expenses`), or `None` for any other.
    pub fn of_root(account: &str) -> Option<Self> {
        let root = account.split(':').next().unwrap_or(account);
        Some(match root {
            "assets" | "asset" => AccountKind::Asset,
            "liabilities" | "liability" => AccountKind::Liability,
            "equity" => AccountKind::Equity,
            "income" | "revenue" | "revenues" => AccountKind::Income,
            "expenses" | "expense" => AccountKind::Expense,
            _ => return None,
        })
    }
}

/// One account's own metadata, as stored: every field optional, an absent
/// one inherited from the nearest ancestor that sets it (but
/// [`Self::name`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountMeta<'a> {
    /// What to call the account instead of its last name segment.
    pub name: Option<&'a str>,
    /// An emoji drawn beside it.
    pub icon: Option<&'a str>,
    /// Its colour, `#rrggbb`.
    pub color: Option<&'a str>,
    /// The project its subtree earns or spends for.
    pub project: Option<&'a str>,
    /// Whether the account and its subtree are left out of lists.
    pub hidden: Option<bool>,
    /// What the account is, when its top-level name doesn't say.
    pub kind: Option<AccountKind>,
    /// The `agentium:<word-id>` session that wrote this revision, if one did.
    pub agentium: Option<&'a str>,
}

/// A metadata field [`AccountMeta`] holds, by its tag name.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MetaField {
    /// [`AccountMeta::name`].
    Name,
    /// [`AccountMeta::icon`].
    Icon,
    /// [`AccountMeta::color`].
    Color,
    /// [`AccountMeta::project`].
    Project,
    /// [`AccountMeta::hidden`].
    Hidden,
    /// [`AccountMeta::kind`].
    Kind,
}

impl MetaField {
    /// Every field, in the order they are listed.
    pub const ALL: [MetaField; 6] = [
        MetaField::Name,
        MetaField::Icon,
        MetaField::Color,
        MetaField::Project,
        MetaField::Hidden,
        MetaField::Kind,
    ];

    /// The field's tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            MetaField::Name => "name",
            MetaField::Icon => "icon",
            MetaField::Color => "color",
            MetaField::Project => "project",
            MetaField::Hidden => "hidden",
            MetaField::Kind => "kind",
        }
    }

    /// The field [`Self::as_str`] names.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.as_str() == text)
    }
}

/// Why a chart edit can't be stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChartError {
    /// An account name isn't valid.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// A colour that isn't `#rrggbb`.
    #[error("color '{0}' isn't #rrggbb")]
    BadColor(String),
    /// A text field set to nothing, or padded with whitespace.
    #[error("{0} can't be empty or start or end with whitespace")]
    BadText(&'static str),
    /// An alias from an account to itself.
    #[error("an alias renames an account to a different one")]
    AliasSelf,
}

impl<'a> AccountMeta<'a> {
    /// Whether no field is set: the account has no metadata of its own.
    pub fn is_empty(&self) -> bool {
        MetaField::ALL.into_iter().all(|field| !self.has(field))
    }

    /// Whether `field` is set.
    pub fn has(&self, field: MetaField) -> bool {
        match field {
            MetaField::Name => self.name.is_some(),
            MetaField::Icon => self.icon.is_some(),
            MetaField::Color => self.color.is_some(),
            MetaField::Project => self.project.is_some(),
            MetaField::Hidden => self.hidden.is_some(),
            MetaField::Kind => self.kind.is_some(),
        }
    }

    /// Unset `field`, so it is inherited again.
    pub fn clear(&mut self, field: MetaField) {
        match field {
            MetaField::Name => self.name = None,
            MetaField::Icon => self.icon = None,
            MetaField::Color => self.color = None,
            MetaField::Project => self.project = None,
            MetaField::Hidden => self.hidden = None,
            MetaField::Kind => self.kind = None,
        }
    }

    /// Check the metadata of `account` can be stored: the name is a valid
    /// account, the colour is `#rrggbb`, and no text field is blank.
    pub fn check(&self, account: &str) -> Result<(), ChartError> {
        check_account(account)?;
        for (field, text) in [
            ("name", self.name),
            ("icon", self.icon),
            ("project", self.project),
        ] {
            if text.is_some_and(|t| t.is_empty() || t.trim() != t) {
                return Err(ChartError::BadText(field));
            }
        }
        if let Some(color) = self.color {
            let hex = color.strip_prefix('#').unwrap_or("");
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ChartError::BadColor(color.to_owned()));
            }
        }
        Ok(())
    }
}

impl<'a> Record<'a> for AccountMeta<'a> {
    const KIND: u32 = kinds::ACCOUNT;
    const PREFIX: &'static str = "acct";

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        let text = [
            ("name", self.name),
            ("icon", self.icon),
            ("color", self.color),
            ("project", self.project),
        ];
        for (name, value) in text {
            if let Some(value) = value {
                tags = tags.tag(name, &[value]);
            }
        }
        if let Some(hidden) = self.hidden {
            tags = tags.tag("hidden", &[if hidden { "1" } else { "0" }]);
        }
        if let Some(kind) = self.kind {
            tags = tags.tag("type", &[kind.as_str()]);
        }
        if let Some(agentium) = self.agentium {
            tags = tags.tag("agentium", &[agentium]);
        }
        tags
    }

    /// `None` for another schema version. A field that doesn't read (a
    /// kind this reader doesn't know) is left unset rather than losing the
    /// rest of the account's metadata.
    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        if vault::tag_value(tags, "v") != Some(SCHEMA_VERSION) {
            return None;
        }
        let hidden = match vault::tag_value(tags, "hidden") {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        };
        Some(AccountMeta {
            name: vault::tag_value(tags, "name"),
            icon: vault::tag_value(tags, "icon"),
            color: vault::tag_value(tags, "color"),
            project: vault::tag_value(tags, "project"),
            hidden,
            kind: vault::tag_value(tags, "type").and_then(AccountKind::parse),
            agentium: vault::tag_value(tags, "agentium"),
        })
    }
}

/// An alias's body: the name its `from` subtree's root becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AliasBody<'a> {
    /// The new name.
    pub to: &'a str,
}

/// A stored alias; its record id is the escaped `from` ([`record_id`]).
pub type AliasNote<'a> = EditNote<'a, AliasBody<'a>>;

impl<'a> EditBody<'a> for AliasBody<'a> {
    const KIND: u32 = kinds::ALIAS;
    const PREFIX: &'static str = "alias";

    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b> {
        tags.tag("to", &[self.to])
    }

    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        Some(AliasBody {
            to: vault::tag_value(tags, "to")?,
        })
    }

    fn check(&self) -> Result<(), EditError> {
        check_account(self.to)
    }
}

/// Check an alias from `from` to `to` can be stored: both are valid
/// accounts, and different.
pub fn check_alias(from: &str, to: &str) -> Result<(), ChartError> {
    check_account(from)?;
    check_account(to)?;
    if from == to {
        return Err(ChartError::AliasSelf);
    }
    Ok(())
}

/// The vault record id of account `account`: its name with `%` and space
/// escaped, since a record id holds no whitespace. Borrowed when there is
/// nothing to escape.
pub fn record_id(account: &str) -> Cow<'_, str> {
    if !account.contains([' ', '%']) {
        return Cow::Borrowed(account);
    }
    Cow::Owned(account.replace('%', "%25").replace(' ', "%20"))
}

/// The account name a [`record_id`] stands for.
pub fn account_of(record_id: &str) -> Cow<'_, str> {
    if !record_id.contains('%') {
        return Cow::Borrowed(record_id);
    }
    Cow::Owned(record_id.replace("%20", " ").replace("%25", "%"))
}

/// `path` and each of its ancestors' names, nearest first:
/// `a:b:c`, `a:b`, `a`.
fn self_and_ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |p| p.rfind(':').map(|at| &p[..at]))
}

/// A field's value as an account sees it, and the account it came from:
/// itself, or the nearest ancestor that sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inherited<'a, T> {
    /// The value.
    pub value: T,
    /// The record id of the account that sets it.
    pub from: &'a str,
}

/// An account's metadata with inheritance applied ([`Chart::resolve`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resolved<'a> {
    /// Its own display name; names don't inherit.
    pub name: Option<&'a str>,
    /// The emoji drawn beside it.
    pub icon: Option<Inherited<'a, &'a str>>,
    /// Its colour, `#rrggbb`.
    pub color: Option<Inherited<'a, &'a str>>,
    /// The project it earns or spends for.
    pub project: Option<Inherited<'a, &'a str>>,
    /// Whether it is left out of lists.
    pub hidden: Option<Inherited<'a, bool>>,
    /// Its kind hint; [`Chart::kind_of`] falls back to its top-level name.
    pub kind: Option<Inherited<'a, AccountKind>>,
}

impl Resolved<'_> {
    /// Whether the account is hidden, itself or by an ancestor.
    pub fn is_hidden(&self) -> bool {
        self.hidden.is_some_and(|h| h.value)
    }
}

/// One project's total in a [`Chart::by_project`] rollup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectTotal<'a> {
    /// The project, or `None` for what no project claims.
    pub project: Option<&'a str>,
    /// The sum, in whatever the caller's amount measured.
    pub amount: i128,
}

/// The vault's chart of accounts: every account's own metadata and every
/// alias, cleared ones included, each sorted by record id.
pub struct Chart<'a> {
    /// The accounts' metadata, by record id.
    pub accounts: Vec<Stored<'a, AccountMeta<'a>>>,
    /// The aliases, by record id (the escaped `from`).
    pub aliases: Vec<Stored<'a, AliasNote<'a>>>,
}

impl<'a> Chart<'a> {
    /// Fold `author`'s account and alias notes, borrowed from `txn`.
    pub fn load(ndb: &Ndb, txn: &'a Transaction, author: &Pubkey) -> Self {
        Chart {
            accounts: vault::latest_all(ndb, txn, author),
            aliases: vault::latest_all(ndb, txn, author),
        }
    }

    /// No metadata and no aliases.
    pub fn empty() -> Self {
        Chart {
            accounts: Vec::new(),
            aliases: Vec::new(),
        }
    }

    /// Account `account`'s own metadata note, if it has one.
    pub fn meta(&self, account: &str) -> Option<&Stored<'a, AccountMeta<'a>>> {
        self.meta_at(&record_id(account))
    }

    /// The metadata note at record id `id`.
    fn meta_at(&self, id: &str) -> Option<&Stored<'a, AccountMeta<'a>>> {
        let at = self.accounts.binary_search_by(|s| s.id.cmp(id)).ok()?;
        Some(&self.accounts[at])
    }

    /// Alias `from`'s note, live or cleared, if it has one.
    pub fn alias(&self, from: &str) -> Option<&Stored<'a, AliasNote<'a>>> {
        let id = record_id(from);
        let at = self
            .aliases
            .binary_search_by(|s| s.id.cmp(id.as_ref()))
            .ok()?;
        Some(&self.aliases[at])
    }

    /// Account `account`'s metadata with inheritance applied: each field its
    /// own, else its nearest ancestor's.
    pub fn resolve(&self, account: &str) -> Resolved<'a> {
        let id = record_id(account);
        let mut out = Resolved {
            name: self.meta_at(&id).and_then(|m| m.body.name),
            ..Resolved::default()
        };
        for at in self_and_ancestors(&id) {
            let Some(stored) = self.meta_at(at) else {
                continue;
            };
            let meta = &stored.body;
            let from = stored.id;
            fill(&mut out.icon, meta.icon, from);
            fill(&mut out.color, meta.color, from);
            fill(&mut out.project, meta.project, from);
            fill(&mut out.hidden, meta.hidden, from);
            fill(&mut out.kind, meta.kind, from);
        }
        out
    }

    /// The project `account` is in, its own or inherited.
    pub fn project_of(&self, account: &str) -> Option<&'a str> {
        self.resolve(account).project.map(|p| p.value)
    }

    /// The project attribution, owned, so a view can keep it past the
    /// transaction this chart borrows from: every account that sets a
    /// project, by its account name (unescaped from its record id).
    pub fn projects(&self) -> Projects {
        Projects::new(self.accounts.iter().filter_map(|stored| {
            Some(ProjectRoot {
                account: account_of(stored.id).into_owned(),
                project: stored.body.project?.to_owned(),
            })
        }))
    }

    /// What `account` is: its kind hint, its own or inherited, else what its
    /// top-level name says.
    pub fn kind_of(&self, account: &str) -> Option<AccountKind> {
        self.resolve(account)
            .kind
            .map(|k| k.value)
            .or_else(|| AccountKind::of_root(account))
    }

    /// Append the live aliases to `edits`, in record-id order, so every
    /// machine holding the same notes derives the same ledger.
    pub fn apply(&self, edits: &mut Edits) {
        edits.aliases.extend(self.aliases.iter().filter_map(|s| {
            let body = s.body.body.as_ref()?;
            Some(Alias {
                from: account_of(s.id).into_owned(),
                to: body.to.to_owned(),
            })
        }));
    }

    /// `under`'s subtree in `accounts` summed by project: `amount` gives
    /// each account's subtree total (a balance, a flow over a range), and
    /// each project gets its accounts' share, less any sub-subtree another
    /// project claims. What no project claims is the `None` row. Largest
    /// magnitude first, then by project; projects summing to zero are left
    /// out.
    ///
    /// `amount` is called once per account where the project changes, not
    /// once per account.
    pub fn by_project(
        &self,
        accounts: &Accounts,
        under: AccountId,
        mut amount: impl FnMut(AccountId) -> i128,
    ) -> Vec<ProjectTotal<'a>> {
        let mut out: Vec<ProjectTotal<'a>> = Vec::new();
        let mut add = |project: Option<&'a str>, qty: i128| match out
            .iter_mut()
            .find(|t| t.project == project)
        {
            Some(total) => total.amount += qty,
            None => out.push(ProjectTotal {
                project,
                amount: qty,
            }),
        };
        let top = accounts.full_name(under);
        add(self.project_of(top), amount(under));
        // A project changes only at an account that sets one, so those are
        // the only roots to visit. Each takes its subtree from the project
        // around it.
        for stored in &self.accounts {
            let Some(project) = stored.body.project else {
                continue;
            };
            let name = account_of(stored.id);
            let Some(root) = accounts.lookup(&name) else {
                continue;
            };
            if !accounts.is_ancestor(under, root) {
                continue;
            }
            let around = accounts
                .parent(root)
                .and_then(|p| self.project_of(accounts.full_name(p)));
            if around == Some(project) {
                continue;
            }
            let qty = amount(root);
            add(Some(project), qty);
            add(around, -qty);
        }
        out.retain(|t| t.amount != 0);
        out.sort_by(|a, b| {
            b.amount
                .unsigned_abs()
                .cmp(&a.amount.unsigned_abs())
                .then(a.project.cmp(&b.project))
        });
        out
    }
}

/// Set `slot` to `value` from account `from` unless a nearer account already
/// set it.
fn fill<'a, T>(slot: &mut Option<Inherited<'a, T>>, value: Option<T>, from: &'a str) {
    if slot.is_none()
        && let Some(value) = value
    {
        *slot = Some(Inherited { value, from });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use crate::vault::Vault;
    use ledgerdb::{Journal, JournalStore, LayerFilter, Ledger, SegmentHeader, derive};
    use nostrdb::Filter;

    const SECRET: [u8; 32] = [11u8; 32];

    fn stored<'a>(id: &'a str, body: AccountMeta<'a>) -> Stored<'a, AccountMeta<'a>> {
        Stored {
            id,
            created_at: 1,
            body,
        }
    }

    fn project(project: &str) -> AccountMeta<'_> {
        AccountMeta {
            project: Some(project),
            ..AccountMeta::default()
        }
    }

    /// A chart from `(account, meta)` pairs, sorted as a load sorts them.
    fn chart<'a>(mut accounts: Vec<Stored<'a, AccountMeta<'a>>>) -> Chart<'a> {
        accounts.sort_by(|a, b| a.id.cmp(b.id));
        Chart {
            accounts,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn metadata_inherits_from_the_nearest_ancestor() {
        let chart = chart(vec![
            stored(
                "expenses",
                AccountMeta {
                    icon: Some("💸"),
                    color: Some("#aa0000"),
                    name: Some("Spending"),
                    ..AccountMeta::default()
                },
            ),
            stored(
                "expenses:food",
                AccountMeta {
                    icon: Some("🍔"),
                    hidden: Some(true),
                    ..AccountMeta::default()
                },
            ),
            stored(
                "expenses:food:shared",
                AccountMeta {
                    hidden: Some(false),
                    kind: Some(AccountKind::Liability),
                    ..AccountMeta::default()
                },
            ),
        ]);

        let coffee = chart.resolve("expenses:food:coffee");
        assert_eq!(coffee.name, None, "names don't inherit");
        let icon = coffee.icon.unwrap();
        assert_eq!((icon.value, icon.from), ("🍔", "expenses:food"));
        let color = coffee.color.unwrap();
        assert_eq!((color.value, color.from), ("#aa0000", "expenses"));
        assert!(coffee.is_hidden());
        assert_eq!(
            chart.kind_of("expenses:food:coffee"),
            Some(AccountKind::Expense)
        );

        let shared = chart.resolve("expenses:food:shared:rent");
        assert!(!shared.is_hidden(), "a nearer hidden=0 shows it again");
        assert_eq!(
            chart.kind_of("expenses:food:shared:rent"),
            Some(AccountKind::Liability)
        );

        assert_eq!(chart.resolve("expenses").name, Some("Spending"));
        // `expensesx` isn't under `expenses`: prefixes stop at a colon.
        assert_eq!(chart.resolve("expensesx:a"), Resolved::default());
        assert_eq!(chart.kind_of("misc"), None);
    }

    #[test]
    fn record_ids_escape_spaces_and_percents() {
        assert!(matches!(record_id("expenses:food"), Cow::Borrowed(_)));
        assert_eq!(record_id("expenses:coffee shop"), "expenses:coffee%20shop");
        assert_eq!(record_id("a:50% off"), "a:50%25%20off");
        for name in ["expenses:coffee shop", "a:50% off", "a:%20"] {
            assert_eq!(account_of(&record_id(name)), name);
        }
        let chart = chart(vec![stored("expenses:coffee%20shop", project("p"))]);
        assert_eq!(chart.project_of("expenses:coffee shop:latte"), Some("p"));
    }

    #[test]
    fn metadata_is_checked_before_it_is_written() {
        let ok = AccountMeta {
            color: Some("#00ff7F"),
            ..project("damus")
        };
        assert_eq!(ok.check("income:damus"), Ok(()));
        assert!(matches!(
            ok.check("income::damus"),
            Err(ChartError::Edit(EditError::BadAccount { .. }))
        ));
        let bad_color = AccountMeta {
            color: Some("red"),
            ..AccountMeta::default()
        };
        assert!(matches!(bad_color.check("a"), Err(ChartError::BadColor(_))));
        assert_eq!(
            project(" p").check("a"),
            Err(ChartError::BadText("project"))
        );
        assert_eq!(check_alias("a:b", "a:b"), Err(ChartError::AliasSelf));
        assert!(check_alias("a:b", "a:c").is_ok());
    }

    /// Sum a toy ledger's subtree totals by project: the project of a
    /// nested root comes out of the project around it.
    #[test]
    fn by_project_rolls_each_subtree_up_into_its_project() {
        let mut accounts = Accounts::new();
        // Direct totals per account; a subtree total is the sum below it.
        let direct = [
            ("income:damus:zaps", -100i128),
            ("income:damus:purple", -50),
            ("income:damus:zaps:personal", -7),
            ("income:salary", -1000),
            ("income:other", -3),
            ("income:notedeck:empty", 0),
        ];
        for (name, _) in direct {
            accounts.intern(name).unwrap();
        }
        let subtree = |id: AccountId| -> i128 {
            direct
                .iter()
                .filter(|(name, _)| {
                    let a = accounts.lookup(name).unwrap();
                    a == id || accounts.is_ancestor(id, a)
                })
                .map(|(_, qty)| qty)
                .sum()
        };
        let chart = chart(vec![
            stored("income:damus", project("damus")),
            stored("income:damus:zaps:personal", project("personal")),
            stored("income:salary", project("personal")),
            // Same as around it: not a new root.
            stored("income:damus:purple", project("damus")),
            stored("income:notedeck", project("notedeck")),
            // Outside `income`: not counted.
            stored("expenses:damus", project("damus")),
        ]);
        let income = accounts.lookup("income").unwrap();
        let mut calls = 0;
        let totals = chart.by_project(&accounts, income, |id| {
            calls += 1;
            subtree(id)
        });
        assert_eq!(
            totals,
            [
                ProjectTotal {
                    project: Some("personal"),
                    amount: -1007
                },
                ProjectTotal {
                    project: Some("damus"),
                    amount: -150
                },
                ProjectTotal {
                    project: None,
                    amount: -3
                },
            ]
        );
        let sum: i128 = totals.iter().map(|t| t.amount).sum();
        assert_eq!(sum, subtree(income));
        assert_eq!(calls, 5, "the top and each root that changes project");
    }

    /// The owned attribution puts every account in the project the chart
    /// does, an escaped record id included.
    #[test]
    fn projects_agree_with_project_of() {
        let chart = chart(vec![
            stored("income:damus", project("damus")),
            stored("income:damus:zaps:personal", project("personal")),
            stored("income:coffee%20shop", project("cafe")),
            stored("income:salary", AccountMeta::default()),
        ]);
        let projects = chart.projects();
        assert_eq!(projects.roots().len(), 3);
        for account in [
            "income",
            "income:damus",
            "income:damus:zaps",
            "income:damus:zaps:personal:tips",
            "income:coffee shop:tips",
            "income:salary",
        ] {
            assert_eq!(
                projects.project_of(account),
                chart.project_of(account),
                "{account}"
            );
        }
    }

    /// Account metadata and aliases survive the vault; a live alias renames
    /// in the derive and a cleared one doesn't.
    #[tokio::test]
    async fn metadata_and_aliases_round_trip_and_aliases_derive() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let kinds = [kinds::ACCOUNT, kinds::ALIAS].map(u64::from);
        let sub = ndb
            .subscribe(&[Filter::new().kinds(kinds).build()])
            .unwrap();

        let meta = AccountMeta {
            name: Some("Damus"),
            icon: Some("🦩"),
            color: Some("#8256dd"),
            project: Some("damus"),
            hidden: Some(false),
            kind: Some(AccountKind::Income),
            agentium: Some("agentium:oak-flat-lamp"),
        };
        vault.write(ndb, "income:damus", &meta, 0).unwrap();
        let shop = record_id("expenses:coffee shop");
        vault
            .write(
                ndb,
                &shop,
                &AccountMeta {
                    icon: Some("☕"),
                    ..AccountMeta::default()
                },
                0,
            )
            .unwrap();
        let food = AliasNote::new(
            AliasBody {
                to: "expenses:groceries",
            },
            None,
        );
        vault.write(ndb, "expenses:food", &food, 0).unwrap();
        let gone = AliasNote::cleared(None);
        vault.write(ndb, "expenses:old", &gone, 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 4, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let chart = Chart::load(ndb, &txn, vault.author());
        assert_eq!(chart.meta("income:damus").unwrap().body, meta);
        assert_eq!(
            chart.resolve("expenses:coffee shop").icon.map(|i| i.value),
            Some("☕")
        );
        assert_eq!(chart.alias("expenses:food").unwrap().body, food);
        assert!(chart.alias("expenses:old").unwrap().body.body.is_none());

        let mut edits = crate::rules::default_edits();
        chart.apply(&mut edits);
        assert_eq!(
            edits.aliases,
            [Alias {
                from: "expenses:food".to_owned(),
                to: "expenses:groceries".to_owned()
            }]
        );

        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        let text =
            "2024-01-02 * Grocer\n  ; id: t:1\n  assets:bank  -4.00 CAD\n  expenses:food:produce\n";
        store
            .write_segment(&header, &Journal::parse(text).journal)
            .unwrap();
        let raw = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        let derived = derive(&raw, &edits);
        let journal = derived.ledger().journal();
        let produce = journal
            .accounts
            .lookup("expenses:groceries:produce")
            .unwrap();
        assert_eq!(derived.ledger().postings_to(produce).count(), 1);
    }
}
