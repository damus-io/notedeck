//! End-to-end tests for text overflow in the Dave chat UI.
//!
//! These go through the real Dave app rendering pipeline: Notedeck → Dave →
//! DaveUi → render_chat → render_assistant_message → render_inlines.
//!
//! Uses `DeviceHarness` from `notedeck_testing` so the device setup is shared
//! with every other Notedeck E2E suite (messages, etc.).

use egui_kittest::kittest::Queryable;
use notedeck::{App, AppContext, AppResponse};
use notedeck_dave::backend::traits::BackendType;
use notedeck_dave::{AiMode, Dave, ExecutedTool, Message, PermissionRequest, ToolResponse};
use notedeck_testing::device::{build_device_minimal, DeviceHarness};
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Dave app factories
// ---------------------------------------------------------------------------

/// Wrapper that simulates Chrome's StripBuilder layout around Dave.
///
/// Chrome renders Dave inside:
/// 1. A vertical `StripBuilder` (for toolbar/keyboard splits)
/// 2. With `item_spacing.x = 0` (set in Chrome::show())
/// 3. Inside a NavDrawer (which passes `ui` directly when closed)
///
/// This wrapper reproduces that environment so tests catch
/// overflow bugs that only appear in the real app path.
struct ChromeStripWrapper {
    dave: Dave,
}

impl App for ChromeStripWrapper {
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        self.dave.update(ctx, egui_ctx);
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // Chrome::show() sets item_spacing.x = 0 before the strip
        ui.spacing_mut().item_spacing.x = 0.0;

        let prev_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = 0.0;

        let mut response = AppResponse::none();
        egui_extras::StripBuilder::new(ui)
            .size(egui_extras::Size::remainder())
            .vertical(|mut strip| {
                strip.cell(|ui| {
                    ui.spacing_mut().item_spacing = prev_spacing;
                    response = self.dave.render(ctx, ui);
                });
            });
        response
    }
}

/// Minimal chrome-like wrapper around Dave that reserves a sidebar and container
/// frame before rendering chat content.
struct DaveChromeLikeWrapper {
    dave: Dave,
    sidebar_width: f32,
}

impl App for DaveChromeLikeWrapper {
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        self.dave.update(ctx, egui_ctx);
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut response = AppResponse::none();
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(self.sidebar_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label("menu");
                },
            );

            response = egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(6, 0))
                .show(ui, |ui| self.dave.render(ctx, ui))
                .inner;
        });
        response
    }
}

// ---------------------------------------------------------------------------
// Transcript injection
// ---------------------------------------------------------------------------

/// A transcript turn used to seed Dave sessions in tests.
#[derive(Clone, Debug)]
enum TranscriptTurn {
    User(String),
    Assistant(String),
    /// A tool response (rendered as compact "ToolName: summary" line)
    ToolResult {
        tool_name: String,
        summary: String,
    },
    /// A pending permission request (renders Allow/Deny/Always/Exit buttons)
    Permission {
        tool_name: String,
        tool_input: serde_json::Value,
    },
}

/// Build a user transcript turn.
fn user_msg(text: impl Into<String>) -> TranscriptTurn {
    TranscriptTurn::User(text.into())
}

/// Build an assistant transcript turn from markdown text.
fn assistant_msg(md: impl Into<String>) -> TranscriptTurn {
    TranscriptTurn::Assistant(md.into())
}

/// Build a tool result transcript turn.
fn tool_result(name: impl Into<String>, summary: impl Into<String>) -> TranscriptTurn {
    TranscriptTurn::ToolResult {
        tool_name: name.into(),
        summary: summary.into(),
    }
}

/// Build a pending permission request (renders Allow/Deny/Always/Exit buttons).
fn permission(tool_name: impl Into<String>, tool_input: serde_json::Value) -> TranscriptTurn {
    TranscriptTurn::Permission {
        tool_name: tool_name.into(),
        tool_input,
    }
}

/// Inject transcript turns into a Dave session. Extracted to eliminate
/// duplication across harness builders.
fn inject_transcript(
    dave: &mut Dave,
    notedeck: &mut notedeck::Notedeck,
    ctx: &egui::Context,
    sid: notedeck_dave::SessionId,
    turns: Vec<TranscriptTurn>,
) {
    for turn in turns {
        match turn {
            TranscriptTurn::User(text) => {
                let app_ctx = notedeck.app_context(ctx);
                let _ = dave.add_user_message_for_session(sid, &app_ctx, text, Vec::new());
            }
            TranscriptTurn::Assistant(text) => {
                if let Some(session) = dave.session_manager_mut().get_mut(sid) {
                    session.append_token(&text);
                    session.finalize_last_assistant();
                }
            }
            TranscriptTurn::ToolResult { tool_name, summary } => {
                if let Some(session) = dave.session_manager_mut().get_mut(sid) {
                    let result = ExecutedTool {
                        tool_name,
                        summary,
                        parent_task_id: None,
                        file_update: None,
                    };
                    let tool_resp = ToolResponse::executed_tool(result);
                    session.chat.push(Message::ToolResponse(tool_resp));
                }
            }
            TranscriptTurn::Permission {
                tool_name,
                tool_input,
            } => {
                if let Some(session) = dave.session_manager_mut().get_mut(sid) {
                    let req = PermissionRequest::new(
                        uuid::Uuid::new_v4(),
                        tool_name,
                        tool_input,
                        None,
                        None,
                        None,
                    );
                    session.chat.push(Message::PermissionRequest(req));
                }
            }
        }
    }
}

/// Create a temp working directory that stays alive when moved into a closure.
/// Returns `(path, guard)` — move `guard` into the closure to prevent early cleanup.
fn work_dir_pair() -> (std::path::PathBuf, Arc<TempDir>) {
    let dir = Arc::new(TempDir::new().expect("work tmpdir"));
    let path = dir.path().to_path_buf();
    (path, dir)
}

// ---------------------------------------------------------------------------
// Harness builders (all use DeviceHarness via build_device_minimal)
// ---------------------------------------------------------------------------

/// Build a Dave device with canned transcript at the given viewport width.
fn render_chat_harness(turns: Vec<TranscriptTurn>, width: f32) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, 800.0),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );

            inject_transcript(&mut dave, notedeck, ctx, sid, turns);

            dave.clear_overlay();
            notedeck.set_app(dave);
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Build a Dave device with canned transcript at a given width and pixels-per-point.
fn render_chat_harness_with_ppp(
    turns: Vec<TranscriptTurn>,
    width: f32,
    pixels_per_point: f32,
) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, 800.0),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            ctx.set_pixels_per_point(pixels_per_point);

            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );

            inject_transcript(&mut dave, notedeck, ctx, sid, turns);

            dave.clear_overlay();
            notedeck.set_app(dave);
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Render a single assistant markdown message through the full Dave pipeline.
fn render_markdown_harness(md: &str, width: f32) -> DeviceHarness {
    render_chat_harness(vec![assistant_msg(md)], width)
}

/// Build an empty Dave chat device (no messages).
fn render_empty_chat_harness(width: f32) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, 800.0),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let _sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );
            dave.clear_overlay();
            notedeck.set_app(dave);
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Build an empty Dave chat wrapped in a chrome-like sidebar layout.
fn render_empty_wrapped_chat_harness(width: f32, sidebar_width: f32) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, 800.0),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let _sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );
            dave.clear_overlay();

            notedeck.set_app(DaveChromeLikeWrapper {
                dave,
                sidebar_width,
            });
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Return every queryable node with horizontal bounds that escapes the viewport.
/// The chat frame's right margin — content must stop this far inside the
/// viewport edge.  Uses the exported constants from dave.rs.
fn chat_right_margin(width: f32) -> f32 {
    let (narrow, wide) = notedeck_dave::ui::chat_margins();
    // Mirror the runtime logic: is_narrow returns `width < NARROW_SCREEN_WIDTH`,
    // so width >= NARROW_SCREEN_WIDTH is "wide".
    if width < notedeck::ui::NARROW_SCREEN_WIDTH {
        f32::from(narrow)
    } else {
        f32::from(wide)
    }
}

