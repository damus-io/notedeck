//! The account's keymap overrides on the chrome: read from nostrdb on startup
//! and on an account switch, merged over the built-in keymap, and merged again
//! whenever a new revision lands, saved on this device or synced from another
//! (see [`crate::keymap::user`]).

use nostrdb::Subscription;
use nostrdb_net::Pubkey;
use notedeck::AppContext;

use super::Chrome;
use crate::keymap::{self, user, Keymap, DEFAULT_KEYMAP};

/// The most new keymap-note keys one poll drains. Any number means "read it
/// again"; this many fit nostrdb's on-stack poll buffer, so an idle poll
/// doesn't allocate.
const POLL_MAX: u32 = 16;

/// The chrome's hold on the selected account's keymap overrides.
#[derive(Default)]
pub(super) struct UserKeymap {
    /// The account the overrides were last read for; a different selected
    /// account reads them again.
    account: Option<Pubkey>,
    /// A local subscription on the account's app-data notes, so a revision
    /// saved on another device takes effect here without a restart.
    sub: Option<Subscription>,
    /// Whether the keymap in use is a user's merged over the defaults, so an
    /// account with none, or a broken one, goes back to the defaults.
    applied: bool,
    /// Why the stored overrides didn't load, for an editor to show; `None`
    /// when they loaded or there are none.
    error: Option<String>,
    /// The last save from here, which the next one by the same account
    /// stamps past: it may not be stored yet when the next runs (see
    /// [`user::write_user_keymap`]).
    last_write: Option<LastWrite>,
}

/// A keymap note this chrome wrote.
struct LastWrite {
    /// Whose it was.
    author: Pubkey,
    /// What it was stamped with.
    created_at: u64,
}

impl Chrome {
    /// Keep the keymap in step with the selected account's overrides: read
    /// them when the account changes, and again when its subscription says a
    /// new revision landed. An idle frame polls one subscription and doesn't
    /// allocate.
    #[profiling::function]
    pub(super) fn poll_user_keymap(&mut self, ctx: &mut AppContext) {
        let author = *ctx.accounts.selected_account_pubkey();
        if self.user_keymap.account != Some(author) {
            self.watch_user_keymap(ctx, author);
            self.reload_user_keymap(ctx, &author);
            return;
        }
        let Some(sub) = self.user_keymap.sub else {
            return;
        };
        if ctx.ndb.poll_for_notes(sub, POLL_MAX).is_empty() {
            return;
        }
        self.reload_user_keymap(ctx, &author);
    }

    /// Subscribe to `author`'s keymap notes in place of the last account's.
    fn watch_user_keymap(&mut self, ctx: &mut AppContext, author: Pubkey) {
        if let Some(sub) = self.user_keymap.sub.take() {
            if let Err(err) = ctx.ndb.unsubscribe(sub) {
                tracing::warn!("user keymap: unsubscribe failed: {err}");
            }
        }
        self.user_keymap.sub = ctx
            .ndb
            .subscribe(&[user::user_keymap_filter(&author)])
            .inspect_err(|err| tracing::error!("user keymap: subscribe failed: {err}"))
            .ok();
        self.user_keymap.account = Some(author);
    }

    /// Read `author`'s overrides and use them merged over the defaults. With
    /// none, or ones that don't merge, use the defaults; a keymap set some
    /// other way ([`Chrome::set_keymap`]) is only replaced when a user's was
    /// in use or one now loads.
    fn reload_user_keymap(&mut self, ctx: &AppContext, author: &Pubkey) {
        let merged =
            user::load_user_keymap(ctx.ndb, author).map(|toml| self.merge_user(ctx, &toml));
        match merged {
            Some(Ok(keymap)) => {
                self.use_user_keymap(ctx, keymap);
                return;
            }
            Some(Err(err)) => {
                tracing::error!("user keymap: {err}; using the defaults");
                self.user_keymap.error = Some(err);
            }
            None => self.user_keymap.error = None,
        }
        if self.user_keymap.applied {
            self.user_keymap.applied = false;
            self.set_keymap(keymap::default_keymap());
        }
    }

    /// `toml` merged over the built-in keymap, its tools checked against the
    /// ones the apps have.
    fn merge_user(&self, ctx: &AppContext, toml: &str) -> Result<Keymap, String> {
        keymap::parse_over(DEFAULT_KEYMAP, toml, |tool| self.tool_known(ctx, tool))
            .map_err(|err| err.to_string())
    }

    /// Put a merged user `keymap` in use.
    fn use_user_keymap(&mut self, ctx: &AppContext, keymap: Keymap) {
        self.set_keymap(keymap);
        self.user_keymap.applied = true;
        self.user_keymap.error = None;
        self.log_keymap_problems(ctx);
    }

    /// Save `toml` as the selected account's keymap overrides and use it.
    ///
    /// It is merged first, so one that doesn't parse, or names a tool no app
    /// has, is refused with the reason and nothing is written. Saved as a
    /// PNS-encrypted note ([`user::write_user_keymap`]), which syncs to the
    /// account's other devices. A watch-only account can't sign one.
    pub fn save_user_keymap(&mut self, ctx: &mut AppContext, toml: &str) -> Result<(), String> {
        let Some(filled) = ctx.accounts.selected_filled() else {
            return Err("a watch-only account can't save a keymap".to_owned());
        };
        let author = *filled.pubkey;
        let secret = filled.secret_key.secret_bytes();
        let keymap = self.merge_user(ctx, toml)?;
        let after = match &self.user_keymap.last_write {
            Some(last) if last.author == author => last.created_at,
            _ => 0,
        };
        let created_at = user::write_user_keymap(ctx.ndb, &author, &secret, toml, after)?;
        self.user_keymap.last_write = Some(LastWrite { author, created_at });
        self.use_user_keymap(ctx, keymap);
        Ok(())
    }

    /// Why the selected account's stored keymap overrides didn't load, if
    /// they didn't; the defaults are in use meanwhile.
    pub fn keymap_error(&self) -> Option<&str> {
        self.user_keymap.error.as_deref()
    }
}
