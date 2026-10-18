//! Cashflow's semantic colors, derived from the host's [`ColorTheme`], and
//! the faces its HUD sets text in.
//!
//! Views never name a raw color: they ask the [`Palette`] for what a color
//! *means* — money in, money out, a muted caption — so a theme can be swapped
//! in by changing how a palette is built, without touching a view. The palette
//! lives in [`crate::CashflowApp`]'s state and reaches views through
//! [`crate::views::ViewCx`]; there is no global (CLAUDE.md rule 6).

use egui::{Color32, FontFamily};
use notedeck::{ColorTheme, NamedFontFamily};

/// The colors every Cashflow view draws with, and the faces its HUD sets
/// text in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Behind every pane.
    pub background: Color32,
    /// A raised card or the nav rail, one step off [`Self::background`].
    pub surface: Color32,
    /// Hairlines between panes and around cards.
    pub border: Color32,
    /// Body text and headings.
    pub text: Color32,
    /// Captions, axis labels and placeholder text.
    pub text_muted: Color32,
    /// The selected nav entry and other "you are here" marks.
    pub accent: Color32,
    /// Money in: income and positive deltas.
    pub income: Color32,
    /// Money out: expenses and negative deltas.
    pub expense: Color32,
    /// Money that only moved between the user's own accounts.
    pub transfer: Color32,
    /// Needs attention: stale sources, missing segments, uncategorized rows.
    pub warning: Color32,
    /// Lines that stand for things rather than for money in or out, such as
    /// accounts compared on one chart, handed out in this order and kept by
    /// the thing they stand for (see [`SERIES`]).
    pub series: [Color32; SERIES],
    /// The Adventure view's game HUD (see [`crate::ui::hud`]).
    pub hud: HudPalette,
    /// The bold and medium faces, built once (see [`Faces`]).
    pub faces: Faces,
}

/// How many [`Palette::series`] colors there are, so how many things one
/// chart compares.
pub const SERIES: usize = 5;

/// The categorical steps for a light page: blue, orange, aqua, yellow,
/// magenta, in that order, which keeps neighbours apart for color-blind
/// readers (checked with the dataviz skill's palette validator).
const SERIES_LIGHT: [Color32; SERIES] = [
    Color32::from_rgb(0x2a, 0x78, 0xd6),
    Color32::from_rgb(0xeb, 0x68, 0x34),
    Color32::from_rgb(0x1b, 0xaf, 0x7a),
    Color32::from_rgb(0xed, 0xa1, 0x00),
    Color32::from_rgb(0xe8, 0x7b, 0xa4),
];

/// [`SERIES_LIGHT`]'s hues stepped for a dark page.
const SERIES_DARK: [Color32; SERIES] = [
    Color32::from_rgb(0x39, 0x87, 0xe5),
    Color32::from_rgb(0xd9, 0x59, 0x26),
    Color32::from_rgb(0x19, 0x9e, 0x70),
    Color32::from_rgb(0xc9, 0x85, 0x00),
    Color32::from_rgb(0xd5, 0x51, 0x81),
];

/// notedeck's bold and medium font families, built once and handed out by
/// reference.
///
/// egui names a family with an `Arc<str>`, so writing
/// `FontFamily::Name("bold".into())` in a widget would allocate every frame.
/// These are built when the [`Palette`] is, and [`Palette::refresh`] keeps
/// them, so a widget only clones an `Arc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Faces {
    /// A title or a level: notedeck's bold face.
    pub bold: FontFamily,
    /// A value: notedeck's medium face, a step heavier than body text.
    pub medium: FontFamily,
}

impl Faces {
    /// Stands in while [`Palette::refresh`] moves the real faces across.
    /// Proportional is a plain variant, so this allocates nothing.
    const PLACEHOLDER: Self = Self {
        bold: FontFamily::Proportional,
        medium: FontFamily::Proportional,
    };
}

impl Default for Faces {
    fn default() -> Self {
        Self {
            bold: FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
            medium: FontFamily::Name(NamedFontFamily::Medium.as_str().into()),
        }
    }
}

/// The game HUD's colors: dark glass panels in a gilt hairline, a gold title
/// and colored "rarity" lines, read by every widget in [`crate::ui::hud`].
///
/// The panel stays dark in the light theme too. A HUD floats over the page
/// like a game overlay, and its gold and rarity text only reads on a dark
/// backing; the light theme makes the glass a little less see-through, since
/// what shows through it is bright. The XP colors follow the theme's accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudPalette {
    /// Behind a tooltip, toast or skill tile: translucent near-black.
    pub panel: Color32,
    /// The gilt hairline around a panel, a bar or an icon slot.
    pub border: Color32,
    /// A sheen catching the top edge of a bar or a panel.
    pub highlight: Color32,
    /// Darkness let into a panel: an icon slot, a bar's notches, a medallion.
    pub shade: Color32,
    /// A tooltip's title line, a skill tile's level and a toast's frame.
    pub title: Color32,
    /// The metal of the level-up toast's 3D medallion: its base colour, lit
    /// as a polished metal (see [`crate::ui::hud`]'s coin). Brass by default.
    pub gilt: Color32,
    /// Body text on a panel.
    pub text: Color32,
    /// Captions on a panel.
    pub text_muted: Color32,
    /// An XP bar's empty track.
    pub track: Color32,
    /// An XP bar's earned XP.
    pub xp_fill: Color32,
    /// The rested-XP bonus drawn past the earned XP, lighter than
    /// [`Self::xp_fill`].
    pub xp_rested: Color32,
    /// Magnitude tiers, smallest to largest (see
    /// [`crate::ui::hud::Rarity`]): green, blue, purple, orange.
    pub uncommon: Color32,
    /// See [`Self::uncommon`].
    pub rare: Color32,
    /// See [`Self::uncommon`].
    pub epic: Color32,
    /// See [`Self::uncommon`].
    pub legendary: Color32,
}

