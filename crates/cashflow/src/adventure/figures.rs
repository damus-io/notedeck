//! What each month did, gathered once before XP is worked out from it.
//!
//! Every figure is read from the derived books and the vault edits behind
//! them, never from a clock or a machine, so two machines holding the same
//! journal and notes gather the same figures.

use std::collections::BTreeSet;

use ledgerdb::{Bucket, CommodityId, DateRange, TxnId, Why};

use crate::net::{self, INCOME, NetFlow};
use crate::projects::Projects;
use crate::zone::Zone;
use crate::{Books, DeriveInputs};

use super::Month;

/// The commodity Stacking counts.
pub const BTC: &str = "BTC";

/// The top-level account Stacking counts inflows to.
pub const ASSETS: &str = "assets";

/// Satoshis in one bitcoin.
const SATS_PER_BTC: i128 = 100_000_000;

/// What a month's Bookkeeping XP is for: vault notes written that month.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bookkeeping {
    /// Transactions an override or a split categorizes.
    pub triaged: u32,
    /// Transactions a vault rule categorizes, at most
    /// [`super::XpTuning::rule_cap`] per rule.
    pub ruled: u32,
    /// Transactions a transfer link confirms.
    pub linked: u32,
    /// Sources imported that month, each once.
    pub imports: u32,
}

impl Bookkeeping {
    /// Whether nothing was done.
    pub fn is_empty(&self) -> bool {
        *self == Bookkeeping::default()
    }
}

/// One project's revenue, per month.
pub(super) struct ProjectRevenue {
    pub project: String,
    pub months: Vec<i128>,
}

/// Every month's figures, each list one per month from `first`.
pub(super) struct Figures {
    /// Income, expenses and net, as [`crate::net`] totals them.
    pub flows: Vec<NetFlow>,
    /// Net sats into the BTC asset accounts.
    pub sats: Vec<i128>,
    /// Each project with an income account, sorted by name.
    pub projects: Vec<ProjectRevenue>,
    /// The vault edits written each month.
    pub bookkeeping: Vec<Bookkeeping>,
}

/// Where [`Figures::gather`] reads from.
pub(super) struct Sources<'a> {
    pub books: &'a Books,
    pub home: CommodityId,
    pub derive: &'a DeriveInputs,
    pub projects: &'a Projects,
    pub zone: Zone,
    pub rule_cap: u32,
}

impl Figures {
    /// The figures of the `len` months from `first`.
    #[profiling::function]
    pub fn gather(sources: &Sources<'_>, first: Month, len: usize) -> Figures {
        let range = DateRange::new(first.start(), Month(first.0 + len as u32).start());
        let flows = net::net_flows(sources.books, sources.home, range, Bucket::Month);
        Figures {
            flows: (0..len)
                .map(|i| flows.get(i).copied().unwrap_or_default())
                .collect(),
            sats: sats(sources.books, first, len),
            projects: project_revenue(sources, range, len),
            bookkeeping: bookkeeping(sources, first, len),
        }
    }
}

/// The index of `month` among the `len` months from `first`, if it is one.
fn slot(first: Month, len: usize, month: Month) -> Option<usize> {
    let at = month.0.checked_sub(first.0)? as usize;
    (at < len).then_some(at)
}

/// Net sats into accounts under [`ASSETS`], per month. Moving between two
/// of the owner's wallets nets to nothing; a ledger without BTC stacks
/// nothing.
fn sats(books: &Books, first: Month, len: usize) -> Vec<i128> {
    let mut out = vec![0; len];
    let ledger = books.ledger();
    let journal = ledger.journal();
    let (Some(btc), Some(assets)) = (
        journal.commodities.get(BTC),
        journal.accounts.lookup(ASSETS),
    ) else {
        return out;
    };
    let scale = 10i128.pow(u32::from(journal.commodities.precision(btc)));
    let accounts = &journal.accounts;
    for account in accounts.ids() {
        if account != assets && !accounts.is_ancestor(assets, account) {
            continue;
        }
        for posting in ledger.postings_to(account) {
            if posting.commodity != btc {
                continue;
            }
            if let Some(at) = slot(first, len, Month::of(posting.date)) {
                out[at] += i128::from(posting.qty) * SATS_PER_BTC / scale;
            }
        }
    }
    out
}

