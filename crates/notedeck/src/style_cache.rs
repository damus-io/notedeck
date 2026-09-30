//! Prebuilt [`egui::Style`] variants, so a per-note spacing tweak is an `Arc`
//! bump instead of a deep `Style` clone.
//!
//! # Why this exists
//!
//! `Ui::spacing_mut()` does not look like an allocation, and it is one of the
//! most expensive calls in a notedeck frame. It is
//! `&mut Ui::style_mut().spacing`, and `Ui::style_mut` is
//! `Arc::make_mut(&mut self.style)`. A child `Ui` shares its style `Arc` with
//! its parent, so the first mutation in any `Ui` clones an entire `Style` —
//! roughly 620 bytes of struct plus the `text_styles` map behind it.
//!
//! The note-rendering path does that seven times per visible note. Measured by
//! `crates/notedeck_columns/tests/frame_alloc.rs`, that was **106 allocations
//! and ~66 KB per frame** on a seven-note timeline: a quarter of the frame's
//! bytes, and the single largest thing in the frame that was notedeck's own
//! code. It scales with window height, because "seven times per visible note"
//! does.
//!
//! Every one of those sites wanted the inherited style with a different
//! `item_spacing`. So build each distinct variant once, keep it, and hand the
//! same `Arc` to every note: [`StyleCache::item_spacing`] and friends.
//!
//! # Correctness, and why identity is not enough
//!
//! `Ui::set_style` replaces rather than merges, so a variant is only valid for
//! the exact style it was derived from — a site that sets `item_spacing.x`
//! still has to inherit everything else from whatever `Ui` it is drawing into.
//! Entries are therefore keyed on the base style, and each entry owns an `Arc`
//! clone of its base.
//!
//! The first key is `Arc` pointer equality, which is exact and free. Holding
//! the base alive is what makes that comparison sound: a freed `Style` could be
//! replaced at the same address by an unrelated one, and a cached entry would
//! then match the wrong base.
//!
//! Identity alone does not survive a frame, though. Anything upstream that
//! touches a `Style` hands everything below it a *new* `Arc` — in the Columns
//! timeline it is `egui_extras::StripLayout::cell` setting `wrap_mode` on the
//! cell each column is drawn into, once a frame. The note sites are a chain,
//! each deriving from the level above, so one changed `Arc` at the top means
//! every level below misses too and the cache never warms up. So a pointer miss
//! falls back to comparing the base's *contents*, and rebases the entry onto
//! the new `Arc` when they match. Two styles that are equal derive equal
//! variants, so the cached one is the right answer.
//!
//! That leaves one `Style` comparison per frame where there were a hundred
//! clones, and it is what takes the steady-state frame from 991 allocations to
//! 885.
//!
//! # The other thing in here: egui's named styles
//!
//! egui spells a style that is not one of its built-ins with an `Arc<str>` —
//! `TextStyle::Name("NoteBody".into())`, `FontFamily::Name("medium".into())`.
//! Notedeck's [`NotedeckTextStyle`] and [`NamedFontFamily`] are enums of
//! `&'static str`s, so every `.text_style()` in a widget built one of those
//! `Arc`s from a literal and dropped it at the end of the frame: 14
//! allocations a frame on a seven-note timeline, two per note.
//!
//! An `Arc<str>` built once and cloned is a refcount bump, so
//! [`StyleCache::text_style`] and [`StyleCache::font_family`] hand out clones
//! of a set built in [`StyleCache::new`]. They are not a cache — there is
//! nothing to invalidate, the strings are literals — and they are here rather
//! than in a `OnceLock` of their own because a global is not allowed (CLAUDE.md
//! rule 6) and this is the struct that is already on the note path. The cold
//! callers of `NotedeckTextStyle::text_style`, which cannot all reach a
//! `StyleCache` without threading one through half the workspace, still build
//! their own; that is fine, they run once per view rather than once per note.
//!
//! # Lifetime
//!
//! One instance lives on the [`Notedeck`](crate::Notedeck) host and is reached
//! through [`AppContext::style_cache`](crate::AppContext::style_cache) and
//! [`NoteContext::style_cache`](crate::NoteContext::style_cache). It is state
//! passed in by reference rather than a global, per CLAUDE.md.