fn horizontal_overflows(harness: &DeviceHarness, width: f32) -> Vec<String> {
    let left_tolerance = 0.0;
    let right_tolerance = 1.0;
    // The chat frame adds equal left/right margins. Content inside it must
    // fit within `width - margin`, not the full viewport.  Elements in the
    // top buttons area (y < 70) and status bar are outside the chat frame
    // and use the full viewport width.
    let chat_right = width - chat_right_margin(width);
    let viewport_right = width;
    // Chat content starts below top buttons + chat_frame top_margin (50px)
    let chat_top = 70.0_f64;
    let screen_rect = harness
        .input()
        .screen_rect
        .unwrap_or_else(|| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0)));

    harness
        .query_all(egui_kittest::kittest::by())
        .filter_map(|node| {
            let bounds = node.raw_bounds()?;
            if bounds.y1 < f64::from(screen_rect.top())
                || bounds.y0 > f64::from(screen_rect.bottom())
                || bounds.x1 < f64::from(screen_rect.left())
                || bounds.x0 > f64::from(screen_rect.right())
            {
                return None;
            }
            // Include non-text nodes (buttons, frames) — use their role name
            // as a fallback description so overflowing non-text elements are
            // also caught.  Skip Unknown-role nodes (scrollbars, containers)
            // that don't represent user-visible content.
            let text = node
                .label()
                .filter(|s| !s.is_empty())
                .or_else(|| node.value().filter(|s| !s.is_empty()))
                .map(|s| s.to_owned());
            let text = match text {
                Some(t) => t,
                None => {
                    let role = node.role();
                    if role == egui::accesskit::Role::Unknown
                        || role == egui::accesskit::Role::GenericContainer
                        || role == egui::accesskit::Role::ScrollView
                    {
                        return None;
                    }
                    format!("<{role:?}>")
                }
            };
            // Use tighter boundary for chat content, full viewport for
            // top buttons / status bar.
            let right_bound = if bounds.y0 >= chat_top {
                chat_right
            } else {
                viewport_right
            };
            let escapes_left = bounds.x0 < -left_tolerance;
            let escapes_right = bounds.x1 > f64::from(right_bound + right_tolerance);
            if escapes_left || escapes_right {
                Some(format!(
                    "text={text:?} bounds=({:.1}, {:.1})-({:.1}, {:.1})",
                    bounds.x0, bounds.y0, bounds.x1, bounds.y1
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Return all nodes containing the marker text whose right edge reaches or exceeds
/// the viewport boundary.
fn marker_right_edge_clips(harness: &DeviceHarness, right_edge: f64, marker: &str) -> Vec<String> {
    marker_right_edge_clips_with_headroom(harness, right_edge, marker, 0.0)
}

/// Return all nodes containing marker text whose right edge reaches into the
/// required headroom zone from the chat boundary.
fn marker_right_edge_clips_with_headroom(
    harness: &DeviceHarness,
    right_edge: f64,
    marker: &str,
    min_headroom_px: f64,
) -> Vec<String> {
    let min_chat_y = 70.0;

    harness
        .query_all(egui_kittest::kittest::by())
        .filter_map(|node| {
            if node
                .query_all(egui_kittest::kittest::by().recursive(false))
                .next()
                .is_some()
            {
                return None;
            }

            let bounds = node.raw_bounds()?;
            if bounds.y0 < min_chat_y {
                return None;
            }
            let text = node
                .label()
                .filter(|s| !s.is_empty())
                .or_else(|| node.value().filter(|s| !s.is_empty()))
                .map(|s| s.to_owned())?;
            if !text.contains(marker) {
                return None;
            }

            let escapes_right_edge = bounds.x1 > (right_edge - min_headroom_px);
            if escapes_right_edge {
                Some(format!(
                    "text={text:?} bounds=({:.1}, {:.1})-({:.1}, {:.1})",
                    bounds.x0, bounds.y0, bounds.x1, bounds.y1
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Inspect egui paint output directly and return marker text rows whose glyph
/// mesh reaches into the clip-rect right-edge headroom zone.
fn marker_textshape_rows_touch_clip_right(
    harness: &DeviceHarness,
    marker: &str,
    min_headroom_px: f32,
) -> Vec<String> {
    fn visit_shape(
        shape: &egui::epaint::Shape,
        clip_rect: egui::Rect,
        marker: &str,
        min_headroom_px: f32,
        hits: &mut Vec<String>,
    ) {
        match shape {
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    visit_shape(shape, clip_rect, marker, min_headroom_px, hits);
                }
            }
            egui::epaint::Shape::Text(text_shape) => {
                if !text_shape.galley.job.text.contains(marker) {
                    return;
                }

                for row in &text_shape.galley.rows {
                    let row_text = row.text();
                    if !row_text.contains(marker) {
                        continue;
                    }

                    let row_mesh_bounds =
                        row.visuals.mesh_bounds.translate(text_shape.pos.to_vec2());
                    let clip_limit = clip_rect.right() - min_headroom_px;
                    if row_mesh_bounds.right() > clip_limit {
                        hits.push(format!(
                            "row={row_text:?} row_right={:.1} clip_right={:.1} headroom={min_headroom_px:.1}",
                            row_mesh_bounds.right(),
                            clip_rect.right()
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let mut hits = Vec::new();
    for clipped_shape in &harness.output().shapes {
        visit_shape(
            &clipped_shape.shape,
            clipped_shape.clip_rect,
            marker,
            min_headroom_px,
            &mut hits,
        );
    }
    hits
}

/// Compute the chat container's right edge from the viewport width and
/// known chat margins.
fn chat_right_edge(width: f32) -> f64 {
    f64::from(width - chat_right_margin(width))
}

/// Build a Dave device with canned transcript at a given width and height.
///
/// Use a short height to force vertical scrolling, which tests whether the
/// scrollbar eats into content width correctly.
fn render_chat_harness_sized(turns: Vec<TranscriptTurn>, width: f32, height: f32) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, height),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );

            inject_transcript(&mut dave, notedeck, ctx, sid, turns);

            dave.clear_overlay();
            notedeck.set_app(dave);
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Build a Dave device wrapped in ChromeStripWrapper at a given width.
///
/// This simulates the real Chrome rendering environment where Dave is rendered
/// inside a vertical StripBuilder with `item_spacing.x = 0`.
fn render_chat_harness_chrome(turns: Vec<TranscriptTurn>, width: f32) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, 800.0),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );

            inject_transcript(&mut dave, notedeck, ctx, sid, turns);

            dave.clear_overlay();
            notedeck.set_app(ChromeStripWrapper { dave });
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Render markdown through the full Dave app pipeline and assert no widget
/// escapes the viewport at the given widths.
fn assert_markdown_has_no_horizontal_overflow(md: &str, widths: &[f32]) {
    assert_chat_has_no_horizontal_overflow(vec![assistant_msg(md)], widths);
}

/// Render a full mixed chat transcript and assert no widget escapes the viewport.
fn assert_chat_has_no_horizontal_overflow(turns: Vec<TranscriptTurn>, widths: &[f32]) {
    for width in widths {
        let harness = render_chat_harness(turns.clone(), *width);
        let overflows = horizontal_overflows(&harness, *width);
        assert!(
            overflows.is_empty(),
            "found horizontal overflow at width {width}: {:?}",
            overflows
        );
    }
}

/// Like `assert_chat_has_no_horizontal_overflow` but with a short viewport
/// height to force vertical scrolling. This tests whether the vertical
/// scrollbar's width is properly accounted for.
fn assert_chat_has_no_horizontal_overflow_with_scrolling(
    turns: Vec<TranscriptTurn>,
    widths: &[f32],
) {
    // Use a short viewport (300px) so the chat content triggers vertical
    // scrolling and the scrollbar eats into horizontal space.
    for width in widths {
        let harness = render_chat_harness_sized(turns.clone(), *width, 300.0);
        let overflows = horizontal_overflows(&harness, *width);
        assert!(
            overflows.is_empty(),
            "found horizontal overflow at width {width} with scrolling: {:?}",
            overflows
        );
    }
}

/// Build a Chrome-wrapped harness with configurable size for scrollbar tests.
fn render_chat_harness_chrome_sized(
    turns: Vec<TranscriptTurn>,
    width: f32,
    height: f32,
) -> DeviceHarness {
    let (work_path, guard) = work_dir_pair();

    let mut harness = build_device_minimal(
        egui::Vec2::new(width, height),
        Box::new(move |notedeck, ctx| {
            let _keep = guard;
            let ndb = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.ndb.clone()
            };
            let path = {
                let app_ctx = notedeck.app_context(ctx);
                app_ctx.path.clone()
            };

            let mut dave = Dave::new(None, ndb, ctx.clone(), &path);
            let sid = dave.session_manager_mut().new_session(
                work_path,
                AiMode::Agentic,
                BackendType::Claude,
            );

            inject_transcript(&mut dave, notedeck, ctx, sid, turns);

            dave.clear_overlay();
            notedeck.set_app(ChromeStripWrapper { dave });
        }),
    );

    for _ in 0..5 {
        harness.step();
    }

    harness
}

/// Chrome-wrapped assertion with short viewport to force vertical scrollbar.
fn assert_chat_has_no_horizontal_overflow_chrome_scrolling(
    turns: Vec<TranscriptTurn>,
    widths: &[f32],
) {
    for width in widths {
        let harness = render_chat_harness_chrome_sized(turns.clone(), *width, 300.0);
        let overflows = horizontal_overflows(&harness, *width);
        assert!(
            overflows.is_empty(),
            "found horizontal overflow at chrome width {width} with scrolling: {:?}",
            overflows
        );
    }
}

// ---------------------------------------------------------------------------
// Test cases
// ---------------------------------------------------------------------------

/// Regression test: bold text interleaved with inline code in unordered list items
/// must not cause content to flow off the left edge of the screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_text_in_unordered_lists_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Here's what you need to know about the implementation:\n\n\
         - The **`CollapsingState`** struct from `egui::collapsing_header::CollapsingState` \
         manages the expand/collapse state. You should call **`show_body_unindented()`** to \
         render content without extra indentation, and use **`show_header()`** with a closure \
         for custom header rendering.\n\
         - **Important:** always check `is_open()` before accessing the body content to avoid \
         unnecessary computation.\n\
         - Use **`set_open()`** to programmatically control the collapsed state when needed.",
        &[300.0, 240.0],
    );
}

/// Ordered list items use the same nested layout shape as unordered lists and must
/// keep bold spans inside the same `LayoutJob`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_text_in_ordered_lists_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Implementation checklist:\n\n\
         1. **Create** the `CollapsingState` before rendering the header so the state is \
         available to both the summary and expanded views.\n\
         2. **Measure** the available width before mixing `**bold**` spans with inline \
         code like `show_body_unindented()` in the same sentence.\n\
         3. **Verify** long follow-up text wraps after the emphasized segment instead of \
         forcing the entire item past the chat margin.",
        &[300.0, 240.0],
    );
}

/// Nested list items add another level of indentation on top of the original
/// horizontal/vertical/horizontal_wrapped structure, which previously made the
/// overflow easier to trigger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_text_in_nested_lists_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Implementation details:\n\n\
         - **Primary task:** render the top-level list item in the normal flow.\n\
           - **Nested detail:** a deeply indented item can mention `show_header()` and \
           `show_body_unindented()` in the same line without shifting left.\n\
           - **Nested warning:** long explanatory text after **strong emphasis** still \
           wraps inside the visible chat column.",
        &[300.0, 240.0],
    );
}

/// Mixed strong and strong+italic spans should stay in the `LayoutJob` even when
/// the emphasized segments appear at the start and middle of list items.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_italic_list_content_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Release notes:\n\n\
         - ***Critical path:*** keep the emphasized prefix inside the same layout job as \
         the remainder of the list item.\n\
         - When `render_inlines()` sees ***bold italic*** text between plain text and \
         `inline_code()`, the line should still wrap without escaping the left margin.\n\
         - End the paragraph with **one more strong span** so multiple emphasis changes are \
         exercised in one rendered list.",
        &[300.0, 240.0],
    );
}

/// A nested fenced code block under a bold list item makes any upstream width
/// expansion much more obvious, because the framed code region gets shifted too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_list_item_with_nested_code_block_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Implementation plan:\n\n\
         - **Primary path:** keep the list item content inside the same layout job before \
         showing the example below.\n\
           ```rust\n\
           let state = egui::collapsing_header::CollapsingState::load_with_default_open(\n\
               ui.ctx(),\n\
               ui.make_persistent_id(\"example\"),\n\
               true,\n\
           );\n\
           state.show_body_unindented(ui, |ui| {\n\
               ui.label(\"body\");\n\
           });\n\
           ```\n\
         - **Follow-up:** after the code block, the next item should still align inside the \
         visible chat column.",
        &[300.0, 240.0],
    );
}

/// Standalone fenced code blocks should stay within the chat column even when
/// introduced by a paragraph with multiple emphasis changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emphasized_paragraph_followed_by_code_block_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Use **strong emphasis** before the example so the preceding paragraph exercises the \
         inline formatting path, then render the code block below without shifting the chat \
         layout.\n\n\
         ```rust\n\
         let builder = egui::Frame::default()\n\
             .inner_margin(8.0)\n\
             .corner_radius(4.0)\n\
             .fill(ui.visuals().panel_fill);\n\
         ```\n\n\
         After the block, render one more **bold marker** to ensure normal inline content still \
         lines up with the rest of the conversation.",
        &[300.0, 240.0],
    );
}

