//! Cashflow's semantic colors, derived from the host's [`ColorTheme`].
//!
//! Views never name a raw color: they ask the [`Palette`] for what a color
//! *means* — money in, money out, a muted caption — so a theme can be swapped
//! in by changing how a palette is built, without touching a view. The palette
//! lives in [`crate::CashflowApp`]'s state and reaches views through
//! [`crate::views::ViewCx`]; there is no global (CLAUDE.md rule 6).

use egui::Color32;
use notedeck::ColorTheme;

/// The colors every Cashflow view draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The Adventure view's game HUD (see [`crate::ui::hud`]).
    pub hud: HudPalette,
}

/// The game HUD's colors: dark glass panels with a gold title and colored
/// "rarity" lines, read by every widget in [`crate::ui::hud`].
///
/// The panel stays dark in the light theme too. A HUD floats over the page
/// like a game overlay, and its gold and rarity text only reads on a dark
/// backing; the light theme makes the glass a little less see-through, since
/// what shows through it is bright. The XP colors follow the theme's accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudPalette {
    /// Behind a tooltip, toast or skill tile: translucent near-black.
    pub panel: Color32,
    /// The thin bright edge around a panel.
    pub border: Color32,
    /// A tooltip's title line and a skill tile's level.
    pub title: Color32,
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
        let (panel_alpha, border) = if light {
            (242, Color32::from_rgb(112, 120, 140))
        } else {
            (228, Color32::from_rgb(150, 158, 178))
        };
        Self {
            panel: Color32::from_rgba_unmultiplied(12, 14, 26, panel_alpha),
            border,
            title: Color32::from_rgb(255, 206, 60),
            text: Color32::from_rgb(238, 238, 238),
            text_muted: Color32::from_rgb(160, 162, 176),
            track: Color32::from_rgba_unmultiplied(12, 14, 26, 210),
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
            hud: HudPalette::from_theme(theme),
        }
    }

    /// Re-derive the palette from the host's current theme.
    ///
    /// Called at the top of every frame. It is a lookup in egui's temp data
    /// and a copy of a few colors, and it follows every theme change the host
    /// makes (light/dark, OLED) without the app tracking which one happened.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        *self = Self::from_theme(&ColorTheme::current(ctx));
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