use crate::{NamedFontFamily, NotedeckTextStyle};
use egui::{FontFamily, Style, TextStyle, Ui, Vec2};
use std::sync::Arc;
use strum::IntoEnumIterator;

/// How many derived styles to keep before starting over.
///
/// The note path needs six. The cap is here so that a caller feeding spacings
/// off a continuous value (an animation, a drag) cannot grow the cache without
/// bound; it clears rather than evicting one entry, because at this size a
/// rebuild is cheap and a correct LRU is not worth the code.
const MAX_VARIANTS: usize = 16;

/// One derived style and the base it is only valid against.
struct Variant {
    /// The style this was derived from. Owned, so the address `derive`
    /// compares against cannot be reused by a different `Style`, and rebased in
    /// place when an equal `Style` arrives at a new address — see the module
    /// docs.
    base: Arc<Style>,

    /// `item_spacing` as raw bits, so the key is an exact comparison rather
    /// than a float one.
    item_spacing: [u32; 2],

    /// `base` with `spacing.item_spacing` replaced.
    derived: Arc<Style>,
}

/// Build one `T` per variant of `E`, indexed by the variant's discriminant.
///
/// Filled by index rather than collected in iteration order, so the lookups in
/// [`StyleCache::text_style`] and [`StyleCache::font_family`] are right by
/// construction and a variant carrying an out-of-range discriminant would panic
/// here, at startup, rather than return the wrong style.
fn intern<E, T>(index: impl Fn(E) -> usize, build: impl Fn(E) -> T) -> Box<[T]>
where
    E: IntoEnumIterator + Copy,
{
    let mut out: Vec<Option<T>> = (0..E::iter().count()).map(|_| None).collect();
    for variant in E::iter() {
        out[index(variant)] = Some(build(variant));
    }
    out.into_iter()
        .map(|built| built.expect("every discriminant is its own index"))
        .collect()
}

/// A small set of [`egui::Style`] variants, built once and reused.
///
/// See the [module docs](self) for what this is for and why it is keyed the way
/// it is.
pub struct StyleCache {
    variants: Vec<Variant>,

    /// One [`egui::TextStyle`] per [`NotedeckTextStyle`], indexed by the
    /// variant's discriminant.
    text_styles: Box<[TextStyle]>,

    /// One [`egui::FontFamily`] per [`NamedFontFamily`], indexed by the
    /// variant's discriminant.
    font_families: Box<[FontFamily]>,
}

impl Default for StyleCache {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleCache {
    pub fn new() -> Self {
        Self {
            // Reserved up front so a cold miss does not also pay for growth.
            variants: Vec::with_capacity(MAX_VARIANTS),
            text_styles: intern(
                |style: NotedeckTextStyle| style as usize,
                |style| style.text_style(),
            ),
            font_families: intern(
                |family: NamedFontFamily| family as usize,
                |family| FontFamily::Name(family.as_str().into()),
            ),
        }
    }

    /// The `egui::TextStyle` for `style`, without building its name again.
    ///
    /// Same value as [`NotedeckTextStyle::text_style`], but the named styles
    /// (`Heading2`, `NoteBody`, ...) are an `Arc` bump rather than an
    /// `Arc<str>` allocation. Per-frame code wants this one; see the [module
    /// docs](self).
    pub fn text_style(&self, style: NotedeckTextStyle) -> TextStyle {
        self.text_styles[style as usize].clone()
    }

    /// The `egui::FontFamily` for `family`, without building its name again.
    ///
    /// The font-family counterpart of [`text_style`](Self::text_style).
    pub fn font_family(&self, family: NamedFontFamily) -> FontFamily {
        self.font_families[family as usize].clone()
    }

    /// Give `ui` the style it already has, with `item_spacing` replaced.
    ///
    /// Equivalent to `ui.spacing_mut().item_spacing = item_spacing`, but the
    /// second and later calls with the same spacing and the same inherited
    /// style cost an `Arc` clone instead of a `Style` clone.
    pub fn item_spacing(&mut self, ui: &mut Ui, item_spacing: Vec2) {
        if ui.spacing().item_spacing == item_spacing {
            return;
        }

        let derived = self.derive(ui.style(), item_spacing);
        ui.set_style(derived);
    }

