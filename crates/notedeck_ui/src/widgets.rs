use crate::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE};
use crate::icons::search_icon;
use egui::{emath::GuiRounding, Align, CornerRadius, Label, Pos2, RichText, Stroke, TextEdit};
use notedeck::tokens::{RADIUS_MD, RADIUS_PILL, RADIUS_SM, SPACING_SM, SPACING_XS, STROKE_THIN};
use notedeck::{ColorTheme, NotedeckTextStyle};

pub fn x_button(rect: egui::Rect) -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_width = rect.width();
        let helper = AnimationHelper::new_from_rect(ui, "user_search_close", rect);

        let fill_color = ui.visuals().text_color();

        let radius = max_width / (2.0 * ICON_EXPANSION_MULTIPLE);

        let painter = ui.painter();
        let ppp = ui.ctx().pixels_per_point();
        let nw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, radius))
            .round_to_pixel_center(ppp);
        let se_edge = helper
            .scale_pos_from_center(Pos2::new(radius, -radius))
            .round_to_pixel_center(ppp);
        let sw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, -radius))
            .round_to_pixel_center(ppp);
        let ne_edge = helper
            .scale_pos_from_center(Pos2::new(radius, radius))
            .round_to_pixel_center(ppp);

        let line_width = helper.scale_1d_pos(2.0);

        painter.line_segment([nw_edge, se_edge], Stroke::new(line_width, fill_color));
        painter.line_segment([ne_edge, sw_edge], Stroke::new(line_width, fill_color));

        helper.take_animation_response()
    }
}

/// Button styled in the Notedeck theme
pub fn styled_button_toggleable(
    text: &str,
    fill_color: egui::Color32,
    enabled: bool,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let text_color = if ui.visuals().dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        let galley = painter.layout(
            text.to_owned(),
            NotedeckTextStyle::Button.get_font_id(ui.ctx()),
            text_color,
            ui.available_width(),
        );

        let size = galley.rect.expand2(egui::vec2(16.0, SPACING_SM)).size();
        let mut button = egui::Button::new(galley).corner_radius(RADIUS_MD);

        if !enabled {
            button = button
                .sense(egui::Sense::focusable_noninteractive())
                .fill(ui.visuals().noninteractive().bg_fill)
                .stroke(ui.visuals().noninteractive().bg_stroke);
        } else {
            button = button.fill(fill_color);
        }

        let mut resp = ui.add_sized(size, button);

        if !enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::NotAllowed);
        }

        resp
    }
}

/// Get appropriate background color for active side panel icon button
pub fn side_panel_active_bg(ui: &egui::Ui) -> egui::Color32 {
    ColorTheme::current(ui.ctx()).interactive_hover
}

/// Get appropriate tint color for side panel icons to ensure visibility
pub fn side_panel_icon_tint(ui: &egui::Ui) -> egui::Color32 {
    ColorTheme::current(ui.ctx()).text_primary
}

/// Returns a styled Frame for search input boxes with rounded corners.
pub fn search_input_frame(ctx: &egui::Context) -> egui::Frame {
    let theme = ColorTheme::current(ctx);
    egui::Frame {
        inner_margin: egui::Margin::symmetric(SPACING_SM as i8, 0),
        outer_margin: egui::Margin::ZERO,
        corner_radius: CornerRadius::same(RADIUS_PILL as u8),
        shadow: Default::default(),
        fill: theme.surface_secondary,
        stroke: Stroke::new(STROKE_THIN, theme.border_default),
    }
}

/// The standard height for search input boxes.
pub const SEARCH_INPUT_HEIGHT: f32 = 34.0;

/// A styled search input box with rounded corners and search icon.
pub fn search_input_box<'a>(query: &'a mut String, hint_text: &'a str) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.horizontal(|ui| {
            search_input_frame(ui.ctx())
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(SPACING_SM, 0.0);

                        ui.add(search_icon(notedeck::tokens::ICON_SM, SEARCH_INPUT_HEIGHT));

                        let response = ui.add_sized(
                            [ui.available_width(), SEARCH_INPUT_HEIGHT],
                            TextEdit::singleline(query)
                                .hint_text(RichText::new(hint_text).weak())
                                .margin(egui::vec2(0.0, 8.0))
                                .frame(false),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, hint_text)
                        });
                        response
                    })
                    .inner
                })
                .inner
        })
        .response
    }
}

