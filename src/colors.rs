use egui::Color32;

pub const PURPLE: Color32 = Color32::from_rgb(0xCC, 0x43, 0xC5);
// TODO: This should not be exposed publicly
pub const PINK: Color32 = Color32::from_rgb(0xE4, 0x5A, 0xC9);
//pub const DARK_BG: Color32 = egui::Color32::from_rgb(40, 44, 52);
pub const GRAY_SECONDARY: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const RED_700: Color32 = Color32::from_rgb(0xC7, 0x37, 0x5A);
const GREEN_700: Color32 = Color32::from_rgb(0x24, 0xEC, 0xC9);
const ORANGE_700: Color32 = Color32::from_rgb(0xF6, 0xB1, 0x4A);

// BACKGROUNDS
const SEMI_DARKER_BG: Color32 = Color32::from_rgb(0x39, 0x39, 0x39);
const DARKER_BG: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F);
const DARK_BG: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
const DARK_ISH_BG: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
const SEMI_DARK_BG: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);

const LIGHTER_GRAY: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
const LIGHT_GRAY: Color32 = Color32::from_rgb(0xc8, 0xc8, 0xc8); // 78%
pub const MID_GRAY: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);
const DARKER_GRAY: Color32 = Color32::from_rgb(0xa5, 0xa5, 0xa5); // 65%
const EVEN_DARKER_GRAY: Color32 = Color32::from_rgb(0x89, 0x89, 0x89); // 54%

pub struct ColorTheme {
    // VISUALS
    pub panel_fill: Color32,
    pub extreme_bg_color: Color32,
    pub text_color: Color32,
    pub err_fg_color: Color32,
    pub warn_fg_color: Color32,
    pub hyperlink_color: Color32,
    pub selection_color: Color32,

    // WINDOW
    pub window_fill: Color32,
    pub window_stroke_color: Color32,

    // NONINTERACTIVE WIDGET
    pub noninteractive_bg_fill: Color32,
    pub noninteractive_weak_bg_fill: Color32,
    pub noninteractive_bg_stroke_color: Color32,
    pub noninteractive_fg_stroke_color: Color32,

    // INACTIVE WIDGET
    pub inactive_bg_stroke_color: Color32,
    pub inactive_bg_fill: Color32,
    pub inactive_weak_bg_fill: Color32,
}

pub fn desktop_dark_color_theme() -> ColorTheme {
    ColorTheme {
        // VISUALS
        panel_fill: DARKER_BG,
        extreme_bg_color: SEMI_DARKER_BG,
        text_color: Color32::WHITE,
        err_fg_color: RED_700,
        warn_fg_color: ORANGE_700,
        hyperlink_color: PURPLE,
        selection_color: GREEN_700,

        // WINDOW
        window_fill: DARK_ISH_BG,
        window_stroke_color: DARK_BG,

        // NONINTERACTIVE WIDGET
        noninteractive_bg_fill: DARK_ISH_BG,
        noninteractive_weak_bg_fill: DARK_BG,
        noninteractive_bg_stroke_color: SEMI_DARKER_BG,
        noninteractive_fg_stroke_color: GRAY_SECONDARY,

        // INACTIVE WIDGET
        inactive_bg_stroke_color: SEMI_DARKER_BG,
        inactive_bg_fill: Color32::from_rgb(0x25, 0x25, 0x25),
        inactive_weak_bg_fill: SEMI_DARK_BG,
    }
}

pub fn mobile_dark_color_theme() -> ColorTheme {
    ColorTheme {
        panel_fill: Color32::BLACK,
        ..desktop_dark_color_theme()
    }
}

pub fn light_color_theme() -> ColorTheme {
    ColorTheme {
        // VISUALS
        panel_fill: Color32::WHITE,
        extreme_bg_color: EVEN_DARKER_GRAY,
        text_color: BLACK,
        err_fg_color: RED_700,
        warn_fg_color: ORANGE_700,
        hyperlink_color: PURPLE,
        selection_color: GREEN_700,

        // WINDOW
        window_fill: Color32::WHITE,
        window_stroke_color: DARKER_GRAY,

        // NONINTERACTIVE WIDGET
        noninteractive_bg_fill: Color32::WHITE,
        noninteractive_weak_bg_fill: EVEN_DARKER_GRAY,
        noninteractive_bg_stroke_color: LIGHTER_GRAY,
        noninteractive_fg_stroke_color: GRAY_SECONDARY,

        // INACTIVE WIDGET
        inactive_bg_stroke_color: EVEN_DARKER_GRAY,
        inactive_bg_fill: LIGHT_GRAY,
        inactive_weak_bg_fill: EVEN_DARKER_GRAY,
    }
}