/// Control coverage for the full Dave harness at the same widths used by the
/// exact repro paragraph.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_paragraph_control_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "This is a plain paragraph without lists, code blocks, file names, or long snake_case \
         identifiers. It should wrap normally inside the Dave chat column at standard narrow \
         widths.",
        &[300.0, 240.0, 220.0, 210.0, 200.0],
    );
}

/// The reported repro paragraph should stay within the Dave chat viewport at the
/// same widths where the plain control still behaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_repro_paragraph_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "The Linux issue is fixed in messages_e2e.rs. \
         restart_should_prefetch_newer_known_participant_relay_list_e2e now waits until \
         sender_current has actually ingested the recipient's relay-A DM relay list before \
         the first send, instead of relying on the fixed warmup alone.",
        &[300.0, 240.0],
    );
}

/// Ordered-list repro from review text should stay inside the Dave chat viewport
/// at the same narrow widths as the other markdown regressions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordered_list_repro_paragraph_has_no_left_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "1. Medium: the restart E2E tests are not fully correct as written; at least one is \
         flaky because it waits on relay-side giftwrap counts without continuing to step the \
         sender that flushes outbound publishes. The helper at messages_e2e.rs only polls the \
         relay DB. In messages_e2e.rs, messages_e2e.rs, messages_e2e.rs, and messages_e2e.rs, \
         the test stops stepping sender_device before asserting the relay count increase. I \
         reproduced that flake directly: \
         same_account_device_restart_catches_up_from_existing_db_e2e failed once with \
         \"expected relay giftwrap count at least 2, actual 1\", then passed immediately on \
         rerun. The same pattern exists in the many-conversation restart variants too.",
        &[300.0, 280.0, 260.0, 240.0, 220.0, 210.0, 200.0, 198.0],
    );
}

/// Mixed markdown with real links and fenced code blocks should wrap inside the
/// chat column at narrow widths without left/right overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn links_and_codeblocks_markdown_has_no_horizontal_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Renderer-level in [markdown_ui.rs](https://example.com/markdown_ui.rs)\n\n\
         Inline checks: `messages_e2e.rs`, \
         `restart_should_prefetch_newer_known_participant_relay_list_e2e`, and \
         [messages_e2e.rs](https://example.com/messages_e2e.rs) should wrap cleanly.\n\n\
         - plain paragraph control at narrow width\n\
         - paragraph followed by code block at narrow width\n\
         - your exact repro paragraph across multiple widths\n\n\
         Full Dave E2E in [text_overflow_tests.rs](https://example.com/text_overflow_tests.rs)\n\n\
         ```rust\n\
         let test_name = \"restart_should_prefetch_newer_known_participant_relay_list_e2e\";\n\
         let file = \"messages_e2e.rs\";\n\
         let value = \"expected relay giftwrap count at least 2, actual 1\";\n\
         ```\n\n\
         Then verify [messages_e2e.rs](https://example.com/messages_e2e.rs) still wraps cleanly.",
        &[300.0, 260.0, 240.0, 220.0, 200.0],
    );
}

/// Coverage for review-style markdown with file links and inline code where
/// clipping was observed on the left side in narrow mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_markdown_with_links_and_inline_code_has_no_left_clipping() {
    assert_markdown_has_no_horizontal_overflow(
        "Mostly yes.\n\n\
         The strongest one is [lib.rs:4560](https://example.com/lib.rs#L4560), `cross_restart`.\n\n\
         That is a real persistence test:\n\
         - it saves through Dave's toggle path\n\
         - it writes the real `collapse_state.json`\n\
         - it constructs a fresh Dave\n\
         - it proves the new instance loads the saved state\n\n\
         commit added.\n\n\
         [lib.rs:102](https://example.com/lib.rs#L102) is also good.\n\
         protects the `Serialize/Deserialize` branch.",
        &[300.0, 260.0, 240.0, 220.0, 200.0],
    );
}

/// Stress mixed chat rendering with long user text and long assistant markdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_mixed_chat_with_user_messages_has_no_horizontal_overflow() {
    let user_unbroken = "sameaccountdevicerestartcatchesupfromexistingdbe2ewithoutseparators";
    let follow_up_unbroken =
        "restartshouldprefetchnewerknownparticipantrelaylistwithoutbreakinglayout";

    let turns = vec![
        user_msg(format!(
            "User repro: this should stay visible even with a long unbroken token: {user_unbroken}. \
             Also include file references like messages_e2e.rs and lib.rs:4560."
        )),
        assistant_msg(
            "Mostly yes.\n\n\
             The strongest one is [lib.rs:4560](https://example.com/lib.rs#L4560), `cross_restart`.\n\
             It writes the real `collapse_state.json`, then constructs a fresh Dave and verifies load.\n\n\
             ```rust\n\
             let test_name = \"restart_should_prefetch_newer_known_participant_relay_list_e2e\";\n\
             let value = \"expected relay giftwrap count at least 2, actual 1\";\n\
             ```\n\n\
             See [messages_e2e.rs](https://example.com/messages_e2e.rs) for details.",
        ),
        user_msg(format!(
            "Follow-up question with another long token: {follow_up_unbroken}. \
             Are you sure these are good tests?"
        )),
    ];

    assert_chat_has_no_horizontal_overflow(
        turns,
        &[
            300.0, 260.0, 240.0, 220.0, 200.0, 190.0, 180.0, 170.0, 160.0,
        ],
    );
}

