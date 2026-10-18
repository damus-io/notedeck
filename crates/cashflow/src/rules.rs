//! The local rules file: `--rules <toml>`, read into ledgerdb's [`Edits`].
//!
//! The format generalizes `amex2ledger`'s `amex.toml` and still reads one
//! unchanged:
//!
//! ```toml
//! # The counter-leg accounts importers post to when they don't know the
//! # category. [default: expenses:unknown, income:unknown, income:zap,
//! # income:lnurl]
//! defaults = ["expenses:unknown", "income:unknown"]
//! # Where linked transfers net out. [default: transfers]
//! transfers = "transfers"
//!
//! [[rule]]
//! account = "expenses:food:coffee"
//! match = ["coffee", "espresso"]       # any one, as a payee substring
//! meta = { raw-city = "montreal" }     # every one, as a substring
//! amount = { min = 100, max = 2000 }   # smallest units, sign ignored
//! source = "amex"                      # the segment's `; source:`
//! layer = "cards"                      # only segments in this layer
//! ```
//!
//! Every rule condition is optional, matching ignores case, and the rules are
//! tried in file order with the first match winning, as in `amex.toml`. A
//! rule's `match` list is *any of* its patterns while everything else is
//! *all of*, so a rule becomes one ledgerdb [`Rule`](ledgerdb::Rule) per
//! pattern, in pattern order, which keeps first-match-wins exactly.
//!
//! `amex.toml`'s top-level `card` and `currency` are the Amex importer's
//! settings, not edits: they are accepted and ignored here. Its `unknown`
//! (the account unmatched merchants go to) is a counter-leg default, so it is
//! added to [`Edits::defaults`]. Any other key is an error, so a typo fails
//! loudly.
//!
//! A file's rules are read as [`FileRule`]s, each of which is the body of a
//! vault rule ([`FileRule::note`]). That is how `cashflow rule import` moves a
//! file into the vault, and why a file and the vault holding the same rules
//! derive the same ledger: both compile through [`RuleNote::push_rules`].
//!
//! This file is the local, one-off way in, and `--rules` still reads one for
//! testing; the rules every machine shares live in the vault
//! ([`crate::vault_rules`]).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ledgerdb::Edits;
use serde::Deserialize;

use crate::vault_rules::{AmountCond, Compiled, MetaNeedle, RuleNote};

/// The counter-leg accounts an importer posts to when it does not know the
/// category, used when a rules file names none. `income:zap` and
/// `income:lnurl` are the `cln` importer's: it knows a payment was a zap or
/// came through a lightning address, but not whose.
pub const DEFAULT_COUNTER_ACCOUNTS: &[&str] = &[
    "expenses:unknown",
    "income:unknown",
    "income:zap",
    "income:lnurl",
];

/// Why a rules file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not valid TOML, or not the rules format.
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    /// A rule's `amount` range is empty or negative.
    #[error("{path}: rule {index} (account {account}): amount needs 0 <= min <= max")]
    BadRange {
        path: PathBuf,
        /// The rule's position in the file, from 1.
        index: usize,
        account: String,
    },
}

/// The file as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    /// The counter-leg accounts, replacing [`DEFAULT_COUNTER_ACCOUNTS`].
    pub defaults: Option<Vec<String>>,
    /// Where linked transfers net out.
    pub transfers: Option<String>,
    /// `amex.toml`'s unmatched-merchant account, added to the defaults.
    pub unknown: Option<String>,
    /// `amex.toml`'s card account: the Amex importer's, ignored here.
    #[allow(dead_code)]
    card: Option<String>,
    /// `amex.toml`'s commodity: the Amex importer's, ignored here.
    #[allow(dead_code)]
    currency: Option<String>,
    /// The `[[rule]]`s, in file order.
    #[serde(default, rename = "rule")]
    pub rules: Vec<FileRule>,
}

/// One `[[rule]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    account: String,
    /// Payee substrings, any one of which matches.
    #[serde(default, rename = "match")]
    patterns: Vec<String>,
    /// Metadata key to a substring its value must contain.
    #[serde(default)]
    meta: BTreeMap<String, String>,
    amount: Option<AmountRange>,
    source: Option<String>,
    layer: Option<String>,
}

/// A rule's `amount = { min, max, commodity }`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AmountRange {
    #[serde(default)]
    min: i64,
    #[serde(default = "i64_max")]
    max: i64,
    commodity: Option<String>,
}

fn i64_max() -> i64 {
    i64::MAX
}

impl FileRule {
    /// The rule as a vault rule's body at `rank`, borrowing from this one.
    pub fn note<'a>(&'a self, rank: &'a str) -> RuleNote<'a> {
        RuleNote {
            patterns: self
                .patterns
                .iter()
                .map(|p| Cow::Borrowed(p.as_str()))
                .collect(),
            meta: self
                .meta
                .iter()
                .map(|(key, needle)| MetaNeedle {
                    key,
                    needle: Cow::Borrowed(needle),
                })
                .collect(),
            amount: self.amount.as_ref().map(|range| AmountCond {
                min: range.min,
                max: range.max,
                commodity: range.commodity.as_deref(),
            }),
            source: self.source.as_deref(),
            layer: self.layer.as_deref(),
            ..RuleNote::new(&self.account, rank)
        }
    }
}

