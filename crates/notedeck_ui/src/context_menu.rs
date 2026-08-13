/// Context menu helpers (paste, etc)
use egui::text::{CCursor, CCursorRange};
use egui_winit::clipboard::Clipboard;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PasteBehavior {
    Clear,
    Append,
}

/// The live text selection inside a text widget, read back from its stored
/// [`egui::TextEditState`]: the widget's id plus the selected span as an ordered
/// char range (empty when it's just a caret, not a highlighted span).
struct TextSelection {
    id: egui::Id,
    range: std::ops::Range<usize>,
}

impl TextSelection {
    /// Read the current selection/caret out of the text widget behind `response`,
    /// or `None` if it has no stored cursor (e.g. never focused, or `response`
    /// isn't a `TextEdit`). A `None` here makes every action below fall back to
    /// the pre-selection, whole-buffer behaviour, so non-`TextEdit` callers keep
    /// working unchanged.
    fn load(ctx: &egui::Context, response: &egui::Response) -> Option<Self> {
        let range = egui::TextEdit::load_state(ctx, response.id)?
            .cursor
            .char_range()?;
        let (a, b) = (range.primary.index, range.secondary.index);
        Some(TextSelection {
            id: response.id,
            range: a.min(b)..a.max(b),
        })
    }

    /// Whether a non-empty span is highlighted (as opposed to a bare caret).
    fn has_span(&self) -> bool {
        !self.range.is_empty()
    }
}

/// Byte range spanned by an ordered char `range` in `text`; an endpoint at or
/// past the char count clamps to `text.len()`, so an end-of-buffer or
/// empty-buffer edit yields a valid splice point rather than panicking.
fn char_range_to_bytes(text: &str, range: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    let byte = |i: usize| text.char_indices().nth(i).map_or(text.len(), |(b, _)| b);
    byte(range.start)..byte(range.end)
}

/// The text a Copy/Cut acts on: the highlighted span if there is one, otherwise
/// the whole buffer — preserving the earlier behaviour where a right-click Copy
/// with nothing selected grabbed the entire field.
fn copy_target<'a>(input: &'a str, selection: Option<&TextSelection>) -> &'a str {
    match selection {
        Some(sel) if sel.has_span() => &input[char_range_to_bytes(input, &sel.range)],
        _ => input,
    }
}

/// Move text widget `id`'s caret to char index `caret` and refocus it, so after
/// we've spliced its buffer out-of-band (cut/paste) the cursor lands sensibly on
/// the next frame instead of snapping to a stale offset.
fn place_caret(ctx: &egui::Context, id: egui::Id, caret: usize) {
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
        return;
    };
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
    egui::TextEdit::store_state(ctx, id, state);
    ctx.memory_mut(|mem| mem.request_focus(id));
}

fn handle_paste(
    ctx: &egui::Context,
    clipboard: &mut Clipboard,
    input: &mut String,
    paste_behavior: PasteBehavior,
    selection: Option<&TextSelection>,
) {
    let Some(text) = clipboard.get() else {
        return;
    };

    // Clear-mode fields (passwords, single-value url pickers, etc.) always
    // replace the whole buffer on paste, ignoring any selection.
    if paste_behavior == PasteBehavior::Clear {
        input.clear();
        input.push_str(&text);
        return;
    }

    // Append mode: drop the pasted text over the highlighted span, or at the
    // caret; with no cursor at all, fall back to appending at the very end.
    let range = selection.map(|s| s.range.clone()).unwrap_or_else(|| {
        let end = input.chars().count();
        end..end
    });
    let caret = range.start + text.chars().count();
    input.replace_range(char_range_to_bytes(input, &range), &text);
    if let Some(sel) = selection {
        place_caret(ctx, sel.id, caret);
    }
}

/// Force text widget `id`'s selection back to the ordered char `range`. egui only
/// *paints* a selection highlight while the widget has focus (see
/// `paint_text_selection` in egui's `text_edit`), and a right-click both keeps the
/// `TextEdit` focused and collapses its span to a caret — so writing the span back
/// into the stored state each frozen frame is what keeps the user's highlight
/// visible through the right-click and the whole time the menu is open.
fn restore_selection(ctx: &egui::Context, id: egui::Id, range: &std::ops::Range<usize>) {
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
        return;
    };
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(range.start),
        CCursor::new(range.end),
    )));
    egui::TextEdit::store_state(ctx, id, state);
}

