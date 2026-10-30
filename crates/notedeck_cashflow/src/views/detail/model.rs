//! What the transaction detail says, worked out once per record and load.
//!
//! [`text_of`] turns one [`TxnDetail`] into the strings the detail draws:
//! the hero's payee and amount, each posting with its chart label, why the
//! counter leg went where it did, the transfer it is or could be a side of,
//! and where the record came from. It runs
//! when the books or the record open change, never per frame; the view lays
//! these strings out into galleys and paints those.

use cashflow::Books;
use cashflow::chart::Labels;
use cashflow::detail::{Decision, MetaLine, TxnDetail};
use cashflow::split::LegDraft;
use cashflow::transfers::{self, Suggestion};
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
    /// What the Categorize section starts from.
    pub categorize: CategorizeFacts,
    /// What the Transfer section shows.
    pub transfer: TransferText,
}

/// The transfer a record is, or could be, a side of: the four states of
/// the detail's Transfer section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferText {
    /// It has no counter leg, so it can't be one side of a transfer: no
    /// section.
    NotApplicable,
    /// Nothing in the owner's other accounts matches it.
    Unmatched,
    /// [`cashflow::transfers::suggest`] pairs it with another record.
    Suggested(OtherLeg),
    /// A live link names it, with this other side.
    Linked {
        /// The other side's record id.
        other: String,
    },
    /// An override or split decides it, so it can't be linked.
    Categorized,
}

/// The other side of a suggested transfer, as the section draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherLeg {
    /// The side money left, as the link names it.
    pub a: String,
    /// The side it arrived on.
    pub b: String,
    /// The other side's record id: `a` or `b`, whichever this isn't.
    pub record: String,
    /// Its own account, as a list shows it.
    pub account: String,
    /// The amount on that account.
    pub amount: String,
    /// "Card payment · 2024-10-12".
    pub payee: String,
    /// How it is dated against this one: "2 days earlier", "same day".
    pub gap: String,
}

/// What the detail's Categorize section needs to know of the record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategorizeFacts {
    /// The source its segment came from: what an "all like this" rule is
    /// limited to.
    pub source: String,
    /// Its split's legs as a person would write them again, a positive
    /// spend however the bank signs it; empty unless a split decides it.
    pub legs: Vec<LegDraft>,
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

/// What [`text_of`] works the text out of.
pub struct TextOf<'a> {
    /// The books on screen.
    pub books: &'a Books,
    /// What names their accounts.
    pub labels: &'a Labels,
    /// The record, traced through them.
    pub detail: &'a TxnDetail,
    /// The transfer the triage suggests it is a side of, if any.
    pub suggested: Option<&'a Suggestion>,
    /// What the written time is measured from, in unix seconds.
    pub now: u64,
}

/// The text of one record's detail.
pub fn text_of(of: TextOf<'_>) -> DetailText {
    let TextOf {
        books,
        labels,
        detail,
        suggested,
        now,
    } = of;
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
        categorize: CategorizeFacts {
            source: detail.source.clone(),
            legs: split_drafts(&detail.decided, commodities),
        },
        transfer: transfer_text(books, labels, detail, suggested),
    }
}

/// The Transfer section's state for `detail`, `suggested` the pair the
/// triage found for it.
fn transfer_text(
    books: &Books,
    labels: &Labels,
    detail: &TxnDetail,
    suggested: Option<&Suggestion>,
) -> TransferText {
    match &detail.decided {
        Decision::AsImported => return TransferText::NotApplicable,
        Decision::Transfer { other, .. } => {
            return TransferText::Linked {
                other: other.clone(),
            };
        }
        Decision::Override { .. } | Decision::Split { .. } => return TransferText::Categorized,
        Decision::Unknown { .. } | Decision::Rule { .. } => {}
    }
    let Some(s) = suggested else {
        return TransferText::Unmatched;
    };
    let record = if s.link.a == detail.record {
        &s.link.b
    } else {
        &s.link.a
    };
    let ledger = books.ledger();
    let journal = ledger.journal();
    let Some(txn) = ledger.find(record) else {
        return TransferText::Unmatched;
    };
    let row = journal.txn(txn);
    let own = journal
        .postings_of(row)
        .iter()
        .find(|p| transfers::is_own(journal.accounts.full_name(p.account)));
    TransferText::Suggested(OtherLeg {
        a: s.link.a.clone(),
        b: s.link.b.clone(),
        record: record.clone(),
        account: own.map_or_else(String::new, |p| {
            shown(labels, journal.accounts.full_name(p.account))
        }),
        amount: own.map_or_else(String::new, |p| {
            money(i128::from(p.qty), p.commodity, &journal.commodities)
        }),
        payee: format!("{} · {}", journal.strings.get(row.payee), row.date),
        gap: gap(row.date, detail.date),
    })
}

/// How `other` is dated against `this`: "same day", "1 day later",
/// "3 days earlier".
fn gap(other: Date, this: Date) -> String {
    let (days, way) = if other.days() < this.days() {
        (this.days() - other.days(), "earlier")
    } else {
        (other.days() - this.days(), "later")
    };
    match days {
        0 => "same day".to_owned(),
        1 => format!("1 day {way}"),
        n => format!("{n} days {way}"),
    }
}

/// The legs of `decided`, a split, as [`cashflow::split::legs`] reads
/// them back: each in the counter leg's direction, so a split of money in
/// reads as positive amounts too. Empty for any other decision.
fn split_drafts(decided: &Decision, commodities: &Commodities) -> Vec<LegDraft> {
    let Decision::Split { legs, .. } = decided else {
        return Vec::new();
    };
    // The legs add up to the counter leg, so their sum carries its sign.
    let sum: i128 = legs.iter().map(|l| i128::from(l.amount.qty)).sum();
    let sign = if sum < 0 { -1 } else { 1 };
    legs.iter()
        .map(|l| LegDraft {
            account: l.account.clone(),
            amount: Some(bare(
                l.amount.qty * sign,
                commodities.precision(l.amount.commodity),
            )),
        })
        .collect()
}

/// `qty` smallest units at `precision` decimals, as a bare number with no
/// commodity: `60.00`, `-3`. What a split leg's amount box holds.
fn bare(qty: i64, precision: u8) -> String {
    let sign = if qty < 0 { "-" } else { "" };
    let unit = 10u64.pow(u32::from(precision));
    let magnitude = qty.unsigned_abs();
    let whole = magnitude / unit;
    if precision == 0 {
        return format!("{sign}{whole}");
    }
    let part = magnitude % unit;
    format!(
        "{sign}{whole}.{part:0width$}",
        width = usize::from(precision)
    )
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

    /// A bare amount keeps every decimal and its sign, and no commodity.
    #[test]
    fn bare_writes_the_number_alone() {
        assert_eq!(bare(6000, 2), "60.00");
        assert_eq!(bare(-305, 2), "-3.05");
        assert_eq!(bare(7, 0), "7");
        assert_eq!(bare(5, 3), "0.005");
    }

    /// The gap reads from this record's side, in whole days.
    #[test]
    fn gap_says_which_way() {
        let d = Date::from_days;
        assert_eq!(gap(d(10), d(10)), "same day");
        assert_eq!(gap(d(9), d(10)), "1 day earlier");
        assert_eq!(gap(d(12), d(10)), "2 days later");
    }

    /// The short hash is the hash's head, and a short one whole.
    #[test]
    fn short_hash_keeps_twelve_digits() {
        assert_eq!(short_hash("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_hash("abc"), "abc");
    }
}