    /// [`item_spacing`](Self::item_spacing), keeping the inherited `y`.
    pub fn item_spacing_x(&mut self, ui: &mut Ui, x: f32) {
        let mut item_spacing = ui.spacing().item_spacing;
        item_spacing.x = x;
        self.item_spacing(ui, item_spacing);
    }

    /// [`item_spacing`](Self::item_spacing), keeping the inherited `x`.
    pub fn item_spacing_y(&mut self, ui: &mut Ui, y: f32) {
        let mut item_spacing = ui.spacing().item_spacing;
        item_spacing.y = y;
        self.item_spacing(ui, item_spacing);
    }

    /// `base` with `item_spacing` replaced, from the cache if it is there.
    fn derive(&mut self, base: &Arc<Style>, item_spacing: Vec2) -> Arc<Style> {
        let key = [item_spacing.x.to_bits(), item_spacing.y.to_bits()];

        if let Some(variant) = self
            .variants
            .iter()
            .find(|variant| variant.item_spacing == key && Arc::ptr_eq(&variant.base, base))
        {
            return variant.derived.clone();
        }

        // A different allocation, but possibly an identical `Style`. egui hands
        // out a fresh `Style` whenever anything upstream mutates one —
        // `egui_extras::StripLayout::cell` sets `wrap_mode` on every cell it
        // draws, which is once a frame above every column — and everything
        // below inherits that new `Arc`. Without this the cache would miss on
        // the first note of every frame and then miss all the way down, because
        // each level rebases on the level above. Rebasing the entry keeps one
        // deep `Style` comparison in place of a cascade of clones.
        if let Some(variant) = self
            .variants
            .iter_mut()
            .find(|variant| variant.item_spacing == key && *variant.base == **base)
        {
            variant.base = base.clone();
            return variant.derived.clone();
        }

        let mut style = (**base).clone();
        style.spacing.item_spacing = item_spacing;
        let derived = Arc::new(style);

        if self.variants.len() >= MAX_VARIANTS {
            self.variants.clear();
        }
        self.variants.push(Variant {
            base: base.clone(),
            item_spacing: key,
            derived: derived.clone(),
        });

        derived
    }

    /// How many variants are currently held. For tests.
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.variants.len()
    }