/// Read the effective text selection for the context menu, working around egui
/// collapsing a `TextEdit`'s highlighted span the instant any pointer button is
/// pressed on it — including the right-click that opens this very menu (see
/// [`TextSelection::load`] callers and egui's `pointer_interaction`). We shadow
/// the widget's live selection in per-widget temp memory every frame and freeze
/// that shadow the moment the secondary button starts interacting (or the menu is
/// open), so the span the user had *just before* they right-clicked survives into
/// the menu's Copy/Cut/Paste handlers.
///
/// While frozen we also paint the shadowed span back into the widget's stored
/// state so the highlight the user made stays *visible* through the right-click —
/// egui had collapsed it to a bare caret and never restored it on its own, which
/// read as "right-clicking deselects".
fn frozen_selection(ui: &mut egui::Ui, response: &egui::Response) -> Option<TextSelection> {
    let snap_id = response.id.with("input_context_selection");

    // Freeze from the first secondary press, through the button being held and
    // released, and for as long as the menu stays open — the whole window during
    // which egui has clobbered the live selection.
    let frozen = response.context_menu_opened()
        || response.secondary_clicked()
        || ui.input(|i| i.pointer.secondary_down());

    if !frozen {
        let live = TextSelection::load(ui.ctx(), response).map(|s| s.range);
        ui.data_mut(|d| d.insert_temp(snap_id, live));
    }

    let range = ui
        .data(|d| d.get_temp::<Option<std::ops::Range<usize>>>(snap_id))
        .flatten()?;

    // Repaint the highlight the right-click collapsed, but only for a real span:
    // restoring a bare caret would fight the user placing the caret elsewhere.
    if frozen && !range.is_empty() {
        restore_selection(ui.ctx(), response.id, &range);
    }

    Some(TextSelection {
        id: response.id,
        range,
    })
}

pub fn input_context(
    ui: &mut egui::Ui,
    response: &egui::Response,
    clipboard: &mut Clipboard,
    input: &mut String,
    paste_behavior: PasteBehavior,
) {
    // The selection is stored on the egui Context, so it's the same whether read
    // here (for the menu) or below (for a middle-click paste).
    let selection = frozen_selection(ui, response);

    context_menu(response, |ui| {
        if ui.button("Paste").clicked() {
            handle_paste(
                ui.ctx(),
                clipboard,
                input,
                paste_behavior,
                selection.as_ref(),
            );
            ui.close_menu();
        }

        if ui.button("Copy").clicked() {
            clipboard.set_text(copy_target(input, selection.as_ref()).to_owned());
            ui.close_menu();
        }

        if ui.button("Cut").clicked() {
            clipboard.set_text(copy_target(input, selection.as_ref()).to_owned());
            match selection.as_ref().filter(|s| s.has_span()) {
                // Excise just the highlighted span and drop the caret where it was.
                Some(sel) => {
                    input.replace_range(char_range_to_bytes(input, &sel.range), "");
                    place_caret(ui.ctx(), sel.id, sel.range.start);
                }
                // No span selected: cut the whole field, as before.
                None => input.clear(),
            }
            ui.close_menu();
        }
    });

    if response.middle_clicked() {
        handle_paste(
            ui.ctx(),
            clipboard,
            input,
            paste_behavior,
            selection.as_ref(),
        );
    }

    // for keyboard visibility
    crate::include_input(ui, response)
}

#[derive(Copy, Clone, Debug)]
pub struct MenuPadding {
    pub button_padding: egui::Vec2,
    pub item_spacing_y: f32,
}

impl Default for MenuPadding {
    fn default() -> Self {
        Self {
            button_padding: egui::vec2(12.0, 8.0),
            item_spacing_y: 4.0,
        }
    }
}

