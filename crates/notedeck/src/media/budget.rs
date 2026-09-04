//! Byte-budgeted storage and LRU eviction for the in-memory texture caches.
//!
//! Every image notedeck displays is decoded and then uploaded to a GPU texture
//! held alive by an [`egui::TextureHandle`]; egui frees the underlying driver
//! allocation when the last handle for it drops. On unified-memory systems
//! (Apple Silicon) those bytes are ordinary system RAM, and the Metal driver
//! allocates them non-purgeable, so the kernel cannot drop them under pressure
//! — it can only compress and swap them. Without a bound, a long-running
//! session accumulates one live driver allocation per image variant it has ever
//! scrolled past.
//!
//! The bound is therefore measured in *bytes*, not entries: an entry count says
//! nothing about cost, and cost per entry spans four orders of magnitude, from
//! 36 KiB for a 96px avatar to tens of MiB for a long GIF (which holds one
//! texture per frame).
//!
//! # Policy
//!
//! - Eviction is least-recently-used, where "recently" is measured in egui pass
//!   numbers recorded by [`TexEntry::touch`] on every read.
//! - A sweep only runs when the total is over budget, and evicts down to
//!   [`low_water`] rather than to exactly the budget, so that uploading one more
//!   texture does not immediately trigger another sweep.
//! - Only `Loaded` entries are evictable. `Pending` entries have a job in
//!   flight and are what stops that job from being requested again; `Error`
//!   entries hold no GPU memory and are what stops a failing URL from being
//!   retried every frame.
//! - An entry touched within the last [`MIN_UNUSED_PASSES`] passes is never
//!   evicted, so nothing visible can be dropped out from under a paint command.
//!
//! # Where it runs
//!
//! Sweeps happen once per pass from `Notedeck::tick`, before any UI is drawn.
//! Dropping a handle mid-pass would in fact be safe — egui defers the free into
//! `TexturesDelta::free`, and `egui-wgpu` applies those only after
//! `Queue::submit` — but sweeping before the UI runs means the invariant does
//! not depend on that backend detail.

use std::cell::Cell;

use egui::TextureHandle;
use hashbrown::HashMap;

use crate::media::images::TextureRequestVariant;
use crate::{Animation, TextureState};

/// Default ceiling on GPU texture memory.
///
/// A long-running desktop session was measured holding 4.5 GiB across 6862 live
/// driver allocations, i.e. ~670 KiB per texture, so 512 MiB bounds the working
/// set to roughly 800 textures — several screens of scrollback in each of a
/// handful of columns — while cutting the observed footprint by an order of
/// magnitude. Phones show one column at a time and have far less memory to give
/// away, so they get a quarter of that.
///
/// Written as `cfg!` rather than `#[cfg]` so both values are type-checked on
/// every target.
pub const DEFAULT_TEXTURE_BUDGET: usize = if cfg!(any(target_os = "android", target_os = "ios")) {
    128 * 1024 * 1024
} else {
    512 * 1024 * 1024
};

/// How many passes an entry must go untouched before it may be evicted.
///
/// This is a safety floor rather than a retention policy — retention comes from
/// the budget, and LRU order means a sweep takes the coldest entries first. The
/// floor exists because egui may run several passes per frame and discard their
/// output, so "not touched during the pass that is about to start" is not on its
/// own enough to prove a texture is off screen.
pub const MIN_UNUSED_PASSES: u64 = 3;

/// The level a sweep evicts down to, as a fraction of `budget`.
///
/// Leaving headroom means a sweep is followed by many quiet passes rather than
/// by another sweep on the very next upload.
pub fn low_water(budget: usize) -> usize {
    budget / 8 * 7
}

/// Texture payloads that can report how much GPU memory they hold.
pub trait TextureBytes {
    fn texture_bytes(&self) -> usize;
}

impl TextureBytes for TextureHandle {
    fn texture_bytes(&self) -> usize {
        self.byte_size()
    }
}

impl TextureBytes for Animation {
    /// An animation holds one texture per frame, so its cost is the sum of all
    /// of them — this is the single biggest reason the budget counts bytes
    /// rather than entries.
    fn texture_bytes(&self) -> usize {
        self.first_frame.texture.byte_size()
            + self
                .other_frames
                .iter()
                .map(|frame| frame.texture.byte_size())
                .sum::<usize>()
    }
}

