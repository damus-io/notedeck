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