pub fn stationary_arbitrary_menu_button<R>(
    ui: &mut egui::Ui,
    button_response: egui::Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    stationary_arbitrary_menu_button_padding(
        ui,
        button_response,
        MenuPadding::default(),
        add_contents,
    )
}

pub fn stationary_arbitrary_menu_button_padding<R>(
    ui: &mut egui::Ui,
    button_response: egui::Response,
    padding: MenuPadding,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let bar_id = ui.id();
    let mut bar_state = egui::menu::BarState::load(ui.ctx(), bar_id);

    let inner = bar_state.bar_menu(&button_response, |ui| {
        ui.spacing_mut().button_padding = padding.button_padding;
        ui.spacing_mut().item_spacing.y = padding.item_spacing_y;
        add_contents(ui)
    });

    bar_state.store(ui.ctx(), bar_id);
    egui::InnerResponse::new(inner.map(|r| r.inner), button_response)
}

/// Drop-in replacement for `response.context_menu()` with configurable padding.
pub fn context_menu_padding(
    response: &egui::Response,
    padding: MenuPadding,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    response.context_menu(|ui| {
        ui.spacing_mut().button_padding = padding.button_padding;
        ui.spacing_mut().item_spacing.y = padding.item_spacing_y;
        add_contents(ui);
    });
}

/// Drop-in replacement for `response.context_menu()` that applies our
/// standard menu padding.
pub fn context_menu(response: &egui::Response, add_contents: impl FnOnce(&mut egui::Ui)) {
    context_menu_padding(response, MenuPadding::default(), add_contents);
}

