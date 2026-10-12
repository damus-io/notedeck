//! The current titles of the cards session titles refer to.
//!
//! A session titled `headway:<board>/<word-id>` is listed under its card's
//! title, split into code and headline (see [`headway::title::TitleParts`]).
//! Getting that title folds the card's board and returns an owned `String`
//! ([`KindRendererRegistry::title`](notedeck::KindRendererRegistry::title)), too
//! much to do for every row every frame, and the board cache has no revision
//! to say when it changed. So the list looks each ref up at most once per
//! [`REFRESH_SECS`] and draws from the text kept here.

use std::collections::HashMap;

/// How long a looked-up title is drawn before it is looked up again, so a
/// card renamed on its board shows in the list within this many seconds.
pub const REFRESH_SECS: f64 = 2.0;

/// Card titles by card ref, as last looked up.
#[derive(Default)]
pub struct CardTitles {
    by_ref: HashMap<String, CardTitle>,
}

/// One ref's last lookup.
struct CardTitle {
    /// The card's title, or `None` when the ref didn't resolve (its card isn't
    /// in the local db yet), kept so a miss isn't retried every frame either.
    title: Option<String>,
    /// When it was looked up, in egui input time.
    checked_at: f64,
}

impl CardTitles {
    /// The title last looked up for `card_ref`, if it resolved.
    pub fn get(&self, card_ref: &str) -> Option<&str> {
        self.by_ref.get(card_ref)?.title.as_deref()
    }

    /// Whether `card_ref` was looked up within [`REFRESH_SECS`] of `now`.
    pub fn is_fresh(&self, card_ref: &str, now: f64) -> bool {
        self.by_ref
            .get(card_ref)
            .is_some_and(|t| (0.0..REFRESH_SECS).contains(&(now - t.checked_at)))
    }

    /// Record a lookup of `card_ref` made at `now`.
    pub fn store(&mut self, card_ref: &str, title: Option<String>, now: f64) {
        match self.by_ref.get_mut(card_ref) {
            Some(entry) => {
                entry.title = title;
                entry.checked_at = now;
            }
            None => {
                self.by_ref.insert(
                    card_ref.to_owned(),
                    CardTitle {
                        title,
                        checked_at: now,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_is_fresh_for_the_refresh_window_then_stale() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        assert!(!titles.is_fresh(r, 10.0), "never looked up");
        titles.store(r, Some("UT5: the HUD".into()), 10.0);
        assert_eq!(titles.get(r), Some("UT5: the HUD"));
        assert!(titles.is_fresh(r, 10.0 + REFRESH_SECS - 0.1));
        assert!(!titles.is_fresh(r, 10.0 + REFRESH_SECS));
        // A clock that went backwards (a fresh egui context) looks it up again.
        assert!(!titles.is_fresh(r, 9.0));
    }

    #[test]
    fn a_miss_is_kept_but_has_no_title() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        titles.store(r, Some("old".into()), 0.0);
        titles.store(r, None, 1.0);
        assert_eq!(titles.get(r), None);
        assert!(titles.is_fresh(r, 1.5));
    }
}