/// Long link labels are rendered via hyperlink widgets (not the markdown LayoutJob),
/// so verify they still stay inside bounds in mixed chat transcripts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_link_label_with_user_messages_has_no_horizontal_overflow() {
    let turns = vec![
        user_msg("Can you review this regression and include exact references in your answer?"),
        assistant_msg(
            "I can reproduce it from \
             [restart_should_prefetch_newer_known_participant_relay_list_e2e](https://example.com/repro) \
             and \
             [same_account_device_restart_catches_up_from_existing_db_e2e](https://example.com/repro2).\n\n\
             Then I check \
             [messages_e2e.rs:999](https://example.com/messages_e2e.rs#L999) and \
             [session_events.rs:1512](https://example.com/session_events.rs#L1512).",
        ),
        user_msg(
            "Please also include an inline code reference to \
             restart_should_prefetch_newer_known_participant_relay_list_e2e.",
        ),
    ];

    assert_chat_has_no_horizontal_overflow(
        turns,
        &[
            300.0, 260.0, 240.0, 220.0, 200.0, 190.0, 180.0, 170.0, 160.0,
        ],
    );
}

/// Narrow layouts should not stack status badges on top of each other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_badges_do_not_overlap_in_narrow_mixed_chat() {
    let turns = vec![
        user_msg("Please generate a long report and include links, inline code, and a summary."),
        assistant_msg(
            "Mostly yes.\n\n\
             The strongest one is [lib.rs:4560](https://example.com/lib.rs#L4560), `cross_restart`.\n\
             That is a real persistence test and it writes `collapse_state.json`.\n\n\
             ```rust\n\
             let test_name = \"restart_should_prefetch_newer_known_participant_relay_list_e2e\";\n\
             let value = \"expected relay giftwrap count at least 2, actual 1\";\n\
             ```\n\n\
             See [messages_e2e.rs](https://example.com/messages_e2e.rs) for details.",
        ),
        user_msg("Are you sure these are good tests?"),
    ];

    let harness = render_chat_harness(turns, 240.0);

    let mut badges = Vec::new();
    for label in ["AUTO", "PLAN", "CMP"] {
        if let Some(node) = harness.query_by_label(label) {
            if let Some(bounds) = node.raw_bounds() {
                badges.push((label, bounds));
            }
        }
    }

    assert!(
        badges.len() >= 2,
        "expected at least two status badges in narrow mode"
    );

    for i in 0..badges.len() {
        for j in (i + 1)..badges.len() {
            let (left_label, left_bounds) = badges[i];
            let (right_label, right_bounds) = badges[j];
            let overlap_x = (left_bounds.x1.min(right_bounds.x1)
                - left_bounds.x0.max(right_bounds.x0))
            .max(0.0);
            assert!(
                overlap_x <= 1.0,
                "status badges overlap: {left_label}={left_bounds:?} \
                 {right_label}={right_bounds:?}"
            );
        }
    }
}

/// In the full Dave app, inline links are rendered inside the same LayoutJob
/// as surrounding text, so they naturally stay on the same row when width allows.
/// This test verifies the combined text (including link text) appears as a single
/// label node without overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_app_inline_link_stays_on_same_row_when_width_allows() {
    let turns = vec![
        user_msg("Please verify this rendering behavior."),
        assistant_msg(
            "Renderer-level in [markdown_ui.rs](https://example.com/markdown_ui.rs): \
             this sentence should keep the link inline when width allows.",
        ),
    ];

    // Links are now inline in the LayoutJob, so the combined text (including
    // link text) is a single label. Just verify no overflow at this width.
    assert_chat_has_no_horizontal_overflow(turns, &[360.0, 300.0, 240.0, 200.0]);
}

/// Probe for unbroken hyperlink labels, which cannot be wrapped by markdown text soft-wrap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbroken_long_link_label_with_user_messages_has_no_horizontal_overflow() {
    let long_label = "restartshouldprefetchnewerknownparticipantrelaylistwithoutanyseparatorsandwithoutanyspacesforwrapping";
    let markdown =
        format!("Investigate [{long_label}](https://example.com/repro) and summarize findings.");

    let turns = vec![
        user_msg("Please include links in your answer."),
        assistant_msg(markdown),
        user_msg("Thanks."),
    ];

    assert_chat_has_no_horizontal_overflow(turns, &[260.0, 240.0, 220.0, 200.0, 180.0]);
}

/// Probe long inline code tokens without separators, which do not get injected
/// soft-wrap opportunities from separator-based wrapping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbroken_long_inline_code_with_user_messages_has_no_horizontal_overflow() {
    let markdown = "Inline code check: \
                    `sameaccountdevicerestartcatchesupfromexistingdbwithoutseparators` \
                    should not overflow in narrow mode.";

    let turns = vec![
        user_msg("Can you include the raw identifier too?"),
        assistant_msg(markdown),
        user_msg("Please confirm it renders correctly."),
    ];

    assert_chat_has_no_horizontal_overflow(
        turns,
        &[260.0, 240.0, 220.0, 200.0, 190.0, 180.0, 170.0, 160.0],
    );
}

/// Seeded user transcript path should not clip user message text on the right.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeded_user_message_does_not_clip_on_right_edge() {
    let marker = "USER_RIGHT_EDGE_REPRO";
    let user_text = format!(
        "{marker}: The Linux issue is fixed in messages_e2e.rs and \
         restart_should_prefetch_newer_known_participant_relay_list_e2e now waits correctly."
    );

    for width in [
        320.0, 300.0, 280.0, 260.0, 240.0, 220.0, 200.0, 190.0, 180.0,
    ] {
        let harness = render_chat_harness(vec![user_msg(user_text.clone())], width);
        let right_edge = chat_right_edge(width);
        let clipped = marker_right_edge_clips(&harness, right_edge, marker);
        assert!(
            clipped.is_empty(),
            "seeded user message clipped at width {width} (edge={right_edge:.1}): {clipped:?}"
        );
    }
}

/// Real UI input path (type + Enter) should not clip user message text on the right.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_does_not_clip_on_right_edge() {
    let marker = "USER_TYPED_RIGHT_EDGE_REPRO";
    let user_text = format!(
        "{marker}: Please verify this exact text remains fully visible with \
         messages_e2e.rs references and long-but-normal sentence structure."
    );

    for width in [
        320.0, 300.0, 280.0, 260.0, 240.0, 220.0, 200.0, 190.0, 180.0,
    ] {
        let mut harness = render_empty_chat_harness(width);
        let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        input.click();
        harness.step();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text(&user_text);
        harness.step();
        harness.press_key(egui::Key::Enter);
        for _ in 0..4 {
            harness.step();
        }

        let right_edge = chat_right_edge(width);
        let clipped = marker_right_edge_clips(&harness, right_edge, marker);
        assert!(
            clipped.is_empty(),
            "typed user message clipped at width {width} (edge={right_edge:.1}): {clipped:?}"
        );
    }
}

/// High-DPI rendering can expose pixel-rounding edge clipping, so probe with
/// non-even widths and a 2.0 pixels-per-point scale.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeded_user_message_does_not_clip_on_right_edge_high_dpi() {
    let marker = "USER_RIGHT_EDGE_REPRO_HIDPI";
    let user_text = format!(
        "{marker}: This sentence should remain fully visible at high DPI when \
         the viewport width changes."
    );

    for width in [
        321.0, 301.0, 281.0, 261.0, 241.0, 221.0, 201.0, 191.0, 181.0,
    ] {
        let harness = render_chat_harness_with_ppp(vec![user_msg(user_text.clone())], width, 2.0);
        let right_edge = chat_right_edge(width);
        let clipped = marker_right_edge_clips(&harness, right_edge, marker);
        assert!(
            clipped.is_empty(),
            "seeded high-dpi user message clipped at width {width} (edge={right_edge:.1}): {clipped:?}"
        );
    }
}

/// Resize-in-place can expose stale layout/wrap bugs not seen in fresh harnesses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_does_not_clip_after_runtime_resize() {
    let marker = "USER_TYPED_RUNTIME_RESIZE_REPRO";
    let user_text = format!(
        "{marker}: This user message should remain fully visible after runtime \
         width changes without clipping on the right edge."
    );

    let mut harness = render_empty_chat_harness(340.0);
    let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    input.click();
    harness.step();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text(&user_text);
    harness.step();
    harness.press_key(egui::Key::Enter);
    for _ in 0..4 {
        harness.step();
    }

    for width in [300.0, 260.0, 220.0, 200.0, 190.0, 180.0] {
        harness.input_mut().screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 800.0),
        ));
        for _ in 0..3 {
            harness.step();
        }

        let clipped = marker_right_edge_clips(&harness, chat_right_edge(width), marker);
        assert!(
            clipped.is_empty(),
            "typed user message clipped after runtime resize to width {width}: {clipped:?}"
        );
    }
}

