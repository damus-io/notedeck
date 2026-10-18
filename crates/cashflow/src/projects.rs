//! Which project each account earns or spends for, owned.
//!
//! [`crate::chart::Chart`] borrows its notes from a nostrdb transaction, so
//! it lives only as long as one. A dashboard keeps its figures across
//! frames, so it needs the one thing it reads from the chart — the
//! project attribution — in a form that outlives the transaction:
//! [`Projects`], made with [`crate::chart::Chart::projects`].
//!
//! The attribution is the chart's: an account is in the project it sets,
//! else its nearest ancestor's ([`Projects::project_of`] agrees with
//! [`crate::chart::Chart::project_of`]).

/// An account that sets a project: everything under it is in that project
/// unless an account further down says otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRoot {
    /// The account's full name in the derived ledger.
    pub account: String,
    /// The project it sets.
    pub project: String,
}

/// Every account that sets a project, sorted by account name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projects {
    roots: Vec<ProjectRoot>,
}

impl Projects {
    /// The attribution `roots` set. When one account is listed twice, the
    /// first listing wins.
    pub fn new(roots: impl IntoIterator<Item = ProjectRoot>) -> Self {
        let mut roots: Vec<ProjectRoot> = roots.into_iter().collect();
        roots.sort_by(|a, b| a.account.cmp(&b.account));
        roots.dedup_by(|later, first| later.account == first.account);
        Self { roots }
    }

    /// Every account that sets a project, sorted by account name.
    pub fn roots(&self) -> &[ProjectRoot] {
        &self.roots
    }

    /// Whether no account sets a project.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The project `account` is in: its own, else its nearest ancestor's.
    pub fn project_of(&self, account: &str) -> Option<&str> {
        self.root_of(account).map(|r| r.project.as_str())
    }

    /// The account that puts `account` in its project: itself, else its
    /// nearest ancestor that sets one.
    pub fn root_of(&self, account: &str) -> Option<&ProjectRoot> {
        let mut path = account;
        loop {
            if let Ok(at) = self
                .roots
                .binary_search_by(|r| r.account.as_str().cmp(path))
            {
                return Some(&self.roots[at]);
            }
            path = &path[..path.rfind(':')?];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(account: &str, project: &str) -> ProjectRoot {
        ProjectRoot {
            account: account.to_owned(),
            project: project.to_owned(),
        }
    }

    /// An account is in its own project, else its nearest ancestor's, and
    /// a name that only shares a prefix is not a descendant.
    #[test]
    fn project_of_takes_the_nearest_ancestors() {
        let projects = Projects::new([
            root("income:damus:zaps:personal", "personal"),
            root("income:damus", "damus"),
        ]);
        assert_eq!(projects.project_of("income:damus"), Some("damus"));
        assert_eq!(projects.project_of("income:damus:purple"), Some("damus"));
        assert_eq!(
            projects.project_of("income:damus:zaps:personal:tips"),
            Some("personal")
        );
        assert_eq!(projects.project_of("income:damusx"), None);
        assert_eq!(projects.project_of("income"), None);
        let root = projects.root_of("income:damus:zaps").unwrap();
        assert_eq!(root.account, "income:damus");
    }

    /// An account listed twice keeps its first project.
    #[test]
    fn the_first_listing_of_an_account_wins() {
        let projects = Projects::new([root("income:a", "one"), root("income:a", "two")]);
        assert_eq!(projects.roots().len(), 1);
        assert_eq!(projects.project_of("income:a:b"), Some("one"));
    }
}
