//! Cashflow's entries on its chrome-owned nav stack (see
//! [`notedeck::Navigator`]).
//!
//! The root entry (a `()` token) is the dashboard: whichever [`crate::View`]
//! the rail has picked. A transaction's detail is a [`CashflowRoute::Txn`]
//! pushed on top of it, from a register row, an inbox row, or another
//! detail, so the chrome's back, Alt-←, mouse-back and drag-back all return
//! to where it was opened from, and a detail opened from a detail is one
//! more step of real history.
//!
//! A route names its transaction by record id, never by `TxnId`: a rescan or
//! a layer toggle renumbers the transactions, and the entry must still mean
//! the same one afterwards.
//!
//! [`crate::CashflowApp`] seeds its view from the stack's top once per
//! change of it ([`SeenRoute`]), and after the frame reconciles the stack
//! with what the frame did: a detail opened pushes, the detail's own back
//! steps back, and a view picked while a detail shows drops every detail
//! entry.

use std::any::Any;
use std::rc::Rc;

/// A view of Cashflow's that the chrome's back and forward step between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CashflowRoute {
    /// One transaction's detail.
    Txn(TxnRoute),
}

/// The transaction a [`CashflowRoute::Txn`] shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxnRoute {
    /// Its record id (`; id:`).
    pub record: String,
    /// "payee · date", snapshotted when the entry was pushed: the chrome's
    /// history dropdown asks for an entry's title with no books to look it
    /// up in.
    pub title: String,
}

impl CashflowRoute {
    /// The record whose detail this entry shows.
    pub fn record(&self) -> &str {
        match self {
            CashflowRoute::Txn(txn) => &txn.record,
        }
    }

    /// The entry's title in the history dropdown.
    pub fn title(&self) -> &str {
        match self {
            CashflowRoute::Txn(txn) => &txn.title,
        }
    }
}

/// The token Cashflow last seeded its view from, so the seed acts once per
/// change of the stack's top rather than every frame, and a view picked
/// while on an entry sticks until the chrome catches up.
#[derive(Default)]
pub(crate) struct SeenRoute {
    token: Option<Rc<dyn Any>>,
}

impl SeenRoute {
    /// Record `token` as the top; true when it differs from the last one.
    pub(crate) fn changed_to(&mut self, token: &Rc<dyn Any>) -> bool {
        if self
            .token
            .as_ref()
            .is_some_and(|seen| Rc::ptr_eq(seen, token))
        {
            return false;
        }
        self.token = Some(token.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same token seeds once; another one, even an equal route, seeds
    /// again, since it is another entry.
    #[test]
    fn seen_route_changes_once_per_token() {
        let mut seen = SeenRoute::default();
        let route = || {
            Rc::new(CashflowRoute::Txn(TxnRoute {
                record: "card:1".to_owned(),
                title: "Coffee · 2024-10-02".to_owned(),
            })) as Rc<dyn Any>
        };
        let a = route();
        assert!(seen.changed_to(&a));
        assert!(!seen.changed_to(&a));
        assert!(seen.changed_to(&route()));
    }
}