/// A cached texture plus the bookkeeping needed to evict it under a budget.
pub struct TexEntry<T> {
    state: TextureState<T>,

    /// GPU bytes this entry holds, sampled once when it was stored.
    ///
    /// Sampled rather than recomputed on demand because
    /// [`egui::TextureHandle::byte_size`] locks the texture manager, and an
    /// animation would take that lock once per frame of the GIF.
    bytes: usize,

    /// The pass number this entry was last handed to a caller on.
    ///
    /// A [`Cell`] because textures are handed out through `&self`: the render
    /// path holds a shared borrow of the cache for as long as it holds the
    /// texture reference it is about to paint, so there is no `&mut` available
    /// at the point of use.
    last_used: Cell<u64>,
}

impl<T: TextureBytes> TexEntry<T> {
    /// Stores `state`, sampling its texture cost and marking it as used on
    /// `pass_nr` so that a sweep in this same pass cannot immediately evict a
    /// texture that was just uploaded.
    pub fn new(state: TextureState<T>, pass_nr: u64) -> Self {
        let bytes = match &state {
            TextureState::Loaded(payload) => payload.texture_bytes(),
            TextureState::Pending | TextureState::Error(_) => 0,
        };

        Self {
            state,
            bytes,
            last_used: Cell::new(pass_nr),
        }
    }
}

impl<T> TexEntry<T> {
    /// Records that this entry was read during `pass_nr` and returns its state.
    pub fn touch(&self, pass_nr: u64) -> &TextureState<T> {
        self.last_used.set(pass_nr);
        &self.state
    }

    /// The state without recording a read. For callers that are inspecting the
    /// cache rather than about to paint from it.
    pub fn peek(&self) -> &TextureState<T> {
        &self.state
    }

    /// GPU bytes this entry holds; zero unless it is `Loaded`.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Describes this entry to a sweep of `current_pass`, or `None` if the
    /// sweep may not drop it.
    ///
    /// See the module docs for why `Pending` and `Error` are retained.
    pub fn as_candidate(&self, current_pass: u64) -> Option<EvictCandidate> {
        if !matches!(self.state, TextureState::Loaded(_)) {
            return None;
        }

        let last_used = self.last_used.get();
        if last_used + MIN_UNUSED_PASSES > current_pass {
            return None;
        }

        Some(EvictCandidate {
            last_used,
            bytes: self.bytes,
        })
    }
}

/// A texture cache keyed by URL and then by request variant.
///
/// One URL can be held at several sizes at once — `Full`, a `Hint` snapped to
/// 128px for whatever column width asked for it, and one per profile-picture
/// size — so the variant map is where the textures live and the URL map is
/// where a URL's identity lives.
///
/// Eviction deliberately drops variant entries but leaves the URL key in place,
/// because [`crate::Images::user_trusts_img`] reads the presence of a URL key as
/// "the user has already chosen to load this image". Dropping the key would
/// re-obfuscate media the user had revealed, so a swept URL keeps its (empty)
/// variant map.
pub struct VariantTexCache<T> {
    urls: HashMap<String, HashMap<TextureRequestVariant, TexEntry<T>>>,

    /// Running total of `TexEntry::bytes` over all `Loaded` entries.
    ///
    /// Maintained incrementally so that the common in-budget case costs a
    /// comparison rather than a walk of every entry.
    loaded_bytes: usize,
}

impl<T> Default for VariantTexCache<T> {
    fn default() -> Self {
        Self {
            urls: Default::default(),
            loaded_bytes: 0,
        }
    }
}

impl<T: TextureBytes> VariantTexCache<T> {
    /// Reads a variant, recording it as used during `pass_nr`.
    pub fn get(
        &self,
        url: &str,
        variant: TextureRequestVariant,
        pass_nr: u64,
    ) -> Option<&TextureState<T>> {
        Some(self.urls.get(url)?.get(&variant)?.touch(pass_nr))
    }

    /// Whether any variant of `url` has ever been requested.
    ///
    /// Stays true across eviction; see the type docs.
    pub fn contains(&self, url: &str) -> bool {
        self.urls.contains_key(url)
    }

    /// Stores `state` for `url`'s `variant`, replacing whatever was there.
    pub fn set_state(
        &mut self,
        url: String,
        variant: TextureRequestVariant,
        state: TextureState<T>,
        pass_nr: u64,
    ) {
        let entry = TexEntry::new(state, pass_nr);
        self.loaded_bytes += entry.bytes();

        if let Some(replaced) = self.urls.entry(url).or_default().insert(variant, entry) {
            self.loaded_bytes = self.loaded_bytes.saturating_sub(replaced.bytes());
        }
    }

