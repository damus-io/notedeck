//! The books: the journal loaded, categorized and indexed, ready to query.
//!
//! [`Books::load`] is the pipeline every reader runs: read the segments the
//! layer filter allows into the raw ledger, [`derive`] the categorized one
//! from it with the edits, build the price db from its `P` directives, and
//! index it for queries in the home commodity. The CLI's `export` and
//! `report` run it once per command; the app runs it off the UI thread and
//! again when the journal or the edits change.

use ledgerdb::{
    CommodityId, Derived, Edits, JournalStore, LayerFilter, Ledger, LoadReport, PriceDb, Query,
    Rollups, StoreError, derive,
};

/// The derived ledger and everything a query reads.
pub struct Books {
    /// What loading the raw ledger skipped or found damaged.
    pub report: LoadReport,
    /// The categorized ledger, and why each transaction was categorized so.
    pub derived: Derived,
    /// Rates from the ledger's `P` directives.
    pub prices: PriceDb,
    /// The query index over [`Self::ledger`].
    pub rollups: Rollups,
}

impl Books {
    /// Load the segments in `store` that `filter` includes, derive them with
    /// `edits`, and index the result with `home` (a commodity name) as the
    /// home commodity. A `home` the ledger has never seen leaves the index
    /// without one: native balances still work, converted ones don't.
    ///
    /// Only listing the store fails; bad segments are in [`Self::report`].
    #[profiling::function]
    pub fn load(
        store: &JournalStore,
        filter: LayerFilter,
        edits: &Edits,
        home: &str,
    ) -> Result<Books, StoreError> {
        let loaded = Ledger::load(store, filter)?;
        let derived = derive(&loaded.ledger, edits);
        let prices = PriceDb::from_ledger(derived.ledger());
        let home = derived.ledger().journal().commodities.get(home);
        let rollups = Rollups::new(derived.ledger(), &prices, home);
        Ok(Books {
            report: loaded.report,
            derived,
            prices,
            rollups,
        })
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

        let books = Books::load(&store, LayerFilter::All, &edits, "CAD").unwrap();
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
            &edits,
            "CAD",
        )
        .unwrap();
        assert!(none.ledger().txns().is_empty());
        assert_eq!(none.home(), None);
    }
}