pub fn context_button(ui: &mut egui::Ui, id: egui::Id, put_at: egui::Rect) -> egui::Response {
    let min_radius = 2.0;
    let anim_speed = 0.05;
    let response = ui.interact(put_at, id, egui::Sense::click());

    let hovered = response.hovered();
    let animation_progress = ui.ctx().animate_bool_with_time(id, hovered, anim_speed);

    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let max_distance = 2.0;
    let expansion_mult = 2.0;
    let min_distance = max_distance / expansion_mult;
    let cur_distance = min_distance + (max_distance - min_distance) * animation_progress;

    let max_radius = 4.0;
    let cur_radius = min_radius + (max_radius - min_radius) * animation_progress;

    let center = put_at.center();
    let left_circle_center = center - egui::vec2(cur_distance + cur_radius, 0.0);
    let right_circle_center = center + egui::vec2(cur_distance + cur_radius, 0.0);

    let translated_radius = (cur_radius - 1.0) / 2.0;

    // This works in both themes
    let color = ui.style().visuals.noninteractive().fg_stroke.color;

    // Draw circles
    let painter = ui.painter_at(put_at);
    painter.circle_filled(left_circle_center, translated_radius, color);
    painter.circle_filled(center, translated_radius, color);
    painter.circle_filled(right_circle_center, translated_radius, color);

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(range: std::ops::Range<usize>) -> TextSelection {
        TextSelection {
            id: egui::Id::new("test"),
            range,
        }
    }

    /// The char→byte mapping the selection-aware edits splice on: ASCII maps 1:1,
    /// multibyte chars advance by their UTF-8 width, and an endpoint at (or past)
    /// the char count clamps to the byte length so an end-of-buffer or empty-buffer
    /// splice point stays valid instead of panicking.
    #[test]
    fn char_range_to_bytes_maps_and_clamps() {
        // ASCII: char indices equal byte offsets.
        assert_eq!(char_range_to_bytes("hello", &(1..4)), 1..4);
        // A bare caret (empty range) is a zero-width byte range at that offset.
        assert_eq!(char_range_to_bytes("hello", &(2..2)), 2..2);
        // End-of-string endpoint clamps to the byte length (an insertion point).
        assert_eq!(char_range_to_bytes("hello", &(5..5)), 5..5);
        // Empty buffer: any endpoint clamps to 0, the only valid splice point.
        assert_eq!(char_range_to_bytes("", &(0..0)), 0..0);
        // Multibyte: "héllo" is h(1) é(2) l(1) l(1) o(1); chars 1..3 = bytes 1..4.
        assert_eq!(char_range_to_bytes("héllo", &(1..3)), 1..4);
        // Selecting through the end of a multibyte string clamps correctly.
        assert_eq!(char_range_to_bytes("héllo", &(4..5)), 5..6);
    }

    /// Copy/Cut act on the highlighted span when there is one, and fall back to the
    /// whole buffer otherwise (no selection at all, or a bare caret) — preserving
    /// the pre-selection behaviour where right-click Copy grabbed the entire field.
    #[test]
    fn copy_target_prefers_span_else_whole_buffer() {
        // A real span copies just that slice.
        assert_eq!(copy_target("hello world", Some(&selection(6..11))), "world");
        // A reversed selection (primary/secondary swapped) is already ordered here.
        assert_eq!(copy_target("hello world", Some(&selection(0..5))), "hello");
        // A bare caret (empty range) falls back to the whole buffer.
        assert_eq!(
            copy_target("hello world", Some(&selection(3..3))),
            "hello world"
        );
        // No selection info at all also falls back to the whole buffer.
        assert_eq!(copy_target("hello world", None), "hello world");
        // Multibyte span slices on char boundaries.
        assert_eq!(copy_target("héllo", Some(&selection(0..2))), "hé");
    }

    /// Read a text widget's stored selection as an ordered char range.
    fn stored_range(ctx: &egui::Context, id: egui::Id) -> Option<std::ops::Range<usize>> {
        let range = egui::TextEdit::load_state(ctx, id)?.cursor.char_range()?;
        let (a, b) = (range.primary.index, range.secondary.index);
        Some(a.min(b)..a.max(b))
    }

    /// A right-click must not visibly clear the highlight: egui collapses a
    /// `TextEdit`'s span to a caret on any pointer press, so [`frozen_selection`]
    /// has to paint the shadowed span back into the widget state each frozen frame.
    /// We seed a span, fire a secondary press over the widget, and assert both the
    /// range the menu will act on *and* the widget's own stored selection survive.
    #[test]
    fn right_click_keeps_the_selection() {
        use egui_kittest::Harness;
        use std::cell::RefCell;
        use std::rc::Rc;

        let id = egui::Id::new("frozen_selection_test_edit");

        struct Shared {
            text: String,
            frame: usize,
            rect: egui::Rect,
            /// The range `frozen_selection` handed the menu on the latest frame.
            menu_range: Option<std::ops::Range<usize>>,
            /// The widget's own stored selection after `frozen_selection` ran.
            stored_after: Option<std::ops::Range<usize>>,
        }

        let shared = Rc::new(RefCell::new(Shared {
            text: "hello world".to_owned(),
            frame: 0,
            rect: egui::Rect::NOTHING,
            menu_range: None,
            stored_after: None,
        }));

        let closure_shared = shared.clone();
        let mut harness = Harness::new_ui(move |ui| {
            let mut sh = closure_shared.borrow_mut();
            let resp = ui.add(
                egui::TextEdit::multiline(&mut sh.text)
                    .id(id)
                    .desired_rows(2),
            );
            sh.rect = resp.rect;

            // Seed the "hello" span once, before any press, exactly as a mouse
            // drag-select would leave it.
            if sh.frame == 0 {
                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(5))));
                egui::TextEdit::store_state(ui.ctx(), id, state);
            }

            sh.menu_range = frozen_selection(ui, &resp).map(|s| s.range);
            sh.stored_after = stored_range(ui.ctx(), id);
            sh.frame += 1;
        });

        // Frame 0: lay out and seed the span (no press yet).
        harness.run_ok();
        assert_eq!(
            shared.borrow().menu_range,
            Some(0..5),
            "the seeded span should be visible before any right-click"
        );

        // Right-click the widget: hover it, then press the secondary button. egui
        // collapses the live selection to a caret on this press.
        let center = shared.borrow().rect.center();
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_ok();

        let sh = shared.borrow();
        assert_eq!(
            sh.menu_range,
            Some(0..5),
            "the menu handlers must still see the pre-right-click span"
        );
        assert_eq!(
            sh.stored_after,
            Some(0..5),
            "the widget's own selection must be restored so the highlight stays visible"
        );
    }
}