    /// GPU bytes currently held by loaded entries.
    pub fn loaded_bytes(&self) -> usize {
        self.loaded_bytes
    }

    /// Number of loaded entries, for diagnostics.
    pub fn loaded_count(&self) -> usize {
        self.urls
            .values()
            .flat_map(|variants| variants.values())
            .filter(|entry| matches!(entry.peek(), TextureState::Loaded(_)))
            .count()
    }

    /// Appends `(last_used, bytes)` for every entry a sweep of `current_pass`
    /// could drop, so the caller can pick a cutoff across several caches at
    /// once without cloning any keys.
    pub fn collect_evictable(&self, current_pass: u64, out: &mut Vec<EvictCandidate>) {
        for variants in self.urls.values() {
            out.extend(
                variants
                    .values()
                    .filter_map(|entry| entry.as_candidate(current_pass)),
            );
        }
    }

    /// Drops evictable entries last used at or before `cutoff_pass`, stopping
    /// once `to_free` bytes have been released. Returns the bytes freed.
    pub fn evict_until(&mut self, current_pass: u64, cutoff_pass: u64, to_free: usize) -> usize {
        let mut freed = 0;

        for variants in self.urls.values_mut() {
            if freed >= to_free {
                break;
            }

            variants.retain(|_variant, entry| {
                if freed >= to_free {
                    return true;
                }
                let Some(candidate) = entry.as_candidate(current_pass) else {
                    return true;
                };
                if candidate.last_used > cutoff_pass {
                    return true;
                }

                freed += candidate.bytes;
                false
            });
        }

        self.loaded_bytes = self.loaded_bytes.saturating_sub(freed);
        freed
    }

    /// Drops every entry and its textures.
    pub fn clear(&mut self) {
        self.urls.clear();
        self.loaded_bytes = 0;
    }
}

/// The cost and age of one entry a sweep may drop.
///
/// Keys are deliberately absent: a sweep sorts these to find a cutoff pass and
/// then re-walks the caches to apply it, which avoids cloning a URL for every
/// candidate.
pub struct EvictCandidate {
    pub last_used: u64,
    pub bytes: usize,
}

/// Picks the pass number a sweep should evict up to, and how many bytes it
/// should free, given every candidate across all caches.
///
/// Returns `None` when the budget cannot be met by evicting candidates — the
/// working set genuinely exceeds the budget — in which case going over budget
/// is preferable to evicting textures that are still on screen.
pub fn plan_sweep(
    candidates: &mut [EvictCandidate],
    total_bytes: usize,
    budget: usize,
) -> Option<SweepPlan> {
    let target = low_water(budget);
    let to_free = total_bytes.saturating_sub(target);
    if to_free == 0 {
        return None;
    }

    candidates.sort_unstable_by_key(|candidate| candidate.last_used);

    let mut freeable = 0;
    for candidate in candidates.iter() {
        freeable += candidate.bytes;
        if freeable >= to_free {
            return Some(SweepPlan {
                cutoff_pass: candidate.last_used,
                to_free,
            });
        }
    }

    // Not enough cold bytes to reach the target. Free what we can: this is the
    // oversubscribed case, and leaving the coldest textures resident would mean
    // never recovering.
    candidates.last().map(|warmest| SweepPlan {
        cutoff_pass: warmest.last_used,
        to_free: freeable,
    })
}

/// The cutoff a sweep applies to every cache.
pub struct SweepPlan {
    /// Entries last used at or before this pass may be dropped.
    pub cutoff_pass: u64,
    /// Stop once this many bytes have been freed.
    pub to_free: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload whose cost we can set, so the policy can be tested without a
    /// GPU or an egui context.
    struct FakeTexture(usize);

    impl TextureBytes for FakeTexture {
        fn texture_bytes(&self) -> usize {
            self.0
        }
    }

    fn loaded(bytes: usize, pass_nr: u64) -> TexEntry<FakeTexture> {
        TexEntry::new(TextureState::Loaded(FakeTexture(bytes)), pass_nr)
    }