/// Height of the leading icon as a fraction of the text row it sits in.
///
/// Roughly the cap height of the body font: a glyph that fills the whole row
/// reads as a badge stuck onto the line, where one sized to the capitals beside
/// it reads as part of the word.
const INLINE_ICON_RATIO: f32 = 0.65;

/// A compact inline reference: a leading icon and one line of text on a rounded
/// tint. The shape a [`KindRenderer`](notedeck::KindRenderer) draws for
/// [`RenderContext::Inline`](notedeck::RenderContext::Inline), spliced into a row
/// of flowing prose.
///
/// Sized to **sit in the line rather than on it**: the chip is one text row tall
/// plus its own outline, so the enclosing `horizontal_wrapped` row barely grows
/// around it and the chip's text lands on the prose baseline either side. Its
/// breathing room above and below is the leading the row already carries, which
/// is why the vertical inner margin is zero — any more and the chip would push
/// the line apart.
///
/// It also owns the two things that make a widget safe to place mid-paragraph, so
/// no renderer has to rediscover them:
///
/// - It is **one line**, always. The label names its wrap mode instead of
///   inheriting one, so a long title ellipsizes rather than folding the chip into
///   a tall block — and an ambient `style.wrap_mode` (a `StripBuilder` cell sets
///   one, and hosts sometimes override that in turn) can't reach inside it.
/// - It **breaks the row** when it wouldn't fit in what's left of the current
///   one. egui wraps text mid-row for you, but a widget is placed at the cursor
///   and has to fit into whatever remains; a chip measures itself first so it can
///   move to the next row like a word too long for the line, instead of
///   ellipsizing away to nothing with an empty row waiting below. Wider than a
///   whole row, it truncates — breaking can't help there.
///
/// `icon` paints the leading glyph into a square of the size it is handed. The
/// returned response covers the whole chip, so callers add their own
/// [`Sense`](egui::Sense) and hover feedback.
pub fn inline_chip(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    text: &str,
    icon: impl FnOnce(&mut egui::Ui, f32),
) -> egui::Response {
    // Padding on the sides only. The outline is the one thing that costs height —
    // a stroke sits outside the content — but `surface_elevated` is the page color
    // in the light theme, so without it the chip would be invisible there.
    let frame = egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(CornerRadius::same(RADIUS_SM as u8))
        .stroke(Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::symmetric(SPACING_XS as i8, 0));

    // `item_spacing` below puts one gap between icon and text, and `total_margin`
    // covers the frame's own margins, so the chip's width is derived from what
    // actually draws it.
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    let icon_size = (row_height * INLINE_ICON_RATIO).round();
    let width = frame.total_margin().sum().x + icon_size + SPACING_XS + text_width(ui, text);
    break_row_unless_fits(ui, width);

    frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                // Center the icon in a slot as tall as the text row. Placed
                // straight into the row it lands on the cap band instead — level
                // with the capitals, but visibly high in a chip whose lower half
                // is the descender space the icon has no use for.
                ui.allocate_ui_with_layout(
                    egui::vec2(icon_size, row_height),
                    egui::Layout::left_to_right(Align::Center),
                    |ui| icon(ui, icon_size),
                );
                ui.add(
                    Label::new(RichText::new(text).color(theme.text_primary))
                        .wrap_mode(egui::TextWrapMode::Truncate),
                );
            })
        })
        .response
}

/// Width `text` occupies in the body font.
///
/// Sums glyph advances rather than laying the text out: `Fonts::layout_no_wrap`
/// needs an owned `String` (galleys are cached by their content), an allocation
/// every frame for every widget on screen. egui lays proportional text out by
/// those same advances — it does no kerning — so the sum is the width the galley
/// would report.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|f| text.chars().map(|c| f.glyph_width(&font, c)).sum())
}

/// Start a fresh row when a widget `width` wide wouldn't fit in what is left of
/// the current one.
///
/// Measures against [`Ui::available_rect_before_wrap`](egui::Ui::available_rect_before_wrap),
/// *not* `available_width`: in a wrapping layout the latter reports the full row
/// on the grounds that a widget can always wrap to the next one (`Layout::available_size`),
/// which is exactly the question being asked here.
///
/// No-ops outside a wrapping layout, and at the start of a row, where breaking
/// would only leave a blank line above the widget.
fn break_row_unless_fits(ui: &mut egui::Ui, width: f32) {
    if !ui.layout().main_wrap() || ui.cursor().min.x <= ui.max_rect().min.x {
        return;
    }
    if width > ui.available_rect_before_wrap().width() {
        ui.end_row();
    }
}