/// Chrome-like container pressure should still keep user bubble text fully visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_does_not_clip_after_runtime_resize_chrome_like_wrapper() {
    let marker = "USER_TYPED_CHROME_LIKE_REPRO";
    let user_text = format!(
        "{marker}: This user message should remain fully visible after runtime \
         width changes even with sidebar/container pressure."
    );

    let mut harness = render_empty_wrapped_chat_harness(360.0, 44.0);
    let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    input.click();
    harness.step();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text(&user_text);
    harness.step();
    harness.press_key(egui::Key::Enter);
    for _ in 0..4 {
        harness.step();
    }

    for width in [
        320.0, 300.0, 280.0, 260.0, 240.0, 220.0, 200.0, 190.0, 180.0,
    ] {
        harness.input_mut().screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 800.0),
        ));
        for _ in 0..3 {
            harness.step();
        }

        let clipped = marker_right_edge_clips(&harness, chat_right_edge(width), marker);
        assert!(
            clipped.is_empty(),
            "typed user message clipped in chrome-like wrapper at width {width}: {clipped:?}"
        );
    }
}

/// User text that includes long code-like tokens and fenced code should still
/// remain visible at narrow widths through the typed-input path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_with_codeblock_does_not_clip_on_right_edge() {
    let marker = "USER_TYPED_CODEBLOCK_RIGHT_EDGE_REPRO";
    let user_text = format!(
        "{marker}\n\n\
         The Linux issue is fixed in messages_e2e.rs. \
         restart_should_prefetch_newer_known_participant_relay_list_e2e now waits until \
         sender_current has actually ingested the recipient relay-A DM relay list before \
         the first send.\n\n\
         ```rust\n\
         let check = \"restart_should_prefetch_newer_known_participant_relay_list_e2e\";\n\
         let file = \"messages_e2e.rs\";\n\
         ```"
    );

    for width in [
        360.0, 340.0, 320.0, 300.0, 280.0, 260.0, 240.0, 220.0, 200.0,
    ] {
        let mut harness = render_empty_chat_harness(width);
        let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        input.click();
        harness.step();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text(&user_text);
        harness.step();
        harness.press_key(egui::Key::Enter);
        for _ in 0..5 {
            harness.step();
        }

        let right_edge = chat_right_edge(width);
        let clipped = marker_textshape_rows_touch_clip_right(&harness, marker, 1.0);
        assert!(
            clipped.is_empty(),
            "typed user codeblock message clipped at width {width} (edge={right_edge:.1}): {clipped:?}"
        );
    }
}

/// Combine high-DPI and runtime-resize pressure for user messages with code-like text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_with_codeblock_does_not_clip_after_hidpi_runtime_resize() {
    let marker = "USER_TYPED_CODEBLOCK_HIDPI_RUNTIME_REPRO";
    let user_text = format!(
        "{marker}: restart_should_prefetch_newer_known_participant_relay_list_e2e \
         waits until sender_current ingests recipient relay list before first send."
    );

    let mut harness = render_chat_harness_with_ppp(vec![user_msg("warmup")], 361.0, 2.0);

    let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    input.click();
    harness.step();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text(&user_text);
    harness.step();
    harness.press_key(egui::Key::Enter);
    for _ in 0..5 {
        harness.step();
    }

    for width in [
        341.0, 321.0, 301.0, 281.0, 261.0, 241.0, 221.0, 201.0, 191.0,
    ] {
        harness.input_mut().screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 800.0),
        ));
        for _ in 0..4 {
            harness.step();
        }

        let right_edge = chat_right_edge(width);
        let clipped = marker_textshape_rows_touch_clip_right(&harness, marker, 1.0);
        assert!(
            clipped.is_empty(),
            "typed user codeblock message clipped at hidpi runtime width {width} (edge={right_edge:.1}): {clipped:?}"
        );
    }
}

/// Sweep runtime widths and scale factors for a short user-authored message to
/// catch edge-alignment clipping that only appears at specific pixel ratios.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_user_message_short_text_has_right_edge_headroom_across_width_and_scale_sweep() {
    let marker = "USER_TYPED_SHORT_SWEEP_REPRO";
    let user_text = format!("{marker}: Are you sure these are good tests?");
    let mut failures = Vec::new();

    for pixels_per_point in [1.0f32, 1.5f32, 2.0f32] {
        let mut harness = render_chat_harness_with_ppp(vec![], 361.0, pixels_per_point);

        let input = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        input.click();
        harness.step();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text(&user_text);
        harness.step();
        harness.press_key(egui::Key::Enter);
        for _ in 0..4 {
            harness.step();
        }

        for width in (181..=361).rev().step_by(2) {
            let width = width as f32;
            harness.input_mut().screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 800.0),
            ));
            for _ in 0..2 {
                harness.step();
            }

            let clipped = marker_textshape_rows_touch_clip_right(&harness, marker, 1.0);
            if !clipped.is_empty() {
                failures.push(format!(
                    "ppp={pixels_per_point:.1} width={width:.1} clipped={clipped:?}"
                ));
                break;
            }
        }
    }

    assert!(
        failures.is_empty(),
        "short user message lost right-edge headroom under width/scale sweep: {failures:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordered_list_repro_marker_stays_visible_at_200px() {
    let raw = "1. Medium: the restart E2E tests are not fully correct as written; at least one is \
               flaky because it waits on relay-side giftwrap counts without continuing to step \
               the sender that flushes outbound publishes. The helper at messages_e2e.rs only \
               polls the relay DB. In messages_e2e.rs, messages_e2e.rs, messages_e2e.rs, and \
               messages_e2e.rs, the test stops stepping sender_device before asserting the relay \
               count increase. I reproduced that flake directly: \
               same_account_device_restart_catches_up_from_existing_db_e2e failed once with \
               \"expected relay giftwrap count at least 2, actual 1\", then passed immediately on \
               rerun. The same pattern exists in the many-conversation restart variants too.";
    let harness = render_markdown_harness(raw, 200.0);

    let paragraph = harness.get_by_label_contains("1. Medium: the restart E2E tests");
    let bounds = paragraph.raw_bounds().expect("paragraph bounds");
    assert!(
        bounds.x0 >= 0.0,
        "ordered-list paragraph was pushed off the left edge: ({:.1}, {:.1})-({:.1}, {:.1})",
        bounds.x0,
        bounds.y0,
        bounds.x1,
        bounds.y1
    );
}

// ---------------------------------------------------------------------------
// Complex markdown stress tests
// ---------------------------------------------------------------------------

