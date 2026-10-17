//! The view router: one [`View`] per screen of the app.

/// A screen of the Cashflow app. The router in [`crate::CashflowApp`] shows one
/// at a time; [`crate::views::show`] maps each to its module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    /// Net worth, this month's in and out, and the headline numbers.
    Overview,
    /// Where money goes and comes from, by account category.
    Categories,
    /// Categories and balances over time.
    Trends,
    /// The postings themselves, and the triage inbox for uncategorized ones.
    Transactions,
    /// Revenue and cost per project.
    Projects,
    /// Targets tracked as Headway cards.
    Goals,
    /// The gamified view: skills, XP and achievements earned from cashflow.
    Adventure,
    /// Layers, journal and sync status, and the palette.
    Settings,
}

impl View {
    /// Every view, in nav order.
    pub const ALL: [View; 8] = [
        View::Overview,
        View::Categories,
        View::Trends,
        View::Transactions,
        View::Projects,
        View::Goals,
        View::Adventure,
        View::Settings,
    ];

    /// The view's name, as the nav and its heading show it.
    pub fn label(self) -> &'static str {
        match self {
            View::Overview => "Overview",
            View::Categories => "Categories",
            View::Trends => "Trends",
            View::Transactions => "Transactions",
            View::Projects => "Projects",
            View::Goals => "Goals",
            View::Adventure => "Adventure",
            View::Settings => "Settings",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every view is in [`View::ALL`] once, with its own label, so the nav
    /// never shows two tabs that look alike.
    #[test]
    fn all_lists_each_view_once_with_its_own_label() {
        let mut views = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        for view in View::ALL {
            assert!(views.insert(view), "{view:?} listed twice");
            assert!(labels.insert(view.label()), "{view:?} shares a label");
        }
    }
}
