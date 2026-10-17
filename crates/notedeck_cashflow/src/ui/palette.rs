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
