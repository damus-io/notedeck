//! The current titles of the cards session titles refer to.
//!
//! A session titled `headway:<board>/<word-id>` is listed under its card's
//! title.
//! Getting that title folds the card's board and returns an owned `String`
//! ([`KindRendererRegistry::title`](notedeck::KindRendererRegistry::title)), too
//! much to do for every row every frame. So each lookup is kept here with the
//! revision of what it read
//! ([`AppRegistries::reference_revision`](notedeck::AppRegistries::reference_revision)),
//! and the list looks a ref up again only once that revision moves: a card
//! renamed on its board shows the next frame, and an unchanged one is never
//! looked up twice. A ref whose parser or renderer can't say when it changed
//! is looked up again every [`UNVERSIONED_REFRESH_SECS`] instead.
//!
//! Each lookup also keeps the card's [`Outline`]: its parent card, which
//! groups a successor chain under its epic.
//!
//! The Cmd-K search reads the same titles, so a session is found by its
//! card's title (see [`search`](crate::search)).

use std::collections::HashMap;

use nostrdb::NoteKey;
use notedeck::{AppRegistries, Outline, ReferenceLookup, ReferenceResolveCtx, ReferenceRevision};

/// How long a lookup of a ref nothing versions is kept before it is looked
/// up again. Such a ref's parser or renderer can't say when its answer
/// changed (none of the registered ones that offer a title is like that), so
/// this bounds how stale it gets without looking it up every frame.
pub const UNVERSIONED_REFRESH_SECS: f64 = 30.0;

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
    /// The note the ref resolved to, which the revision check reads.
    note: Option<NoteKey>,
    /// The revision of what the lookup read; `None` when it can't be told,
    /// and then `checked_at` decides.
    revision: Option<ReferenceRevision>,
    /// When it was looked up, in egui input time.
    checked_at: f64,
}

impl CardTitles {
    /// The title last looked up for `card_ref`, if it resolved.
    pub fn get(&self, card_ref: &str) -> Option<&str> {
        self.by_ref.get(card_ref)?.title.as_deref()
    }

    /// Whether the lookup kept for `card_ref` still holds at `now`: nothing it
    /// read has changed since, by `revision` (asked with the key of the note
    /// it resolved to, see
    /// [`AppRegistries::reference_revision`](notedeck::AppRegistries::reference_revision)),
    /// or, for a lookup nothing versions, it was made within
    /// [`UNVERSIONED_REFRESH_SECS`]. A lookup "after" `now` (a fresh egui
    /// context restarts its clock) doesn't hold.
    pub fn is_current(
        &self,
        card_ref: &str,
        now: f64,
        revision: impl FnOnce(Option<NoteKey>) -> Option<ReferenceRevision>,
    ) -> bool {
        let Some(kept) = self.by_ref.get(card_ref) else {
            return false;
        };
        match kept.revision {
            Some(at) => revision(kept.note) == Some(at),
            None => (0.0..UNVERSIONED_REFRESH_SECS).contains(&(now - kept.checked_at)),
        }
    }

    /// Whether the lookup kept for `card_ref` still holds at `now`, checked
    /// through `registries` (see [`is_current`](Self::is_current)).
    pub fn is_current_in(
        &self,
        registries: &AppRegistries,
        ctx: &ReferenceResolveCtx,
        card_ref: &str,
        now: f64,
    ) -> bool {
        self.is_current(card_ref, now, |note| {
            registries.reference_revision(card_ref, ctx, note)
        })
    }