/// Heading followed by paragraph with bold, links, and inline code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heading_bold_link_code_paragraph_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "## Implementation Summary\n\n\
         The **`render_inlines`** function in \
         [markdown_ui.rs](https://example.com/markdown_ui.rs) processes each \
         `InlineElement` and appends it to a shared `LayoutJob`. When it encounters \
         a **link**, it must `flush_job()` first, then call `ui.hyperlink_to()`. \
         This flush/re-add cycle is where width accounting can go wrong.\n\n\
         See also: [egui docs](https://docs.rs/egui) and `egui::text::LayoutJob`.",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Multiple paragraphs with links interspersed in the middle of sentences.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_paragraphs_with_mid_sentence_links_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "First paragraph references [file_one.rs](https://example.com/one) and continues \
         with more text after the link that should wrap properly.\n\n\
         Second paragraph starts with plain text, then has [another_link.rs](https://example.com/two) \
         followed by `inline_code_here` and **bold text** all in the same line. The remaining \
         text after all these inline elements must still wrap within bounds.\n\n\
         Third paragraph is just plain text to verify the layout recovers after the complex \
         inline content above.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Blockquote containing bold, code, and links.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blockquote_with_rich_inlines_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "> **Important:** The `flush_job()` function in \
         [markdown_ui.rs](https://example.com/markdown_ui.rs) sets \
         `job.wrap.max_width` to `ui.available_width() - 1.0` before adding \
         the label. This accounts for subpixel rounding but may not be enough \
         when nested inside a `BlockQuote` frame with its own inner margin.\n\n\
         Text after the blockquote should still wrap normally.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Table followed by paragraph — table columns must be constrained to fit
/// within the available width without overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn table_then_paragraph_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "| File | Status | Notes |\n\
         |------|--------|-------|\n\
         | markdown_ui.rs | Modified | Fixed flush_job width |\n\
         | dave.rs | Modified | Added content_width constraint |\n\
         | text_overflow_tests.rs | Added | E2E overflow coverage |\n\n\
         After the table, this paragraph with **bold text** and `inline code` \
         and a [link](https://example.com) should still wrap within the chat \
         column boundaries without any overflow.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Deeply nested list with bold, code, links at each level.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deeply_nested_list_with_rich_content_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "- **Top level item** with `inline_code` and [a link](https://example.com)\n\
           - Second level has **more bold** and references \
           `some_very_long_function_name_that_might_overflow` in the description\n\
             - Third level: [deep_link.rs](https://example.com/deep) with trailing \
             text that must wrap inside the deeply indented column\n\
         - Another top level item with a long sentence that contains **emphasized text** \
         followed by `code_span` and more plain text that wraps",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Mixed content: heading, paragraph, code block, list, blockquote, table, thematic break.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kitchen_sink_markdown_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "# Kitchen Sink Test\n\n\
         This test exercises **every markdown element** in a single message to find \
         any width accounting bugs that only appear when elements interact.\n\n\
         ## Code Example\n\n\
         ```rust\n\
         fn render_element(element: &MdElement, theme: &MdTheme, buffer: &str, ui: &mut Ui) {\n\
             match element {\n\
                 MdElement::Paragraph(inlines) => render_paragraph(inlines, theme, buffer, ui),\n\
                 MdElement::CodeBlock(cb) => render_code_block(cb, theme, ui),\n\
                 _ => {}\n\
             }\n\
         }\n\
         ```\n\n\
         ## Key Points\n\n\
         - **First:** the `flush_job` function must account for available width\n\
         - **Second:** links break out of the `LayoutJob` via `ui.hyperlink_to()`\n\
         - **Third:** see [markdown_ui.rs](https://example.com) for implementation\n\n\
         > **Note:** blockquotes add inner margins that reduce available width. \
         Combined with list indentation, this can push content past the edge.\n\n\
         ---\n\n\
         | Element | Overflow Risk |\n\
         |---------|---------------|\n\
         | Paragraph | Low |\n\
         | List + Bold + Code | High |\n\
         | Blockquote + Link | Medium |\n\n\
         Final paragraph after all elements to verify width recovery.",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 220.0, 200.0],
    );
}

/// Long conversation with multiple turns, each containing complex markdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_turn_complex_conversation_no_overflow() {
    let turns = vec![
        user_msg("Can you review the overflow fixes and summarize what each one does?"),
        assistant_msg(
            "## Summary of Overflow Fixes\n\n\
             Here's what each change addresses:\n\n\
             1. **Bold text in `LayoutJob`** — Previously, bold spans were flushed as \
             separate `ui.label(RichText::strong())` calls. Now they stay inline using \
             the named `bold` font family from [markdown_ui.rs](https://example.com/md).\n\n\
             2. **Paragraph width constraint** — Paragraphs are now wrapped in \
             `allocate_ui_with_layout` with `max_width = ui.available_width() - 1.0`.\n\n\
             3. **Soft-wrap for long tokens** — The `soft_wrap_text()` helper inserts \
             zero-width spaces after `_`, `.`, `-`, `/`, `\\`, `:` in tokens ≥24 chars.\n\n\
             ```rust\n\
             fn soft_wrap_text(text: &str) -> Cow<'_, str> {\n\
                 // Insert \\u{200B} after separators in long tokens\n\
             }\n\
             ```\n\n\
             > **Key insight:** the root cause was `horizontal_wrapped` expanding \
             > beyond its allocation when widgets of varying sizes were added.",
        ),
        user_msg(
            "What about the user chat bubble? I noticed it clips on the right at narrow widths \
             with long text like restart_should_prefetch_newer_known_participant_relay_list_e2e",
        ),
        assistant_msg(
            "Good catch. The user bubble fix adds:\n\n\
             - **6px right inset** via `ui.add_space(6.0)` in the `right_to_left` layout\n\
             - **Inner max_width reduction** of 6px inside the `Frame::show` closure\n\n\
             This prevents pixel-rounding from pushing the rightmost glyph past the \
             container boundary. The fix is in \
             [dave.rs:1370-1385](https://example.com/dave.rs#L1370).\n\n\
             For the specific identifier you mentioned:\n\
             `restart_should_prefetch_newer_known_participant_relay_list_e2e` — this \
             gets soft-wrapped by `soft_wrap_text()` since it's >24 chars and contains \
             underscores. Each `_` becomes a potential line-break point.",
        ),
        user_msg("Can you show me the test results?"),
        assistant_msg(
            "All **25 tests pass**, including:\n\n\
             | Test | Width Range | Status |\n\
             |------|-------------|--------|\n\
             | `bold_text_in_unordered_lists` | 240-300px | ✅ |\n\
             | `ordered_list_repro_paragraph` | 198-300px | ✅ |\n\
             | `links_and_codeblocks_markdown` | 200-300px | ✅ |\n\
             | `typed_user_message_does_not_clip` | 180-320px | ✅ |\n\
             | `width_and_scale_sweep` | 181-361px × 3 DPI | ✅ |\n\n\
             The one failing test (`status_badges_do_not_overlap`) is pre-existing — \
             badges aren't in the accessibility tree at 160px.",
        ),
    ];

    assert_chat_has_no_horizontal_overflow(turns, &[400.0, 320.0, 280.0, 240.0, 200.0]);
}