/// Each project's revenue per month: what its income accounts took in,
/// each account counted once, in the project [`Projects::project_of`] puts
/// it in. A project's root adds its subtree, and takes it back out of the
/// project around it, if one is.
fn project_revenue(sources: &Sources<'_>, range: DateRange, len: usize) -> Vec<ProjectRevenue> {
    let Sources {
        books,
        home,
        projects,
        ..
    } = *sources;
    let names: BTreeSet<&str> = projects
        .roots()
        .iter()
        .filter(|r| under(&r.account, INCOME))
        .map(|r| r.project.as_str())
        .collect();
    let mut out: Vec<ProjectRevenue> = names
        .into_iter()
        .map(|project| ProjectRevenue {
            project: project.to_owned(),
            months: vec![0; len],
        })
        .collect();
    let accounts = &books.ledger().journal().accounts;
    let query = books.query();
    for root in projects
        .roots()
        .iter()
        .filter(|r| under(&r.account, INCOME))
    {
        let Some(account) = accounts.lookup(&root.account) else {
            continue;
        };
        let enclosing = root
            .account
            .rfind(':')
            .and_then(|at| projects.project_of(&root.account[..at]));
        if enclosing == Some(root.project.as_str()) {
            continue;
        }
        let series = query.flows(account, range, Bucket::Month, home);
        let mut add = |project: &str, sign: i128| {
            let Some(into) = out.iter_mut().find(|p| p.project == project) else {
                return;
            };
            for (month, point) in into.months.iter_mut().zip(&series.points) {
                // Income postings are negative: revenue is their negation.
                *month -= point.total * sign;
            }
        };
        add(&root.project, 1);
        if let Some(enclosing) = enclosing {
            add(enclosing, -1);
        }
    }
    out
}

/// Whether `account` is `top` or under it.
fn under(account: &str, top: &str) -> bool {
    account
        .strip_prefix(top)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
}

/// The vault edits behind the books, counted in the month each was
/// written, and the sources imported each month. A note dated outside the
/// months counts nowhere.
fn bookkeeping(sources: &Sources<'_>, first: Month, len: usize) -> Vec<Bookkeeping> {
    let Sources {
        books,
        derive,
        zone,
        rule_cap,
        ..
    } = *sources;
    let mut out = vec![Bookkeeping::default(); len];
    let at = |secs: u64| slot(first, len, Month::of(zone.date(secs)));
    let mut per_rule = vec![0u32; derive.provenance.rules.len()];
    for i in 0..books.ledger().txns().len() {
        let why = books.derived.why(TxnId(i as u32));
        let Some(month) = derive.written_at(why).and_then(at) else {
            continue;
        };
        let counts = &mut out[month];
        match why {
            Why::Override(_) | Why::Split(_) => counts.triaged += 1,
            Why::Transfer(_) => counts.linked += 1,
            Why::Rule(i) => {
                let Some(owner) = derive.owners.get(i as usize) else {
                    continue;
                };
                let Some(seen) = per_rule.get_mut(owner.rule as usize) else {
                    continue;
                };
                if *seen < rule_cap {
                    *seen += 1;
                    counts.ruled += 1;
                }
            }
            Why::Untouched | Why::Default => {}
        }
    }
    let imports: BTreeSet<SourceMonth<'_>> = books
        .ledger()
        .segments()
        .iter()
        .filter_map(|s| {
            Some(SourceMonth {
                month: at(s.header.imported)?,
                source: &s.header.source,
            })
        })
        .collect();
    for import in imports {
        out[import.month].imports += 1;
    }
    out
}

/// A source imported in a month, so each pair counts once.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SourceMonth<'a> {
    month: usize,
    source: &'a str,
}