impl RulesFile {
    /// The counter-leg defaults and transfers account the file asks for:
    /// [`default_edits`] with `defaults`, `unknown` and `transfers` applied,
    /// and no rules.
    pub fn base_edits(&self) -> Edits {
        let mut edits = default_edits();
        if let Some(defaults) = &self.defaults {
            edits.defaults = defaults.clone();
        }
        if let Some(unknown) = &self.unknown
            && !edits.defaults.contains(unknown)
        {
            edits.defaults.push(unknown.clone());
        }
        if let Some(transfers) = &self.transfers {
            edits.transfers = transfers.clone();
        }
        edits
    }
}

/// Read the rules file at `path` into [`Edits`] with no overrides, splits or
/// links.
pub fn load(path: &Path) -> Result<Edits, RulesError> {
    parse(&read_text(path)?, path)
}

/// Read the rules file at `path` as written, its rules checked.
pub fn read(path: &Path) -> Result<RulesFile, RulesError> {
    parse_file(&read_text(path)?, path)
}

fn read_text(path: &Path) -> Result<String, RulesError> {
    std::fs::read_to_string(path).map_err(|source| RulesError::Read {
        path: path.to_owned(),
        source,
    })
}

/// [`read`] from text already read; `path` only names it in errors.
pub fn parse_file(text: &str, path: &Path) -> Result<RulesFile, RulesError> {
    let file: RulesFile = toml::from_str(text).map_err(|e| RulesError::Parse {
        path: path.to_owned(),
        message: e.message().to_owned(),
    })?;
    for (index, rule) in file.rules.iter().enumerate() {
        if let Some(range) = &rule.amount
            && (range.min < 0 || range.min > range.max)
        {
            return Err(RulesError::BadRange {
                path: path.to_owned(),
                index: index + 1,
                account: rule.account.clone(),
            });
        }
    }
    Ok(file)
}

/// [`load`] from text already read; `path` only names it in errors.
pub fn parse(text: &str, path: &Path) -> Result<Edits, RulesError> {
    let file = parse_file(text, path)?;
    let notes: Vec<RuleNote> = file.rules.iter().map(|rule| rule.note("")).collect();
    let mut compiled = Compiled::of(&notes);
    let base = file.base_edits();
    compiled.edits.defaults = base.defaults;
    compiled.edits.transfers = base.transfers;
    Ok(compiled.edits)
}

/// The edits with no rules file: the default counter accounts and nothing
/// else.
pub fn default_edits() -> Edits {
    Edits {
        defaults: DEFAULT_COUNTER_ACCOUNTS
            .iter()
            .map(|a| (*a).to_owned())
            .collect(),
        ..Edits::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{Cond, Field};

    fn parse_ok(text: &str) -> Edits {
        parse(text, Path::new("rules.toml")).unwrap()
    }

    #[test]
    fn empty_file_is_the_defaults() {
        let edits = parse_ok("");
        assert_eq!(edits, default_edits());
        assert_eq!(edits.defaults, DEFAULT_COUNTER_ACCOUNTS);
        assert_eq!(edits.transfers, "transfers");
    }

    /// An `amex.toml`-shaped file reads unchanged: `card`/`currency` are
    /// ignored, `unknown` joins the defaults, and each pattern is a rule.
    #[test]
    fn amex_toml_shape_reads() {
        let edits = parse_ok(
            r#"
card = "liabilities:card"
unknown = "expenses:misc"
currency = "CAD"

[[rule]]
account = "expenses:food"
match = ["GROCER", "market"]

[[rule]]
account = "expenses:travel"
match = ["AIRLINE"]
"#,
        );
        assert_eq!(
            edits.defaults,
            [
                "expenses:unknown",
                "income:unknown",
                "income:zap",
                "income:lnurl",
                "expenses:misc"
            ]
        );
        let accounts: Vec<&str> = edits.rules.iter().map(|r| r.account.as_str()).collect();
        assert_eq!(
            accounts,
            ["expenses:food", "expenses:food", "expenses:travel"]
        );
        assert_eq!(
            edits.rules[1].conditions,
            [Cond::Contains {
                field: Field::Payee,
                needle: "market".to_owned()
            }]
        );
    }

    #[test]
    fn extended_conditions_map_to_conds() {
        let edits = parse_ok(
            r#"
defaults = ["expenses:unknown"]
transfers = "equity:transfers"

[[rule]]
account = "income:zaps"
meta = { raw-zap-p = "abc" }
amount = { min = 1000, max = 1000, commodity = "msat" }
source = "cln"
layer = "lightning"
"#,
        );
        assert_eq!(edits.defaults, ["expenses:unknown"]);
        assert_eq!(edits.transfers, "equity:transfers");
        let rule = &edits.rules[0];
        assert_eq!(rule.layer.as_deref(), Some("lightning"));
        assert_eq!(
            rule.conditions,
            [
                Cond::Contains {
                    field: Field::Meta("raw-zap-p".to_owned()),
                    needle: "abc".to_owned()
                },
                Cond::Amount {
                    commodity: Some("msat".to_owned()),
                    min: 1000,
                    max: 1000
                },
                Cond::Source("cln".to_owned()),
            ]
        );
    }

    #[test]
    fn bad_files_name_the_problem() {
        let err = parse("[[rule]]\naccount = \"a\"\nfrob = 1\n", Path::new("r.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("r.toml") && err.contains("frob"), "{err}");

        let err = parse(
            "[[rule]]\naccount = \"a\"\namount = { min = 5, max = 1 }\n",
            Path::new("r.toml"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("rule 1"), "{err}");

        assert!(parse("zzz", Path::new("r.toml")).is_err());
    }
}
