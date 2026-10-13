//! The current titles of the cards session titles refer to.
//!
//! A session titled `headway:<board>/<word-id>` is listed under its card's
//! title, split into code and headline (see [`headway::title::TitleParts`]).
//! Getting that title folds the card's board and returns an owned `String`
//! ([`KindRendererRegistry::title`](notedeck::KindRendererRegistry::title)), too
//! much to do for every row every frame, and the board cache has no revision
//! to say when it changed. So the list looks each ref up at most once per
//! [`REFRESH_SECS`] and draws from the text kept here.
//!
//! Each lookup also keeps the card's [`Outline`]: its parent card, which
//! groups a successor chain under its epic, and how many of its subissues
//! are done, which the chain's header counts.

use std::collections::HashMap;

use notedeck::{Outline, Progress};

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
    /// The card's place in its outline, `None` when the ref didn't resolve.
    outline: Option<Outline>,
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

    /// The ref of the card `card_ref`'s card is a subissue of, as last looked up.
    pub fn parent(&self, card_ref: &str) -> Option<&str> {
        self.by_ref
            .get(card_ref)?
            .outline
            .as_ref()?
            .parent
            .as_deref()
    }

    /// How many of `card_ref`'s subissues were done, of how many, as last
    /// looked up; `None` for a card with none.
    pub fn parts(&self, card_ref: &str) -> Option<Progress> {
        self.by_ref.get(card_ref)?.outline.as_ref()?.parts
    }

    /// Record a lookup of `card_ref` made at `now`. Returns whether it found
    /// anything other than what was kept, so the caller regroups: a card's
    /// parent links successor chains, and an epic's title and progress head
    /// them (see
    /// [`SessionManager::lineage_changed`](crate::session::SessionManager::lineage_changed)).
    pub fn store(&mut self, card_ref: &str, lookup: CardLookup, now: f64) -> bool {
        let CardLookup { title, outline } = lookup;
        match self.by_ref.get_mut(card_ref) {
            Some(entry) => {
                let changed = entry.title != title || entry.outline != outline;
                entry.title = title;
                entry.outline = outline;
                entry.checked_at = now;
                changed
            }
            None => {
                self.by_ref.insert(
                    card_ref.to_owned(),
                    CardTitle {
                        title,
                        outline,
                        checked_at: now,
                    },
                );
                true
            }
        }
    }
}

/// What one lookup of a card ref found.
#[derive(Default)]
pub struct CardLookup {
    /// The card's title, `None` when the ref didn't resolve.
    pub title: Option<String>,
    /// Its outline, `None` when the ref didn't resolve or its kind has none.
    pub outline: Option<Outline>,
}

impl CardLookup {
    /// A lookup that found `title` and nothing else.
    pub fn titled(title: &str) -> Self {
        CardLookup {
            title: Some(title.to_owned()),
            outline: None,
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
        titles.store(r, CardLookup::titled("UT5: the HUD"), 10.0);
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
        titles.store(r, CardLookup::titled("old"), 0.0);
        titles.store(r, CardLookup::default(), 1.0);
        assert_eq!(titles.get(r), None);
        assert!(titles.is_fresh(r, 1.5));
    }

    #[test]
    fn a_store_says_when_what_it_found_changed() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        let under = |parent: Option<&str>| CardLookup {
            title: Some("UT5".into()),
            outline: Some(Outline {
                parent: parent.map(str::to_owned),
                parts: None,
            }),
        };
        assert!(titles.store(r, under(None), 0.0), "first lookup");
        assert!(!titles.store(r, under(None), 0.5), "same again");
        assert!(titles.store(r, under(Some("headway:dave/epic")), 1.0));
        assert_eq!(titles.parent(r), Some("headway:dave/epic"));
        assert!(!titles.store(r, under(Some("headway:dave/epic")), 2.0));
        assert!(
            titles.store(r, CardLookup::titled("UT5"), 3.0),
            "lost its outline"
        );
        assert_eq!(titles.parent(r), None);
    }
}
