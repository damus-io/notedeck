//! Centralized design tokens for the Notedeck design system.
//!
//! All spacing, sizing, radius, and stroke values used across notedeck
//! apps should reference these tokens instead of using magic numbers.

// --- Spacing ---

/// Extra-small spacing (4px)
pub const SPACING_XS: f32 = 4.0;
/// Small spacing (8px)
pub const SPACING_SM: f32 = 8.0;
/// Medium spacing (12px)
pub const SPACING_MD: f32 = 12.0;
/// Large spacing (16px)
pub const SPACING_LG: f32 = 16.0;
/// Extra-large spacing (24px)
pub const SPACING_XL: f32 = 24.0;
/// Double extra-large spacing (32px)
pub const SPACING_XXL: f32 = 32.0;

// --- Corner Radius ---

/// Small corner radius (4px) — subtle rounding for cards, inputs
pub const RADIUS_SM: f32 = 4.0;
/// Medium corner radius (8px) — buttons, panels
pub const RADIUS_MD: f32 = 8.0;
/// Large corner radius (12px) — modals, dialogs
pub const RADIUS_LG: f32 = 12.0;
/// Pill corner radius (18px) — search bars, tags, pills
pub const RADIUS_PILL: f32 = 18.0;

// --- Stroke Widths ---

/// Thin stroke (1.0px) — borders, dividers
pub const STROKE_THIN: f32 = 1.0;
/// Medium stroke (1.5px) — icons, emphasis borders
pub const STROKE_MEDIUM: f32 = 1.5;
/// Thick stroke (2.0px) — active states, heavy emphasis
pub const STROKE_THICK: f32 = 2.0;

// --- Icon Sizes ---

/// Small icon (16px) — inline icons, search icon
pub const ICON_SM: f32 = 16.0;
/// Medium icon (24px) — toolbar, nav icons
pub const ICON_MD: f32 = 24.0;
/// Large icon (32px) — feature icons, empty states
pub const ICON_LG: f32 = 32.0;

// --- Profile Picture Sizes ---

/// Small profile pic (24px) — inline mentions, compact lists
pub const PFP_SM: f32 = 24.0;
/// Medium profile pic (38px) — note headers, timelines
pub const PFP_MD: f32 = 38.0;
/// Large profile pic (48px) — profile cards, dialogs
pub const PFP_LG: f32 = 48.0;
/// Extra-large profile pic (80px) — profile pages, settings
pub const PFP_XL: f32 = 80.0;

// --- Button Heights ---

/// Small button height (28px) — compact actions
pub const BUTTON_SM: f32 = 28.0;
/// Medium button height (34px) — standard actions
pub const BUTTON_MD: f32 = 34.0;
/// Large button height (44px) — primary CTAs, touch targets
pub const BUTTON_LG: f32 = 44.0;

// --- Opacity ---

/// Disabled state opacity
pub const OPACITY_DISABLED: f32 = 0.38;
/// Secondary/muted content opacity
pub const OPACITY_MUTED: f32 = 0.6;
/// Overlay backdrop opacity
pub const OPACITY_OVERLAY: f32 = 0.5;

// --- Animation ---

/// Default animation speed for hover/interaction transitions
pub const ANIM_SPEED: f32 = 0.05;
/// Icon expansion multiple on hover
pub const ICON_EXPANSION_MULTIPLE: f32 = 1.2;

// --- Frame Margin ---

/// Default frame margin
pub const FRAME_MARGIN: f32 = 8.0;