    fn cache_with(entries: &[(&str, usize, u64)]) -> VariantTexCache<FakeTexture> {
        let mut cache = VariantTexCache::default();
        for (url, bytes, pass_nr) in entries {
            cache.set_state(
                (*url).to_owned(),
                TextureRequestVariant::Full,
                TextureState::Loaded(FakeTexture(*bytes)),
                *pass_nr,
            );
        }
        cache
    }

    #[test]
    fn entry_is_not_evictable_until_it_has_aged_past_the_floor() {
        let entry = loaded(1024, 100);

        assert!(entry.as_candidate(100).is_none(), "used this pass");
        assert!(
            entry.as_candidate(100 + MIN_UNUSED_PASSES - 1).is_none(),
            "still inside the safety floor"
        );
        assert!(entry.as_candidate(100 + MIN_UNUSED_PASSES).is_some());
    }

    #[test]
    fn touch_resets_an_entrys_age() {
        let entry = loaded(1024, 10);
        assert!(entry.as_candidate(100).is_some());

        entry.touch(100);
        assert!(entry.as_candidate(100).is_none());
    }

    #[test]
    fn pending_and_error_entries_are_never_evictable() {
        let pending: TexEntry<FakeTexture> = TexEntry::new(TextureState::Pending, 0);
        let errored: TexEntry<FakeTexture> =
            TexEntry::new(TextureState::Error(crate::Error::Generic("nope".into())), 0);

        assert!(pending.as_candidate(u64::MAX / 2).is_none());
        assert!(errored.as_candidate(u64::MAX / 2).is_none());
        assert_eq!(pending.bytes(), 0);
        assert_eq!(errored.bytes(), 0);
    }

    #[test]
    fn loaded_bytes_tracks_inserts_and_replacements() {
        let mut cache = cache_with(&[("a", 100, 1), ("b", 250, 1)]);
        assert_eq!(cache.loaded_bytes(), 350);

        // Re-delivering the same variant at a different size must not
        // double-count the entry it replaces.
        cache.set_state(
            "a".to_owned(),
            TextureRequestVariant::Full,
            TextureState::Loaded(FakeTexture(400)),
            2,
        );
        assert_eq!(cache.loaded_bytes(), 650);
    }

    #[test]
    fn loaded_bytes_tracks_variants_of_one_url_separately() {
        let mut cache = VariantTexCache::default();
        cache.set_state(
            "a".to_owned(),
            TextureRequestVariant::Full,
            TextureState::Loaded(FakeTexture(100)),
            1,
        );
        cache.set_state(
            "a".to_owned(),
            TextureRequestVariant::Profile(64),
            TextureState::Loaded(FakeTexture(7)),
            1,
        );

        assert_eq!(cache.loaded_bytes(), 107);
        assert_eq!(cache.loaded_count(), 2);
    }

    #[test]
    fn an_animation_costs_the_sum_of_all_its_frames() {
        // epaint's texture manager is pure CPU, so this measures real
        // TextureHandles without needing a GPU.
        let ctx = egui::Context::default();
        let frame = |name: &str, side: usize| crate::TextureFrame {
            delay: std::time::Duration::from_millis(50),
            texture: crate::media::load_texture_checked(
                &ctx,
                name,
                egui::ColorImage::new([side, side], egui::Color32::RED),
                Default::default(),
            ),
        };

        // 4 bytes per pixel: 16x16 is 1024 bytes, 8x8 is 256.
        let animation = Animation {
            first_frame: frame("first", 16),
            other_frames: vec![frame("second", 8), frame("third", 8)],
        };

        assert_eq!(
            animation.texture_bytes(),
            1024 + 256 + 256,
            "a GIF's cost is every frame it holds, not just the one on screen"
        );

        // And the entry must bill the whole animation, since eviction drops all
        // of an animation's frames or none of them.
        let entry = TexEntry::new(TextureState::Loaded(animation), 0);
        assert_eq!(entry.bytes(), 1536);
    }

    #[test]
    fn sweep_plan_is_none_when_under_the_low_water_mark() {
        let mut candidates = vec![EvictCandidate {
            last_used: 0,
            bytes: 100,
        }];

        // 800 is under low_water(1000) == 875, so there is nothing to do even
        // though a cold candidate exists.
        assert!(plan_sweep(&mut candidates, 800, 1000).is_none());
    }