/// Consecutive links without text between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consecutive_links_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "See [file_a.rs](https://example.com/a) [file_b.rs](https://example.com/b) \
         [file_c.rs](https://example.com/c) and \
         [very_long_file_name_that_might_cause_issues.rs](https://example.com/d) for details.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Bold text wrapping a long path-like identifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bold_long_path_identifier_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "The main entry point is **`crates/notedeck_dave/src/ui/markdown_ui.rs::render_assistant_message`** \
         which calls into `render_element` for each parsed `MdElement`. When the element is a \
         `Paragraph`, it delegates to `render_inlines` which builds a `LayoutJob`.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Alternating bold and links in a single paragraph — exercises the
/// flush_job/hyperlink_to/resume-job cycle repeatedly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alternating_bold_and_links_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Start with **bold text** then [link_one](https://example.com/1) then \
         **more bold** then [link_two](https://example.com/2) then **even more bold** \
         then [link_three](https://example.com/3) then **final bold segment** and \
         some trailing plain text to make sure everything still wraps.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Ordered list where each item has a link and code span — exercises list
/// indentation + flush_job interaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordered_list_with_links_and_code_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "1. Open [markdown_ui.rs](https://example.com/md) and find the `render_inlines` function\n\
         2. Check that `flush_job` sets `max_width` to `ui.available_width() - 1.0` before \
         adding the [label](https://docs.rs/egui/latest/egui/struct.Label.html)\n\
         3. Verify the `soft_wrap_text` helper in [markdown_ui.rs](https://example.com/md) \
         handles `crates/notedeck_dave/src/ui/markdown_ui.rs` correctly\n\
         4. Run `cargo test --test text_overflow_tests` and confirm all tests pass",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Code block with very long lines that exceed the viewport.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_block_with_long_lines_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Here's the problematic code:\n\n\
         ```rust\n\
         let very_long_variable_name = some_module::some_submodule::SomeStruct::new(parameter_one, parameter_two, parameter_three, parameter_four);\n\
         let another_long_line = format!(\"this is a very long format string that contains {} and {} and {} placeholders\", value_one, value_two, value_three);\n\
         ```\n\n\
         The lines above are intentionally long to test code block wrapping behavior.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Paragraph with many inline code spans back to back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_inline_code_spans_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "The function signature is `fn render_inlines(inlines: &[InlineElement], theme: &MdTheme, buffer: &str, ui: &mut Ui)` \
         and it uses `LayoutJob::default()`, `TextFormat`, `FontId::new()`, `FontFamily::Proportional`, \
         `flush_job()`, `append_wrapped_text()`, `soft_wrap_text()`, and `ui.hyperlink_to()` internally.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Italic and strikethrough mixed with bold and code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_emphasis_styles_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "This tests *italic text* mixed with **bold text** and ~~strikethrough text~~ and \
         `inline code` and ***bold italic text*** all in one paragraph. The combination of \
         different `TextFormat` styles in the same `LayoutJob` must not cause width miscalculation \
         at narrow viewports where wrapping is frequent.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Real-world Dave response pattern: explanation with file references,
/// code blocks, and a summary list.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realistic_dave_code_review_response_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "I've reviewed the changes in `crates/notedeck_dave/src/ui/markdown_ui.rs` and \
         `crates/notedeck_dave/src/ui/dave.rs`. Here's my analysis:\n\n\
         ### Width Constraint Chain\n\n\
         The overflow fix works through a chain of width constraints:\n\n\
         1. `render_chat()` sets `ui.set_max_width(content_width)` on the chat container\n\
         2. `render_element()` → `MdElement::Paragraph` wraps content in `allocate_ui_with_layout` \
         with `max_width = available_width - 1.0`\n\
         3. `flush_job()` sets `job.wrap.max_width = available_width - 1.0` before adding\n\
         4. `render_list_item()` computes `content_width = row_width - marker_width - item_spacing`\n\n\
         ### The Remaining Issue\n\n\
         The `ui.hyperlink_to()` call in `render_inlines` (line 297) does **not** go through \
         `flush_job` — it adds a widget directly. This widget's intrinsic width is the full \
         text width, which at narrow viewports can exceed `available_width`.\n\n\
         ```rust\n\
         InlineElement::Link { text, url } => {\n\
             flush_job(&mut job, ui);\n\
             ui.hyperlink_to(\n\
                 RichText::new(text.resolve(buffer)).color(theme.link_color),\n\
                 url.resolve(buffer),\n\
             );\n\
         }\n\
         ```\n\n\
         > **Recommendation:** Wrap the hyperlink in a width-constrained sub-ui or use \
         > `Label::new().wrap()` with a clickable sense instead of `hyperlink_to()`.\n\n\
         See [egui::Hyperlink](https://docs.rs/egui/latest/egui/struct.Hyperlink.html) for \
         the widget API.",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 220.0, 200.0],
    );
}

/// Thematic break between complex sections.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thematic_break_between_complex_sections_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "## Before the Break\n\n\
         Some **bold text** with `code` and [a link](https://example.com) in a paragraph.\n\n\
         ---\n\n\
         ## After the Break\n\n\
         - List item with **bold** and `code` after the thematic break\n\
         - Another item with [link](https://example.com/after) to verify recovery\n\n\
         > Final blockquote with **emphasis** to stress test width after `---`.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Very narrow viewport (sub-200px) stress test with typical Dave output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn very_narrow_viewport_stress_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "I found the bug in `render_inlines`. The **`flush_job`** call was not \
         accounting for the `inner_margin` of the parent `Frame`. Here's the fix:\n\n\
         ```rust\n\
         job.wrap.max_width = (ui.available_width() - 1.0).max(0.0);\n\
         ```\n\n\
         This ensures the wrap width never exceeds the available space.",
        &[240.0, 220.0, 200.0],
    );
}

/// Paragraph immediately after a heading (no blank line separator in rendered output).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heading_immediately_followed_by_content_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "### `render_assistant_message` Function\n\
         This function takes `elements: &[MdElement]`, `partial: Option<&Partial>`, \
         `buffer: &str`, and `ui: &mut Ui`. It iterates over elements and calls \
         `render_element` for each one. The **critical path** is the `Paragraph` variant \
         which uses `allocate_ui_with_layout` with a constrained `max_width`.",
        &[400.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Long conversation that forces vertical scrolling at a short viewport height.
/// When the vertical scrollbar appears, it eats into horizontal space — this test
/// verifies the content width properly accounts for the scrollbar.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_chat_with_vertical_scrolling_no_overflow() {
    let turns = vec![
        user_msg("Please review my overflow fixes."),
        assistant_msg(
            "## Summary of Overflow Fixes\n\n\
             Here's what each change addresses:\n\n\
             1. **Bold text in `LayoutJob`** — Previously, bold spans were flushed as \
             separate `ui.label(RichText::strong())` calls. Now they stay inline using \
             the named `bold` font family.\n\n\
             2. **Paragraph width constraint** — Paragraphs are now wrapped in \
             `allocate_ui_with_layout` with `max_width = ui.available_width() - 1.0`.\n\n\
             3. **Soft-wrap for long tokens** — The `soft_wrap_text()` helper inserts \
             zero-width spaces after `_`, `.`, `-`, `/`, `\\`, `:` in tokens ≥24 chars.",
        ),
        user_msg("What about links? I saw those overflow too."),
        assistant_msg(
            "Links are now kept inside the `LayoutJob` instead of using separate \
             `ui.hyperlink_to()` widgets. This prevents them from breaking out of the \
             `horizontal_wrapped` layout. See [markdown_ui.rs](https://example.com) \
             for the implementation.\n\n\
             The key insight is that `hyperlink_to()` adds an unconstrained widget \
             that doesn't respect the wrap width of the containing `LayoutJob`.\n\n\
             | Element | Fix | Status |\n\
             |---------|-----|--------|\n\
             | Bold | Keep in LayoutJob | Done |\n\
             | Links | Keep in LayoutJob | Done |\n\
             | Tables | Constrain columns | Done |",
        ),
        user_msg("Does it work at narrow widths?"),
        assistant_msg(
            "Yes — the tests verify overflow at widths from **200px** down to **180px**. \
             The `horizontal_overflows()` helper checks every accessibility node's \
             `raw_bounds()` against the viewport. Any node whose `x1` exceeds the \
             viewport width (plus a 4px tolerance) is flagged.\n\n\
             > **Note:** The `status_badges_do_not_overlap` test still fails at 160px \
             > because badges aren't in the accessibility tree at that width. This is \
             > a pre-existing issue unrelated to text overflow.",
        ),
    ];

    assert_chat_has_no_horizontal_overflow_with_scrolling(
        turns,
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Blockquote preceded by paragraph in the same message — test that blockquote
/// width accounting doesn't break after a paragraph.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragraph_then_blockquote_no_overflow() {
    assert_markdown_has_no_horizontal_overflow(
        "Yes — the tests verify overflow at widths from **200px** down to **180px**. \
         The `horizontal_overflows()` helper checks every accessibility node's \
         `raw_bounds()` against the viewport. Any node whose `x1` exceeds the \
         viewport width (plus a 4px tolerance) is flagged.\n\n\
         > **Note:** The `status_badges_do_not_overlap` test still fails at 160px \
         > because badges aren't in the accessibility tree at that width. This is \
         > a pre-existing issue unrelated to text overflow.",
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Long multi-line user message with list-like content and long sentences.
/// Reproduces the real-world case of a user typing a detailed bug report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_multiline_user_message_no_overflow() {
    let long_user_msg = "there is a bug where the chat messages overflow off screen\n\n\
         - Current Dave overflow tests are still not representative of the real runtime \
         container. They mount Dave directly via text_overflow_tests.rs \
         (notedeck.set_app(dave)), plus a simplified fake wrapper, not real Chrome\n\
         - Real app path uses Chrome strip/drawer layout in chrome.rs, and Dave already has \
         a workaround for strip-induced truncation in dave.rs. This integration boundary is \
         likely where the bug hides\n\
         - You cannot solve this by importing notedeck_chrome into notedeck_dave";

    let turns = vec![user_msg(long_user_msg)];
    assert_chat_has_no_horizontal_overflow(turns, &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0]);
}

/// Long user message followed by assistant response — tests that the user
/// bubble width doesn't affect subsequent assistant message layout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_user_message_then_assistant_no_overflow() {
    let turns = vec![
        user_msg(
            "analyze what should be cherry-picked from messages/improve-reliability \
             into this branch. I need the relay timeout fix and the bidirectional DM \
             test but not the outbox refactor",
        ),
        assistant_msg(
            "I'll inspect `messages/improve-reliability` commit-by-commit and map which \
             changes are directly useful for Chrome-container Dave overflow testing versus \
             unrelated risk. I'll start by comparing branch history and changed paths, \
             then I'll recommend an exact cherry-pick list.",
        ),
    ];
    assert_chat_has_no_horizontal_overflow(turns, &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0]);
}

/// Render a chat transcript through the Chrome-wrapped Dave pipeline and assert
/// no widget escapes the viewport at the given widths.
fn assert_chat_has_no_horizontal_overflow_chrome(turns: Vec<TranscriptTurn>, widths: &[f32]) {
    for width in widths {
        let harness = render_chat_harness_chrome(turns.clone(), *width);
        let overflows = horizontal_overflows(&harness, *width);
        assert!(
            overflows.is_empty(),
            "found horizontal overflow in Chrome wrapper at width {width}: {:?}",
            overflows
        );
    }
}

// ---------------------------------------------------------------------------
// Chrome-wrapped overflow tests (user messages)
// ---------------------------------------------------------------------------

/// Long user message in Chrome strip wrapper must not overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chrome_wrapped_long_user_message_no_overflow() {
    let turns = vec![user_msg(
        "there is a bug where the chat messages overflow off screen. \
         Current Dave overflow tests are still not representative of the real \
         runtime container. They mount Dave directly via text_overflow_tests.rs \
         not the real Chrome strip/drawer layout in chrome.rs",
    )];
    assert_chat_has_no_horizontal_overflow_chrome(
        turns,
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// User message with long unbroken identifiers in Chrome strip wrapper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chrome_wrapped_user_message_with_long_identifiers_no_overflow() {
    let turns = vec![user_msg(
        "please fix restart_should_prefetch_newer_known_participant_relay_list_e2e \
         and same_account_device_restart_catches_up_from_existing_db_e2e tests. \
         They are flaky because of the relay timeout issue.",
    )];
    assert_chat_has_no_horizontal_overflow_chrome(
        turns,
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Multi-turn conversation with user and assistant messages in Chrome strip wrapper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chrome_wrapped_multi_turn_conversation_no_overflow() {
    let turns = vec![
        user_msg(
            "analyze what should be cherry-picked from messages/improve-reliability \
             into this branch. I need the relay timeout fix and the bidirectional DM \
             test but not the outbox refactor",
        ),
        assistant_msg(
            "I'll inspect `messages/improve-reliability` commit-by-commit and map which \
             changes are directly useful for Chrome-container Dave overflow testing versus \
             unrelated risk.\n\n\
             | Change | Keep | Reason |\n\
             |--------|------|--------|\n\
             | Relay timeout | Yes | Needed for test stability |\n\
             | Bidirectional DM | Yes | Requested |\n\
             | Outbox refactor | No | Out of scope |",
        ),
        user_msg(
            "ok go ahead and cherry-pick those. make sure the tests still pass after \
             the cherry-pick. also check if there are any merge conflicts with the \
             current branch",
        ),
        assistant_msg(
            "Done. Both commits cherry-picked cleanly:\n\n\
             > **Note:** The relay timeout fix required a small adjustment to the \
             > `ensure_relay` timeout value in `messages_e2e.rs` to avoid a stall \
             > when the relay is slow to respond.",
        ),
    ];
    assert_chat_has_no_horizontal_overflow_chrome(
        turns,
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Short user message in Chrome strip wrapper at narrow widths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chrome_wrapped_short_user_message_narrow_no_overflow() {
    let turns = vec![
        user_msg("Are you sure these fixes actually fix the overflow issues?"),
        assistant_msg("Yes, the tests confirm no overflow at the tested widths."),
        user_msg("ok can you show me the results?"),
    ];
    assert_chat_has_no_horizontal_overflow_chrome(turns, &[320.0, 280.0, 240.0, 200.0]);
}

/// Assistant message with table and blockquote in Chrome strip wrapper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chrome_wrapped_table_and_blockquote_no_overflow() {
    let turns = vec![
        user_msg("What about links?"),
        assistant_msg(
            "Links are inline now.\n\n\
             | Element | Fix | Status |\n\
             |---------|-----|--------|\n\
             | Bold | Keep in LayoutJob | Done |\n\
             | Links | Keep in LayoutJob | Done |\n\
             | Tables | Constrain columns | Done |\n\n\
             > **Note:** All tests pass at widths from 200px to 400px.",
        ),
    ];
    assert_chat_has_no_horizontal_overflow_chrome(
        turns,
        &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0],
    );
}

/// Table followed by blockquote in a later message — the table's inter-column
/// spacing must not expand the parent layout and cause subsequent elements to
/// overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn table_then_blockquote_in_later_message_no_overflow() {
    let turns = vec![
        user_msg("What about links?"),
        assistant_msg(
            "Links are inline now.\n\n\
             | Element | Fix | Status |\n\
             |---------|-----|--------|\n\
             | Bold | Keep in LayoutJob | Done |\n\
             | Links | Keep in LayoutJob | Done |\n\
             | Tables | Constrain columns | Done |",
        ),
        user_msg("Does it work at narrow widths?"),
        assistant_msg(
            "Yes — the tests verify overflow.\n\n\
             > **Note:** The `status_badges_do_not_overlap` test still fails at 160px \
             > because badges aren't in the accessibility tree at that width. This is \
             > a pre-existing issue unrelated to text overflow.",
        ),
    ];
    assert_chat_has_no_horizontal_overflow(turns, &[400.0, 360.0, 320.0, 280.0, 240.0, 200.0]);
}

// ---------------------------------------------------------------------------
// Tool result overflow tests
// ---------------------------------------------------------------------------

/// Tool result summaries with long text must not expand the layout and cause
/// subsequent messages to overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_results_with_long_summaries_no_overflow() {
    let turns = vec![
        user_msg("take a look at what is happening in this branch compared to master. it's a work in progress. give me a status update on it."),
        tool_result("Bash", "`git log master..HEAD --oneline` (233 chars)"),
        tool_result("Bash", "`git diff master..HEAD --stat` (905 chars)"),
        tool_result("Bash", "`git diff master..HEAD -- crates/notedeck_dave/src/ui/markdown_ui.rs` (3881 chars)"),
        tool_result("Bash", "`git diff master..HEAD -- crates/notedeck_dave/tests/text_overflow_tests.rs` (9776 chars)"),
        tool_result("Bash", "`git show 7b2dbbc56 --stat --format=\"%B\"` (777 chars)"),
        assistant_msg(
            "Here's the status of this branch (`agentium/fix-text-overflow`, 4 commits ahead of master):\n\n\
             ## Goal\n\n\
             Fix text overflow/clipping issues in the Dave chat UI at narrow viewport widths.\n\n\
             ## Changes\n\n\
             - **`markdown_ui.rs`** — Table column width calculation now accounts for inter-column spacing\n\
             - **`text_overflow_tests.rs`** — Extensive E2E test coverage for overflow at various widths",
        ),
    ];
    assert_chat_has_no_horizontal_overflow(turns.clone(), &[400.0, 360.0, 320.0, 280.0, 240.0]);
    assert_chat_has_no_horizontal_overflow_chrome(turns, &[400.0, 360.0, 320.0, 280.0, 240.0]);
}

/// Reproduces the real-world 750px-wide scrollbar overflow:  when many messages
/// trigger vertical scrolling, the scrollbar eats into available width.  Without
/// capping content_width at clip_rect().width(), all content overshoots the
/// viewport by the scrollbar width.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrollbar_does_not_cause_overflow_at_desktop_width() {
    let turns = vec![
        user_msg("take a look at what is happening in this branch compared to master. it's a work in progress. give me a status update on it."),
        tool_result("Bash", "`git log master..HEAD --oneline` (233 chars)"),
        tool_result("Bash", "`git diff master..HEAD --stat` (905 chars)"),
        tool_result("Bash", "`git diff master..HEAD -- crates/notedeck_dave/src/ui/markdown_ui.rs` (3881 chars)"),
        tool_result("Bash", "`git diff master..HEAD -- crates/notedeck_dave/tests/text_overflow_tests.rs` (9776 chars)"),
        tool_result("Bash", "`git show 7b2dbbc56 --stat --format=\"%B\"` (777 chars)"),
        assistant_msg(
            "Here's the status of this branch (`agentium/fix-text-overflow`, 4 commits ahead of master):\n\n\
             ## Goal\n\n\
             Fix text overflow/clipping issues in the Dave chat UI, particularly at narrow viewport widths.\n\n\
             ## What's been done (4 commits)\n\n\
             - **`markdown_ui.rs`** — Table columns constrained, links inlined in LayoutJob\n\
             - **`dave.rs`** — Tool result summaries truncated, subagent descriptions truncated\n\
             - **`text_overflow_tests.rs`** — 56 E2E overflow tests at multiple viewport widths",
        ),
        user_msg("great, now run the full test suite to make sure everything passes"),
    ];
    // Test at desktop-class widths with short height to force scrollbar
    assert_chat_has_no_horizontal_overflow_with_scrolling(
        turns.clone(),
        &[750.0, 600.0, 500.0, 400.0],
    );
    assert_chat_has_no_horizontal_overflow_chrome_scrolling(turns, &[750.0, 600.0, 500.0, 400.0]);
}

/// Permission request buttons (Allow/Deny/Always/Exit) must not overflow
/// the viewport on narrow widths — they should wrap instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_buttons_no_overflow_on_narrow() {
    let turns = vec![
        user_msg("run the build"),
        permission(
            "Bash",
            serde_json::json!({
                "description": "Fetch Anthropic messages API docs",
                "command": "curl -s \"https://docs.anthropic.com/en/api/messages\" | head -200 2>/dev/null || echo \"curl failed\""
            }),
        ),
    ];
    assert_chat_has_no_horizontal_overflow(turns.clone(), &[400.0, 360.0, 320.0, 280.0]);
    assert_chat_has_no_horizontal_overflow_chrome(turns, &[400.0, 360.0, 320.0, 280.0]);
}

/// Tool results followed by user message must not cause user bubble overflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_results_then_user_message_no_overflow() {
    let turns = vec![
        user_msg("check the tests"),
        tool_result("Bash", "`cargo test --test text_overflow_tests` (very long output with many lines of test results that could extend past the viewport boundary)"),
        assistant_msg("All 54 tests pass."),
        user_msg("great, now let's also check if there are any overflow issues with the user messages. can you add more tests?"),
    ];
    assert_chat_has_no_horizontal_overflow(turns.clone(), &[400.0, 360.0, 320.0, 280.0, 240.0]);
    assert_chat_has_no_horizontal_overflow_chrome(turns, &[400.0, 360.0, 320.0, 280.0, 240.0]);
}