    /// Look `card_ref` up through `registries` and [`store`](Self::store) it,
    /// unless the lookup kept for it still holds (see
    /// [`is_current`](Self::is_current)). Returns whether what it found
    /// changed, as `store` does.
    pub fn refresh(
        &mut self,
        registries: &AppRegistries,
        ctx: &ReferenceResolveCtx,
        card_ref: &str,
        now: f64,
    ) -> bool {
        if self.is_current_in(registries, ctx, card_ref, now) {
            return false;
        }
        self.store(card_ref, CardLookup::of(registries, ctx, card_ref), now)
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

    /// Record a lookup of `card_ref` made at `now`. Returns whether it found
    /// a title or outline other than what was kept, so the caller regroups: a
    /// card's parent links successor chains (see
    /// [`SessionManager::lineage_changed`](crate::session::SessionManager::lineage_changed)).
    /// A lookup that only moved the revision changed nothing shown.
    pub fn store(&mut self, card_ref: &str, lookup: CardLookup, now: f64) -> bool {
        let CardLookup {
            title,
            outline,
            note,
            revision,
        } = lookup;
        match self.by_ref.get_mut(card_ref) {
            Some(entry) => {
                let changed = entry.title != title || entry.outline != outline;
                entry.title = title;
                entry.outline = outline;
                entry.note = note;
                entry.revision = revision;
                entry.checked_at = now;
                changed
            }
            None => {
                self.by_ref.insert(
                    card_ref.to_owned(),
                    CardTitle {
                        title,
                        outline,
                        note,
                        revision,
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
    /// The note the ref resolved to, `None` when it didn't.
    pub note: Option<NoteKey>,
    /// The revision of what the lookup read, `None` when it can't be told.
    pub revision: Option<ReferenceRevision>,
}

impl CardLookup {
    /// The current title and outline of what `card_ref` names, through the
    /// registered reference parsers and the kind renderer of the note it
    /// resolves to (see [`AppRegistries::lookup_reference`] and
    /// [`KindRendererRegistry::title`](notedeck::KindRendererRegistry::title)),
    /// with the revision of what that read. Empty when it doesn't resolve.
    pub fn of(registries: &AppRegistries, ctx: &ReferenceResolveCtx, card_ref: &str) -> Self {
        let renderers = &registries.kind_renderers;
        let ReferenceLookup {
            value,
            note,
            revision,
        } = registries.lookup_reference(card_ref, ctx, |card| CardLookup {
            title: renderers.title(ctx.ndb, ctx.txn, card, None),
            outline: renderers.outline(ctx.ndb, ctx.txn, card, None),
            ..CardLookup::default()
        });
        CardLookup {
            note,
            revision,
            ..value.unwrap_or_default()
        }
    }

    /// A lookup that found `title` and nothing else, and that nothing
    /// versions.
    pub fn titled(title: &str) -> Self {
        CardLookup {
            title: Some(title.to_owned()),
            ..CardLookup::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A revision a stand-in parser and renderer report.
    fn rev(resolve: u64, read: Option<u64>) -> Option<ReferenceRevision> {
        Some(ReferenceRevision { resolve, read })
    }

    /// A lookup that found `title`, read at `revision`.
    fn versioned(title: &str, revision: Option<ReferenceRevision>) -> CardLookup {
        CardLookup {
            revision,
            ..CardLookup::titled(title)
        }
    }

    #[test]
    fn a_versioned_lookup_holds_until_its_revision_moves() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        assert!(
            !titles.is_current(r, 10.0, |_| rev(1, Some(1))),
            "never looked up"
        );
        titles.store(r, versioned("UT5: the HUD", rev(1, Some(1))), 10.0);
        assert_eq!(titles.get(r), Some("UT5: the HUD"));
        assert!(
            titles.is_current(r, 10.0 + 3600.0, |_| rev(1, Some(1))),
            "time alone never stales a versioned lookup"
        );
        assert!(
            !titles.is_current(r, 10.0, |_| rev(2, Some(1))),
            "resolve moved"
        );
        assert!(
            !titles.is_current(r, 10.0, |_| rev(1, Some(2))),
            "read moved"
        );
        assert!(
            !titles.is_current(r, 10.0, |_| None),
            "no longer versioned: look again"
        );
    }

    #[test]
    fn the_revision_check_is_asked_about_the_note_it_resolved_to() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        let note = NoteKey::new(42);
        titles.store(
            r,
            CardLookup {
                note: Some(note),
                ..versioned("UT5", rev(1, Some(1)))
            },
            0.0,
        );
        assert!(titles.is_current(r, 0.0, |asked| {
            assert_eq!(asked, Some(note));
            rev(1, Some(1))
        }));
    }

    #[test]
    fn an_unversioned_lookup_is_current_for_its_window_then_stale() {
        let mut titles = CardTitles::default();
        let r = "agentium:tone-grief-hour";
        let unversioned = |_| panic!("an unversioned lookup is never checked by revision");
        titles.store(r, CardLookup::titled("first"), 10.0);
        assert!(titles.is_current(r, 10.0 + UNVERSIONED_REFRESH_SECS - 0.1, unversioned));
        assert!(!titles.is_current(r, 10.0 + UNVERSIONED_REFRESH_SECS, unversioned));
        // A clock that went backwards (a fresh egui context) looks it up again.
        assert!(!titles.is_current(r, 9.0, unversioned));
    }

    #[test]
    fn a_miss_is_kept_but_has_no_title() {
        let mut titles = CardTitles::default();
        let r = "headway:dave/tone-grief-hour";
        titles.store(r, CardLookup::titled("old"), 0.0);
        titles.store(r, versioned_miss(rev(3, None)), 1.0);
        assert_eq!(titles.get(r), None);
        assert!(titles.is_current(r, 1.5, |_| rev(3, None)));
        assert!(
            !titles.is_current(r, 1.5, |_| rev(4, None)),
            "the card may have arrived"
        );
    }

    /// A lookup that resolved to nothing, read at `revision`.
    fn versioned_miss(revision: Option<ReferenceRevision>) -> CardLookup {
        CardLookup {
            revision,
            ..CardLookup::default()
        }
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
            ..CardLookup::default()
        };
        assert!(titles.store(r, under(None), 0.0), "first lookup");
        assert!(!titles.store(r, under(None), 0.5), "same again");
        assert!(titles.store(r, under(Some("headway:dave/epic")), 1.0));
        assert_eq!(titles.parent(r), Some("headway:dave/epic"));
        assert!(!titles.store(r, under(Some("headway:dave/epic")), 2.0));
        assert!(
            !titles.store(
                r,
                CardLookup {
                    revision: rev(9, Some(9)),
                    ..under(Some("headway:dave/epic"))
                },
                3.0
            ),
            "a new revision alone changed nothing shown"
        );
        assert!(
            titles.store(r, CardLookup::titled("UT5"), 3.0),
            "lost its outline"
        );
        assert_eq!(titles.parent(r), None);
    }
}