impl HudPalette {
    /// The standard HUD, with its XP colors taken from the theme's accent.
    pub fn from_theme(theme: &ColorTheme) -> Self {
        let light = is_light(theme);
        let panel_alpha = if light { 245 } else { 238 };
        Self {
            panel: Color32::from_rgba_unmultiplied(12, 14, 24, panel_alpha),
            border: Color32::from_rgb(150, 122, 64),
            highlight: Color32::from_white_alpha(70),
            shade: Color32::from_black_alpha(140),
            title: Color32::from_rgb(255, 206, 60),
            // Linear (0.90, 0.60, 0.12): a warm brass. Lit as polished
            // metal it reflects the dim lighting environment, so it is
            // brighter than the brass it reads as (microverse's 0.68,
            // 0.48, 0.14 renders khaki here).
            gilt: Color32::from_rgb(244, 203, 97),
            text: Color32::from_rgb(238, 238, 238),
            text_muted: Color32::from_rgb(160, 162, 176),
            track: Color32::from_rgba_unmultiplied(6, 7, 14, 230),
            xp_fill: theme.accent,
            xp_rested: theme.accent.lerp_to_gamma(Color32::WHITE, 0.5),
            uncommon: Color32::from_rgb(72, 222, 72),
            rare: Color32::from_rgb(72, 148, 255),
            epic: Color32::from_rgb(184, 108, 255),
            legendary: Color32::from_rgb(255, 144, 36),
        }
    }
}

/// Whether `theme` is a light theme: its page background is brighter than
/// mid-grey.
fn is_light(theme: &ColorTheme) -> bool {
    let bg = theme.surface_primary;
    let luma = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    luma > 127.0
}

impl Palette {
    /// The standard palette: the host theme's semantic colors, read as money.
    pub fn from_theme(theme: &ColorTheme) -> Self {
        Self::with_faces(theme, Faces::default())
    }

    /// [`Self::from_theme`] with `faces` already built.
    fn with_faces(theme: &ColorTheme, faces: Faces) -> Self {
        Self {
            background: theme.surface_primary,
            surface: theme.surface_secondary,
            border: theme.border_default,
            text: theme.text_primary,
            text_muted: theme.text_muted,
            accent: theme.accent,
            income: theme.success,
            expense: theme.destructive,
            transfer: theme.text_secondary,
            warning: theme.warning,
            series: if is_light(theme) {
                SERIES_LIGHT
            } else {
                SERIES_DARK
            },
            hud: HudPalette::from_theme(theme),
            faces,
        }
    }

    /// Re-derive the palette from the host's current theme.
    ///
    /// Called at the top of every frame. It is a lookup in egui's temp data
    /// and a copy of a few colors, and it follows every theme change the host
    /// makes (light/dark, OLED) without the app tracking which one happened.
    /// The [`Faces`] carry over rather than being built again.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        let faces = std::mem::replace(&mut self.faces, Faces::PLACEHOLDER);
        *self = Self::with_faces(&ColorTheme::current(ctx), faces);
    }
}

impl Default for Palette {
    /// The desktop dark theme's palette, until the first [`Palette::refresh`].
    fn default() -> Self {
        Self::from_theme(&notedeck::theme::desktop_dark_color_theme())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::theme::{desktop_dark_color_theme, light_color_theme};

    /// Money in and money out never share a color, in either theme, so a
    /// chart never draws income and expense alike.
    #[test]
    fn income_and_expense_differ_in_both_themes() {
        for theme in [desktop_dark_color_theme(), light_color_theme()] {
            let palette = Palette::from_theme(&theme);
            assert_ne!(palette.income, palette.expense);
            assert_ne!(palette.text, palette.background);
        }
    }

    /// The HUD's rarity tiers are four distinct colors, none of them the
    /// panel's body text, in either theme.
    #[test]
    fn hud_rarity_tiers_are_distinct() {
        for theme in [desktop_dark_color_theme(), light_color_theme()] {
            let hud = Palette::from_theme(&theme).hud;
            let tiers = [hud.text, hud.uncommon, hud.rare, hud.epic, hud.legendary];
            for (i, a) in tiers.iter().enumerate() {
                for b in &tiers[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }
    }

    /// `refresh` follows the context's light/dark mode.
    #[test]
    fn refresh_follows_the_host_theme() {
        let ctx = egui::Context::default();
        let mut palette = Palette::default();

        ctx.set_visuals(egui::Visuals::light());
        palette.refresh(&ctx);
        assert_eq!(palette, Palette::from_theme(&light_color_theme()));

        ctx.set_visuals(egui::Visuals::dark());
        palette.refresh(&ctx);
        assert_eq!(palette, Palette::from_theme(&desktop_dark_color_theme()));
    }
}
