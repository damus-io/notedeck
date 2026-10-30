//! What the transaction detail says, worked out once per record and load.
//!
//! [`text_of`] turns one [`TxnDetail`] into the strings the detail draws:
//! the hero's payee and amount, each posting with its chart label, why the
//! counter leg went where it did, and where the record came from. It runs
//! when the books or the record open change, never per frame; the view lays
//! these strings out into galleys and paints those.

use cashflow::Books;
use cashflow::chart::Labels;
use cashflow::detail::{Decision, MetaLine, TxnDetail};
use ledgerdb::{Amount, Commodities, Date};

use crate::money::money;
use crate::views::transactions::model::shown;

/// Seconds in a day, for the segment's import date.
const DAY: u64 = 86_400;

/// Which tier of derive decided the counter leg: the chip beside the payee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// No counter leg to decide.
    AsImported,
    /// Nothing did: it sits on the importer's default.
    Unknown,
    /// A rule.
    Rule,
    /// An override on this record.
    Override,
    /// A split of this record.
    Split,
    /// A transfer link.
    Transfer,
}

impl Tier {
    /// The tier deciding `decision`.
    fn of(decision: &Decision) -> Self {
        match decision {
            Decision::AsImported => Tier::AsImported,
            Decision::Unknown { .. } => Tier::Unknown,
            Decision::Rule { .. } => Tier::Rule,
            Decision::Override { .. } => Tier::Override,
            Decision::Split { .. } => Tier::Split,
            Decision::Transfer { .. } => Tier::Transfer,
        }
    }

    /// Its chip's text.
    pub fn label(self) -> &'static str {
        match self {
            Tier::AsImported => "As imported",
            Tier::Unknown => "Unknown",
            Tier::Rule => "Rule",
            Tier::Override => "Override",
            Tier::Split => "Split",
            Tier::Transfer => "Transfer",
        }
    }
}

/// Everything the detail draws for one record, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailText {
    /// The record id.
    pub record: String,
    /// "payee · date": the crumb, and the history entry's title.
    pub title: String,
    /// The payee.
    pub payee: String,
    /// "2024-10-02 · card".
    pub subtitle: String,
    /// What decided the counter leg.
    pub tier: Tier,
    /// The counter legs' amount, in their own commodity.
    pub amount: String,
    /// Whether money came in (the counter legs are negative).
    pub incoming: bool,
    /// The same in the home commodity and the rate, when it is another
    /// commodity: "≈ 61.20 CAD at 1.3600".
    pub home: Option<String>,
    /// Every posting, in the order written.
    pub postings: Vec<PostingText>,
    /// Why the counter leg went where it did.
    pub why: WhyText,
    /// Where the record came from.
    pub source: SourceText,
    /// The transaction's metadata, the importer's `raw-*` among it.
    pub meta: Vec<MetaLine>,
    /// Its plain comments.
    pub comments: Vec<String>,
}

/// One posting, as the detail lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingText {
    /// The account as a list shows it: icon and display name, else its
    /// full name.
    pub account: String,
    /// Its full name, under a display name that differs.
    pub full: Option<String>,
    /// The amount in its own commodity.
    pub amount: String,
    /// The same in the home commodity; empty without a rate, or when it is
    /// the home commodity already.
    pub home: String,
    /// Whether it sits on the default account nothing has moved it off.
    pub unknown: bool,
}

/// Why the counter leg went where it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhyText {
    /// "Categorized by rule oak-flat-lamp".
    pub headline: String,
    /// What the deciding edit says, a line each: the rule's patterns, the
    /// override's account, the split's legs.
    pub lines: Vec<String>,
    /// "Written 3 min ago", when a vault note decided it.
    pub written: Option<String>,
    /// The `agentium:<word-id>` session that wrote that note, if one did.
    pub agentium: Option<String>,
    /// This record's edits that derive refused, each with why.
    pub rejected: Vec<String>,
}

/// Where a record came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceText {
    /// "card · layer banks".
    pub origin: String,
    /// "segment 1a2b3c4d5e6f · imported 2024-10-01".
    pub segment: String,
}

/// The text of `detail`, out of `books`, its accounts named by `labels`;
/// `now` (unix seconds) is what the written time is measured from.
pub fn text_of(books: &Books, labels: &Labels, detail: &TxnDetail, now: u64) -> DetailText {
    let commodities = &books.ledger().journal().commodities;
    let home = books.home();
    let default = match &detail.decided {
        Decision::Unknown { default } => Some(default.as_str()),
        _ => None,
    };
    let postings = detail
        .postings
        .iter()
        .map(|p| {
            let account = shown(labels, &p.account);
            let full = (account != p.account).then(|| p.account.clone());
            let home_text = match (p.home, home) {
                (Some(h), Some(home)) if p.amount.commodity != home => {
                    h.display(commodities).to_string()
                }
                _ => String::new(),
            };
            PostingText {
                account,
                full,
                amount: p.amount.display(commodities).to_string(),
                home: home_text,
                unknown: default == Some(p.account.as_str()),
            }
        })
        .collect();
    let hero = Hero::of(detail, commodities, home);
    DetailText {
        record: detail.record.clone(),
        title: title(&detail.payee, detail.date),
        payee: detail.payee.clone(),
        subtitle: format!("{} · {}", detail.date, detail.source),
        tier: Tier::of(&detail.decided),
        amount: hero.amount,
        incoming: hero.incoming,
        home: hero.home,
        postings,
        why: why_text(detail, commodities, now),
        source: SourceText {
            origin: format!("{} · layer {}", detail.source, detail.layer),
            segment: format!(
                "segment {} · imported {}",
                short_hash(&detail.segment.to_string()),
                Date::from_days(u32::try_from(detail.imported / DAY).unwrap_or(u32::MAX))
            ),
        },
        meta: detail.meta.clone(),
        comments: detail.comments.clone(),
    }
}

