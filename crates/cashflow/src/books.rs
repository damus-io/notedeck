//! The books: the journal loaded, categorized and indexed, ready to query.
//!
//! [`Books::load`] is the pipeline every reader runs: read the segments the
//! layer filter allows, less the ones the vault retires
//! ([`crate::manifest::retired`]), into the raw ledger, [`derive`] the categorized one
//! from it with the edits, build the price db from its `P` directives, and
//! index it for queries in the home commodity. The CLI's `export` and
//! `report` run it once per command.
//!
//! The app keeps the raw ledger instead, so a new segment is merged in with
//! [`Ledger::refresh`] rather than reloading everything, and an edit
//! re-derives with [`rederive`], reusing the previous books' rule matches.
//! [`Books::from_raw`] is that second half of the pipeline on its own.

use std::collections::HashSet;
use std::sync::Arc;

use ledgerdb::{
    CommodityId, Derived, Edits, JournalStore, LayerFilter, Ledger, LoadReport, PriceDb, Query,
    Rollups, SegmentHash, StoreError, derive, rederive,
};

/// The derived ledger and everything a query reads.
pub struct Books {
    /// What loading the raw ledger skipped or found damaged.
    pub report: LoadReport,
    /// The categorized ledger, and why each transaction was categorized so.
    /// Shared, since the books themselves aren't `Sync` (the price db
    /// caches conversion paths), so a re-derive on another thread can reuse
    /// its rule matches while these books are still drawn.
    pub derived: Arc<Derived>,
    /// Rates from the ledger's `P` directives.
    pub prices: PriceDb,
    /// The query index over [`Self::ledger`].
    pub rollups: Rollups,
}

impl Books {
    /// Load the segments in `store` that `filter` includes and `retired`
    /// doesn't name (even when the file is still here), derive them with
    /// `edits`, and index the result with `home` (a commodity name) as the
    /// home commodity. A `home` the ledger has never seen leaves the index
    /// without one: native balances still work, converted ones don't.
    ///
    /// Only listing the store fails; bad segments are in [`Self::report`].
    #[profiling::function]
    pub fn load(
        store: &JournalStore,
        filter: LayerFilter,
        retired: &HashSet<SegmentHash>,
        edits: &Edits,
        home: &str,
    ) -> Result<Books, StoreError> {
        let loaded = Ledger::load_without(store, filter, retired.clone())?;
        Ok(Books::from_raw(
            &loaded.ledger,
            loaded.report,
            edits,
            None,
            home,
        ))
    }

    /// Derive `raw` with `edits` and index the result with `home` as the
    /// home commodity, as [`Self::load`] does after reading the store.
    /// `report` is what the load or refresh of `raw` found.
    ///
    /// With `previous`, the [`Self::derived`] of the books last derived from
    /// `raw` (before a refresh or an edit), the derive reuses its rule
    /// matches when the rules and segments are unchanged ([`rederive`]); the
    /// result is the same either way.
    #[profiling::function]
    pub fn from_raw(
        raw: &Ledger,
        report: LoadReport,
        edits: &Edits,
        previous: Option<&Derived>,
        home: &str,
    ) -> Books {
        let derived = Arc::new(match previous {
            Some(previous) => rederive(raw, edits, previous),
            None => derive(raw, edits),
        });
        let prices = PriceDb::from_ledger(derived.ledger());
        let home = derived.ledger().journal().commodities.get(home);
        let rollups = Rollups::new(derived.ledger(), &prices, home);
        Books {
            report,
            derived,
            prices,
            rollups,
        }
    }

    /// The categorized ledger.
    pub fn ledger(&self) -> &Ledger {
        self.derived.ledger()
    }

    /// The home commodity, if the ledger has it.
    pub fn home(&self) -> Option<CommodityId> {
        self.rollups.home()
    }

    /// Queries over the categorized ledger.
    pub fn query(&self) -> Query<'_> {
        self.rollups.query(self.ledger(), &self.prices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledgerdb::{Date, Journal, SegmentHeader};

    #[test]
    fn load_derives_with_the_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let text = "2024-01-02 * Corner Grocer\n  ; id: t:1\n  liabilities:card  -4.50 CAD\n  expenses:unknown\n\n\
                    2024-01-03 * Airline\n  ; id: t:2\n  liabilities:card  -100.00 CAD\n  expenses:unknown\n";
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "cards".to_owned(),
        };
        store
            .write_segment(&header, &Journal::parse(text).journal)
            .unwrap();
        let edits = crate::rules::parse(
            "[[rule]]\naccount = \"expenses:food\"\nmatch = [\"grocer\"]\n",
            std::path::Path::new("r.toml"),
        )
        .unwrap();

        let books = Books::load(&store, LayerFilter::All, &HashSet::new(), &edits, "CAD").unwrap();
        let accounts = &books.ledger().journal().accounts;
        let food = accounts.lookup("expenses:food").unwrap();
        let unknown = accounts.lookup("expenses:unknown").unwrap();
        let at = Date::parse("2024-12-31").unwrap();
        let home = books.home();
        assert!(home.is_some());
        let q = books.query();
        assert_eq!(q.balance(food, at, home).totals[0].qty, 450);
        assert_eq!(q.balance(unknown, at, home).totals[0].qty, 10000);

        let none = Books::load(
            &store,
            LayerFilter::Except(vec!["cards".to_owned()]),
            &HashSet::new(),
            &edits,
            "CAD",
        )
        .unwrap();
        assert!(none.ledger().txns().is_empty());
        assert_eq!(none.home(), None);
    }
}
