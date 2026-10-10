//! A Notedeck host for app tests that need a real [`AppContext`].
//!
//! [`DeviceHarness`](crate::DeviceHarness) runs a whole app inside an eframe
//! harness. A test that renders one view of an app, or calls into an app
//! with an `AppContext` of its own, wants only the host: its store, accounts
//! and settings. [`TestHost`] is that host, booted the way the device
//! harness boots one (`--testrunner`, so it connects to no relay) and kept
//! alive with its data dir.

use nostrdb_net::FullKeypair;
use notedeck::{AppContext, Notedeck};
use tempfile::TempDir;

use crate::fixtures::add_account;

/// A booted [`Notedeck`] over its own data dir, with the welcome flow done.
///
/// Its `egui::Context` is a throwaway one: a kittest harness makes its own,
/// which [`Self::setup_once`] installs notedeck's fonts and visuals into on
/// the harness's first frame.
pub struct TestHost {
    notedeck: Notedeck,
    set_up: bool,
    /// Keeps the store and settings on disk for as long as the host lives.
    _data_dir: TempDir,
}

impl TestHost {
    /// A host with no account, as notedeck boots on a fresh install.
    pub fn new() -> Self {
        let data_dir = TempDir::new().expect("tmpdir");
        let args = ["notedeck-test".to_owned(), "--testrunner".to_owned()];
        let mut notedeck = Notedeck::init(&egui::Context::default(), data_dir.path(), &args);
        notedeck.app_context().settings.complete_welcome();
        TestHost {
            notedeck,
            set_up: false,
            _data_dir: data_dir,
        }
    }

    /// A host signed in as `account`: added with its secret key and selected.
    pub fn with_account(account: &FullKeypair) -> Self {
        let mut host = Self::new();
        {
            let mut app_ctx = host.app_context();
            add_account(&mut app_ctx, account);
            app_ctx.select_account(&account.pubkey);
        }
        host
    }

    /// What the host hands an app each frame.
    pub fn app_context(&mut self) -> AppContext<'_> {
        self.notedeck.app_context()
    }

    /// The host itself, to register what an app registers at startup
    /// (reference parsers, kind renderers, an app).
    pub fn notedeck(&mut self) -> &mut Notedeck {
        &mut self.notedeck
    }

    /// Install notedeck's fonts, visuals and text styles into `ctx`, the
    /// first time only, with animations off so a frame shows where a widget
    /// ends up. Returns whether this call did it.
    ///
    /// Fonts set during a frame apply from the next one, so a frame that
    /// gets `true` back has nothing worth drawing.
    pub fn setup_once(&mut self, ctx: &egui::Context) -> bool {
        if self.set_up {
            return false;
        }
        self.notedeck.setup(ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.0);
        self.set_up = true;
        true
    }
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh host has no account and is past the welcome screen.
    #[test]
    fn new_host_has_no_account_and_no_welcome() {
        let mut host = TestHost::new();
        let app_ctx = host.app_context();
        assert!(app_ctx.settings.welcome_completed());
        assert!(app_ctx
            .accounts
            .get_selected_account()
            .key
            .secret_key
            .is_none());
    }

    /// `with_account` selects the account it adds, secret key and all.
    #[test]
    fn with_account_selects_the_account() {
        let account = FullKeypair::generate();
        let mut host = TestHost::with_account(&account);
        let selected = host
            .app_context()
            .accounts
            .get_selected_account()
            .key
            .clone();
        assert_eq!(selected.pubkey, account.pubkey);
        assert!(selected.secret_key.is_some());
    }

    /// Setup runs on the first call only.
    #[test]
    fn setup_once_runs_once() {
        let mut host = TestHost::new();
        let ctx = egui::Context::default();
        assert!(host.setup_once(&ctx));
        assert!(!host.setup_once(&ctx));
    }
}