    /// Whether nothing has been derived yet. For tests.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `f` over a fresh `Ui` and hand back what the cache did.
    fn with_ui(cache: &mut StyleCache, mut f: impl FnMut(&mut StyleCache, &mut Ui)) -> Arc<Style> {
        let ctx = egui::Context::default();
        let mut out = None;
        ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                f(cache, ui);
                out = Some(ui.style().clone());
            });
        })
        .drop_without_applying_deltas();
        out.expect("panel ran")
    }

    #[test]
    fn sets_the_spacing_it_was_asked_for() {
        let mut cache = StyleCache::new();
        let style = with_ui(&mut cache, |cache, ui| {
            cache.item_spacing_x(ui, 13.0);
        });
        assert_eq!(style.spacing.item_spacing.x, 13.0);
    }

    /// The point of the whole thing: the second note reuses the first note's
    /// style rather than cloning one of its own.
    #[test]
    fn reuses_the_same_arc_across_uis() {
        let ctx = egui::Context::default();
        let mut cache = StyleCache::new();
        let mut styles = Vec::new();

        ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                for _ in 0..3 {
                    ui.horizontal(|ui| {
                        cache.item_spacing_x(ui, 4.0);
                        styles.push(ui.style().clone());
                    });
                }
            });
        })
        .drop_without_applying_deltas();

        assert_eq!(cache.len(), 1, "one variant for one spacing");
        assert!(Arc::ptr_eq(&styles[0], &styles[1]));
        assert!(Arc::ptr_eq(&styles[1], &styles[2]));
    }

    /// Setting the spacing that is already inherited is a no-op, not a variant.
    #[test]
    fn inherited_spacing_derives_nothing() {
        let mut cache = StyleCache::new();
        let ctx = egui::Context::default();
        let inherited = ctx.global_style().spacing.item_spacing;

        with_ui(&mut cache, |cache, ui| {
            cache.item_spacing(ui, inherited);
        });

        assert!(cache.is_empty());
    }

    /// A variant is only handed back for the base it was derived from, so a
    /// site that tweaks `x` keeps everything else it would have inherited.
    #[test]
    fn a_different_base_gets_a_different_variant() {
        let ctx = egui::Context::default();
        let mut cache = StyleCache::new();
        let mut derived = Vec::new();

        ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    cache.item_spacing_x(ui, 4.0);
                    derived.push(ui.style().clone());
                });
                ui.horizontal(|ui| {
                    // a base the cache has never seen
                    ui.style_mut().visuals.button_frame = false;
                    cache.item_spacing_x(ui, 4.0);
                    derived.push(ui.style().clone());
                });
            });
        })
        .drop_without_applying_deltas();

        assert_eq!(cache.len(), 2);
        assert!(!Arc::ptr_eq(&derived[0], &derived[1]));
        assert!(derived[0].visuals.button_frame);
        assert!(!derived[1].visuals.button_frame);
        assert_eq!(derived[1].spacing.item_spacing.x, 4.0);
    }

    /// The case the note path actually hits: something upstream clones the
    /// style every frame, so the base arrives at a new address with the same
    /// contents. The entry rebases rather than the cache growing a copy.
    #[test]
    fn an_equal_base_at_a_new_address_rebases() {
        let ctx = egui::Context::default();
        let mut cache = StyleCache::new();
        let mut derived = Vec::new();

        ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                for _ in 0..3 {
                    ui.horizontal(|ui| {
                        // Force a fresh `Arc` holding an identical `Style`,
                        // the way `egui_extras::StripLayout::cell` does.
                        let same = (**ui.style()).clone();
                        ui.set_style(same);

                        cache.item_spacing_x(ui, 4.0);
                        derived.push(ui.style().clone());
                    });
                }
            });
        })
        .drop_without_applying_deltas();

        assert_eq!(cache.len(), 1, "rebased, not copied");
        assert!(Arc::ptr_eq(&derived[0], &derived[1]));
        assert!(Arc::ptr_eq(&derived[1], &derived[2]));
    }

    /// The point of the interning: the same `Arc<str>` comes back, so a call
    /// per note per frame is a refcount bump rather than an allocation.
    #[test]
    fn a_named_text_style_hands_back_the_same_arc() {
        let cache = StyleCache::new();

        let (TextStyle::Name(first), TextStyle::Name(second)) = (
            cache.text_style(NotedeckTextStyle::NoteBody),
            cache.text_style(NotedeckTextStyle::NoteBody),
        ) else {
            panic!("NoteBody is one of egui's named styles");
        };

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_named_font_family_hands_back_the_same_arc() {
        let cache = StyleCache::new();

        let (FontFamily::Name(first), FontFamily::Name(second)) = (
            cache.font_family(NamedFontFamily::Medium),
            cache.font_family(NamedFontFamily::Medium),
        ) else {
            panic!("every NamedFontFamily is one of egui's named families");
        };

        assert!(Arc::ptr_eq(&first, &second));
    }

    /// The interned set has to agree with the enums it was built from, for
    /// every variant — the lookup is by discriminant, so a variant landing on
    /// the wrong index would silently render in the wrong style.
    #[test]
    fn every_variant_interns_to_what_the_enum_says() {
        let cache = StyleCache::new();

        for style in NotedeckTextStyle::iter() {
            assert_eq!(cache.text_style(style), style.text_style(), "{style:?}");
        }

        for family in NamedFontFamily::iter() {
            assert_eq!(
                cache.font_family(family),
                FontFamily::Name(family.as_str().into()),
                "{family:?}"
            );
        }
    }

    #[test]
    fn stays_bounded() {
        let ctx = egui::Context::default();
        let mut cache = StyleCache::new();

        ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                for i in 0..(MAX_VARIANTS * 3) {
                    ui.horizontal(|ui| {
                        cache.item_spacing_x(ui, i as f32);
                    });
                }
            });
        })
        .drop_without_applying_deltas();

        assert!(cache.len() <= MAX_VARIANTS);
    }
}