/// "payee · date", what a detail's history entry is titled.
pub fn title(payee: &str, date: Date) -> String {
    format!("{payee} · {date}")
}

/// The hero's figure: what the counter legs moved.
struct Hero {
    amount: String,
    incoming: bool,
    home: Option<String>,
}

impl Hero {
    /// The counter legs of `detail` (the ones off the owner's own accounts,
    /// else the last), summed in the first one's commodity as the register
    /// sums them, and in `home` when that is another commodity.
    fn of(
        detail: &TxnDetail,
        commodities: &Commodities,
        home: Option<ledgerdb::CommodityId>,
    ) -> Self {
        let counters = || detail.postings.iter().filter(|p| !p.own);
        let Some(first) = counters().next().or(detail.postings.last()) else {
            return Hero {
                amount: String::new(),
                incoming: false,
                home: None,
            };
        };
        let commodity = first.amount.commodity;
        let same = |p: &&cashflow::detail::PostingLine| p.amount.commodity == commodity;
        let (qty, home_qty): (i128, Option<i128>) = if counters().next().is_some() {
            (
                counters()
                    .filter(same)
                    .map(|p| i128::from(p.amount.qty))
                    .sum(),
                counters()
                    .filter(same)
                    .map(|p| p.home.map(|h| i128::from(h.qty)))
                    .sum(),
            )
        } else {
            (
                i128::from(first.amount.qty),
                first.home.map(|h| i128::from(h.qty)),
            )
        };
        let home = match (home, home_qty) {
            (Some(home), Some(home_qty)) if home != commodity => {
                let shown = money(home_qty, home, commodities);
                Some(match rate(qty, commodity, home_qty, home, commodities) {
                    Some(rate) => format!("≈ {shown} at {rate:.4}"),
                    None => format!("≈ {shown}"),
                })
            }
            _ => None,
        };
        Hero {
            amount: money(qty, commodity, commodities),
            incoming: qty < 0,
            home,
        }
    }
}

/// How many of `home` one of `commodity` was worth: `home_qty` over `qty`,
/// each in whole units.
fn rate(
    qty: i128,
    commodity: ledgerdb::CommodityId,
    home_qty: i128,
    home: ledgerdb::CommodityId,
    commodities: &Commodities,
) -> Option<f64> {
    if qty == 0 {
        return None;
    }
    let units = |q: i128, c| q as f64 / 10f64.powi(i32::from(commodities.precision(c)));
    Some(units(home_qty, home) / units(qty, commodity))
}

/// Why `detail`'s counter leg went where it did.
fn why_text(detail: &TxnDetail, commodities: &Commodities, now: u64) -> WhyText {
    let amount = |a: Amount| a.display(commodities).to_string();
    let (headline, lines) = match &detail.decided {
        Decision::AsImported => (
            "As imported".to_owned(),
            vec!["It has no counter leg to categorize.".to_owned()],
        ),
        Decision::Unknown { default } => (
            "Uncategorized".to_owned(),
            vec![format!(
                "Nothing has categorized it, so it stays on {default}."
            )],
        ),
        Decision::Rule {
            id,
            account,
            patterns,
            ..
        } => {
            let headline = match id {
                Some(id) => format!("Categorized by rule {id}"),
                None => "Categorized by a rules-file rule".to_owned(),
            };
            let mut lines = vec![format!("Sends it to {account}")];
            if !patterns.is_empty() {
                let quoted: Vec<String> = patterns.iter().map(|p| format!("“{p}”")).collect();
                lines.push(format!("when the payee contains {}", quoted.join(" or ")));
            }
            (headline, lines)
        }
        Decision::Override { account, .. } => (
            "Categorized by an override".to_owned(),
            vec![format!("This one transaction goes to {account}")],
        ),
        Decision::Split { legs, .. } => (
            format!("Split into {} parts", legs.len()),
            legs.iter()
                .map(|l| format!("{}  {}", l.account, amount(l.amount)))
                .collect(),
        ),
        Decision::Transfer { other, .. } => (
            "A transfer between your accounts".to_owned(),
            vec![format!("Linked with {other}")],
        ),
    };
    let written = detail.decided.written();
    WhyText {
        headline,
        lines,
        written: written.map(|w| format!("Written {}", ago(w.at, now))),
        agentium: written.and_then(|w| w.agentium.clone()),
        rejected: detail.rejected.clone(),
    }
}

/// How long before `now` the moment `at` was (both unix seconds), as a
/// person says it: "just now", "5 min ago", "3 h ago", "12 days ago".
pub fn ago(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..DAY => format!("{} h ago", secs / 3_600),
        _ => {
            let days = secs / DAY;
            if days == 1 {
                "1 day ago".to_owned()
            } else {
                format!("{days} days ago")
            }
        }
    }
}

/// The first twelve hex digits of a segment's hash, enough to tell it from
/// the others.
fn short_hash(hex: &str) -> &str {
    hex.get(..12).unwrap_or(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each band of [`ago`] reads as a person would say it.
    #[test]
    fn ago_reads_in_the_largest_whole_unit() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 159), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 3 * 3_600 + 5), "3 h ago");
        assert_eq!(ago(0, DAY), "1 day ago");
        assert_eq!(ago(0, 12 * DAY), "12 days ago");
        // A clock behind the note's reads as just written.
        assert_eq!(ago(500, 100), "just now");
    }

    /// The short hash is the hash's head, and a short one whole.
    #[test]
    fn short_hash_keeps_twelve_digits() {
        assert_eq!(short_hash("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_hash("abc"), "abc");
    }
}
