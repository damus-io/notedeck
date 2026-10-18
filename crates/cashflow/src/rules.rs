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
//! *all of*, so a rule becomes one ledgerdb [`Rule`] per pattern, in pattern
//! order, which keeps first-match-wins exactly.
//!
//! `amex.toml`'s top-level `card` and `currency` are the Amex importer's
//! settings, not edits: they are accepted and ignored here. Its `unknown`
//! (the account unmatched merchants go to) is a counter-leg default, so it is
//! added to [`Edits::defaults`]. Any other key is an error, so a typo fails
//! loudly.
//!
//! This file is the local, one-off way in. Rules synced through the vault are
//! headway:cashflow/wrestle-depart-cash, and build on the same [`Rule`]s.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ledgerdb::{Cond, Edits, Field, Rule};
use serde::Deserialize;

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
struct RulesFile {
    defaults: Option<Vec<String>>,
    transfers: Option<String>,
    /// `amex.toml`'s unmatched-merchant account.
    unknown: Option<String>,
    /// `amex.toml`'s card account: the Amex importer's, ignored here.
    #[allow(dead_code)]
    card: Option<String>,
    /// `amex.toml`'s commodity: the Amex importer's, ignored here.
    #[allow(dead_code)]
    currency: Option<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<RuleEntry>,
}

/// One `[[rule]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleEntry {
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

/// Read the rules file at `path` into [`Edits`] with no overrides, splits or
/// links.
pub fn load(path: &Path) -> Result<Edits, RulesError> {
    let text = std::fs::read_to_string(path).map_err(|source| RulesError::Read {
        path: path.to_owned(),
        source,
    })?;
    parse(&text, path)
}

/// [`load`] from text already read; `path` only names it in errors.
pub fn parse(text: &str, path: &Path) -> Result<Edits, RulesError> {
    let file: RulesFile = toml::from_str(text).map_err(|e| RulesError::Parse {
        path: path.to_owned(),
        message: e.message().to_owned(),
    })?;

    let mut edits = default_edits();
    if let Some(defaults) = file.defaults {
        edits.defaults = defaults;
    }
    if let Some(unknown) = file.unknown
        && !edits.defaults.contains(&unknown)
    {
        edits.defaults.push(unknown);
    }
    if let Some(transfers) = file.transfers {
        edits.transfers = transfers;
    }
    for (index, entry) in file.rules.into_iter().enumerate() {
        push_rule(&mut edits.rules, entry, index + 1, path)?;
    }
    Ok(edits)
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

/// Append `entry` as one [`Rule`] per `match` pattern, or one rule when it
/// has none.
fn push_rule(
    rules: &mut Vec<Rule>,
    entry: RuleEntry,
    index: usize,
    path: &Path,
) -> Result<(), RulesError> {
    let mut shared = Vec::new();
    for (key, needle) in entry.meta {
        shared.push(Cond::Contains {
            field: Field::Meta(key),
            needle,
        });
    }
    if let Some(range) = entry.amount {
        if range.min < 0 || range.min > range.max {
            return Err(RulesError::BadRange {
                path: path.to_owned(),
                index,
                account: entry.account,
            });
        }
        shared.push(Cond::Amount {
            commodity: range.commodity,
            min: range.min,
            max: range.max,
        });
    }
    if let Some(source) = entry.source {
        shared.push(Cond::Source(source));
    }

    let rule = |conditions| Rule {
        account: entry.account.clone(),
        conditions,
        layer: entry.layer.clone(),
    };
    if entry.patterns.is_empty() {
        rules.push(rule(shared));
        return Ok(());
    }
    for needle in &entry.patterns {
        // The pattern goes first: ledgerdb indexes a rule by its first
        // `Contains`, so a payee pattern is what narrows the candidates.
        let mut conditions = Vec::with_capacity(shared.len() + 1);
        conditions.push(Cond::Contains {
            field: Field::Payee,
            needle: needle.clone(),
        });
        conditions.extend(shared.iter().cloned());
        rules.push(rule(conditions));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