    #[test]
    fn sweep_plan_frees_down_to_the_low_water_mark_not_just_to_budget() {
        let mut candidates = vec![
            EvictCandidate {
                last_used: 5,
                bytes: 100,
            },
            EvictCandidate {
                last_used: 1,
                bytes: 100,
            },
            EvictCandidate {
                last_used: 3,
                bytes: 100,
            },
        ];

        let plan =
            plan_sweep(&mut candidates, 1000, 1000).expect("at budget, still over low water");

        // low_water(1000) == 875, so 125 bytes must go: the two coldest
        // candidates (passes 1 and 3), leaving the cutoff at pass 3.
        assert_eq!(plan.to_free, 125);
        assert_eq!(plan.cutoff_pass, 3);
    }

    #[test]
    fn sweep_plan_takes_the_coldest_candidates_first() {
        let mut candidates = vec![
            EvictCandidate {
                last_used: 90,
                bytes: 500,
            },
            EvictCandidate {
                last_used: 10,
                bytes: 500,
            },
        ];

        let plan = plan_sweep(&mut candidates, 1200, 1000).expect("over budget");

        // Only 325 bytes are needed, which the pass-10 candidate covers alone,
        // so the warm pass-90 candidate must stay out of the cutoff.
        assert_eq!(plan.cutoff_pass, 10);
    }

    #[test]
    fn sweep_plan_frees_everything_cold_when_the_working_set_exceeds_the_budget() {
        let mut candidates = vec![EvictCandidate {
            last_used: 1,
            bytes: 10,
        }];

        // 10 cold bytes cannot get 5000 down to 875, but freeing them is still
        // better than giving up.
        let plan = plan_sweep(&mut candidates, 5000, 1000).expect("should free what it can");
        assert_eq!(plan.to_free, 10);
        assert_eq!(plan.cutoff_pass, 1);
    }

    #[test]
    fn sweep_plan_is_none_when_nothing_is_cold_enough() {
        let mut candidates: Vec<EvictCandidate> = Vec::new();
        assert!(plan_sweep(&mut candidates, 5000, 1000).is_none());
    }

    #[test]
    fn evict_until_stops_once_it_has_freed_enough() {
        let mut cache = cache_with(&[("a", 100, 1), ("b", 100, 1), ("c", 100, 1)]);
        let current = 1 + MIN_UNUSED_PASSES;

        // Asking for 100 must not clear the whole cache just because every
        // entry shares the cutoff pass.
        let freed = cache.evict_until(current, 1, 100);

        assert_eq!(freed, 100);
        assert_eq!(cache.loaded_bytes(), 200);
        assert_eq!(cache.loaded_count(), 2);
    }

    #[test]
    fn evict_until_leaves_entries_warmer_than_the_cutoff() {
        let mut cache = cache_with(&[("cold", 100, 1), ("warm", 100, 50)]);
        let current = 50 + MIN_UNUSED_PASSES;

        let freed = cache.evict_until(current, 1, usize::MAX);

        assert_eq!(freed, 100);
        assert!(!cache.contains_variant("cold"));
        assert!(cache.contains_variant("warm"));
    }

    #[test]
    fn evicted_urls_keep_their_key_so_media_stays_trusted() {
        let mut cache = cache_with(&[("https://example.com/a.png", 100, 1)]);

        cache.evict_until(1 + MIN_UNUSED_PASSES, 1, usize::MAX);

        assert_eq!(cache.loaded_bytes(), 0);
        assert!(
            cache.contains("https://example.com/a.png"),
            "Images::user_trusts_img reads this key to decide whether the user \
             already revealed the media, so eviction must not drop it"
        );
    }

    #[test]
    fn get_records_a_use_and_protects_the_entry() {
        let cache = cache_with(&[("a", 100, 1)]);
        let current = 1 + MIN_UNUSED_PASSES;

        let mut candidates = Vec::new();
        cache.collect_evictable(current, &mut candidates);
        assert_eq!(candidates.len(), 1, "cold before it is read");

        cache
            .get("a", TextureRequestVariant::Full, current)
            .expect("still cached");

        candidates.clear();
        cache.collect_evictable(current, &mut candidates);
        assert!(candidates.is_empty(), "reading it made it warm again");
    }

    impl<T> VariantTexCache<T> {
        /// Whether `url` still holds its `Full` variant. Test-only: production
        /// code goes through `get`, which also records a use.
        fn contains_variant(&self, url: &str) -> bool {
            self.urls
                .get(url)
                .is_some_and(|variants| variants.contains_key(&TextureRequestVariant::Full))
        }
    }
}
