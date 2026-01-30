//! Color palette for the Git app UI.
//!
//! Colors are based on GitHub Primer design system, optimized for dark mode.
//! Touch targets and sizing follow Apple HIG (44pt minimum).

#![allow(dead_code)]

use egui::Color32;

/// Background colors for dark mode.
pub mod bg {
    use super::*;

    /// Primary background - very dark blue-gray.
    pub const PRIMARY: Color32 = Color32::from_rgb(0x0D, 0x11, 0x17);

    /// Card/panel background - slightly lighter for contrast.
    pub const CARD: Color32 = Color32::from_rgb(0x16, 0x1B, 0x22);

    /// Hover/selected state background.
    pub const HOVER: Color32 = Color32::from_rgb(0x21, 0x26, 0x2D);

    /// Elevated surface (modals, dropdowns).
    pub const ELEVATED: Color32 = Color32::from_rgb(0x26, 0x2C, 0x36);

    /// Border/subtle divider color.
    pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3D);
}

/// Text colors for dark mode.
pub mod text {
    use super::*;

    /// Primary text - off-white for eye comfort.
    pub const PRIMARY: Color32 = Color32::from_rgb(0xE6, 0xED, 0xF3);

    /// Secondary text - muted gray.
    pub const SECONDARY: Color32 = Color32::from_rgb(0x8B, 0x94, 0x9E);

    /// Tertiary text - further reduced contrast.
    pub const TERTIARY: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);

    /// Muted text - lowest contrast.
    pub const MUTED: Color32 = Color32::from_rgb(0x48, 0x4F, 0x58);
}

/// Status colors for badges.
pub mod status {
    use super::*;

    /// Open status - GitHub green.
    pub const OPEN: Color32 = Color32::from_rgb(0x23, 0x86, 0x36);

    /// Merged/applied status - GitHub purple.
    pub const MERGED: Color32 = Color32::from_rgb(0x82, 0x50, 0xDF);

    /// Closed status - GitHub red.
    pub const CLOSED: Color32 = Color32::from_rgb(0xD1, 0x24, 0x2F);

    /// Draft status - neutral gray.
    pub const DRAFT: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);
}

/// Accent colors.
pub mod accent {
    use super::*;

    /// Primary brand color - Notedeck purple.
    pub const PRIMARY: Color32 = Color32::from_rgb(0xCC, 0x43, 0xC5);

    /// Link/info color - GitHub blue.
    pub const LINK: Color32 = Color32::from_rgb(0x58, 0xA6, 0xFF);

    /// Warning color.
    pub const WARNING: Color32 = Color32::from_rgb(0xD2, 0x99, 0x22);

    /// Error color.
    pub const ERROR: Color32 = Color32::from_rgb(0xF8, 0x51, 0x49);

    /// Success color - brighter green for emphasis.
    pub const SUCCESS: Color32 = Color32::from_rgb(0x3F, 0xB9, 0x50);
}

/// Label/tag colors.
pub mod label {
    use super::*;

    /// Label background.
    pub const BG: Color32 = Color32::from_rgb(0x30, 0x36, 0x3D);

    /// Label text.
    pub const TEXT: Color32 = Color32::from_rgb(0xC9, 0xD1, 0xD9);
}

/// Diff/patch syntax highlighting colors.
pub mod diff {
    use super::*;

    /// Added line foreground - green.
    pub const ADDED: Color32 = Color32::from_rgb(0x3F, 0xB9, 0x50);

    /// Removed line foreground - red.
    pub const REMOVED: Color32 = Color32::from_rgb(0xF8, 0x51, 0x49);

    /// Context line foreground - muted.
    pub const CONTEXT: Color32 = Color32::from_rgb(0x8B, 0x94, 0x9E);

    /// Diff header (diff --git, index, ---, +++).
    pub const HEADER: Color32 = Color32::from_rgb(0x58, 0xA6, 0xFF);

    /// Hunk header (@@...@@).
    pub const HUNK: Color32 = Color32::from_rgb(0xA3, 0x71, 0xF7);

    /// Patch metadata (From:, Date:, Subject:).
    pub const META: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);
}

/// Sizing constants following Apple HIG.
pub mod sizing {
    /// Minimum touch target size (Apple HIG: 44pt).
    pub const MIN_TOUCH_TARGET: f32 = 44.0;

    /// Card padding - horizontal (as i8 for egui Margin).
    pub const CARD_PADDING_H: i8 = 16;

    /// Card padding - vertical (as i8 for egui Margin).
    pub const CARD_PADDING_V: i8 = 12;

    /// Spacing between cards.
    pub const CARD_SPACING: f32 = 12.0;

    /// Small spacing (between elements within a card).
    pub const SPACING_SM: f32 = 8.0;

    /// Medium spacing.
    pub const SPACING_MD: f32 = 16.0;

    /// Large spacing.
    pub const SPACING_LG: f32 = 24.0;

    /// Border radius for cards (as u8 for egui CornerRadius).
    pub const CARD_ROUNDING: u8 = 8;

    /// Border radius for badges (as u8 for egui CornerRadius).
    pub const BADGE_ROUNDING: u8 = 4;
}

/// Font sizes.
pub mod font {
    /// Title size (repository names).
    pub const TITLE: f32 = 16.0;

    /// Heading size (page headings).
    pub const HEADING: f32 = 24.0;

    /// Body text size.
    pub const BODY: f32 = 14.0;

    /// Small text size (labels, metadata).
    pub const SMALL: f32 = 12.0;

    /// Tiny text size (timestamps, hints).
    pub const TINY: f32 = 11.0;

    /// Monospace text size (URLs, code).
    pub const MONO: f32 = 12.0;
}
