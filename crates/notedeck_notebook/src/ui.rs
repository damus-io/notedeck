use crate::{NEW_NODE_SIZE, NodeEdit, Notebook, UiIntent};
use egui::{Color32, Pos2, Rect, Shape, Stroke, epaint::CubicBezierShape, vec2};
use jsoncanvas::{
    FileNode, GroupNode, JsonCanvas, LinkNode, Node, NodeId, TextNode,
    color::{Color, PresetColor},
    edge::{Edge, Side},
    node::GenericNode,
};
use notedeck::AppContext;
use std::collections::HashMap;
use std::ops::Neg;

/// An in-progress edge-drawing gesture, dragged from a node's side handle. The
/// payload is the same for both phases — only whether the drag is still live or
/// has just been released differs — so they share one enum.
enum Connect {
    /// Dragging from `node`'s `side` handle; the preview line runs to `pos`.
    Dragging { node: NodeId, side: Side, pos: Pos2 },
    /// The drag was released at `pos`; if it lands on another node, an edge from
    /// `node`'s `side` to that node is created.
    Released { node: NodeId, side: Side, pos: Pos2 },
}

/// The four node sides an edge can attach to. A fresh array each call since
/// `Side` is neither `Copy` nor `Clone`; the values are moved out as we iterate.
fn sides() -> [Side; 4] {
    [Side::Top, Side::Right, Side::Bottom, Side::Left]
}

/// JSON Canvas side string for `side`, used as a stable handle id and as the
/// `from`/`to` side when building an edge.
pub(crate) fn side_str(side: &Side) -> &'static str {
    match side {
        Side::Top => "top",
        Side::Right => "right",
        Side::Bottom => "bottom",
        Side::Left => "left",
    }
}

/// Visible radius of a node's connection handle, in canvas pixels.
const HANDLE_RADIUS: f32 = 5.0;
/// Click/drag target size of a connection handle (larger than it looks, so it's
/// easy to grab).
const HANDLE_HIT: f32 = 18.0;

/// Render the notebook canvas: a pannable/zoomable scene of nodes and edges,
/// with draggable, selectable, editable nodes. Selection and live-drag state are
/// written back into `notebook`; committed edits (move, text edit, create,
/// delete, connect) are returned as a single [`UiIntent`] for the caller to
/// ingest. Dragging from a node's side handle onto another node draws an edge.
pub fn notebook_ui(
    notebook: &mut Notebook,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) -> Option<UiIntent> {
    if !notebook.loaded {
        notebook.scene_rect = ui.available_rect_before_wrap();
        notebook.loaded = true;
    }

    // Effective rects for every node, accounting for drag overrides. Edges and
    // nodes both read from this so a dragged node's edges follow it.
    let rects: HashMap<NodeId, Rect> = notebook
        .canvas
        .get_nodes()
        .iter()
        .map(|(id, node)| (id.clone(), notebook.node_rect(id, node)))
        .collect();

    // Collect interactions inside the scene closure and apply them after, so the
    // closure only borrows the canvas immutably (Scene needs &mut scene_rect).
    // The edit state is moved out so the editor can mutate its buffer in place.
    let mut scene_rect = notebook.scene_rect;
    let view = notebook.scene_rect;
    let mut dragged: Option<(NodeId, Pos2)> = None;
    let mut drag_stopped: Option<NodeId> = None;
    let mut clicked: Option<NodeId> = None;
    let mut bg_clicked = false;
    let mut hovered: Option<NodeId> = None;
    // The connection gesture this frame, if any: a drag from a node's side handle
    // toward the pointer, then its release (which resolves to an edge if it lands
    // on another node).
    let mut connect: Option<Connect> = None;
    // An edge whose delete handle was clicked this frame, removed after the closure.
    let mut disconnect: Option<UiIntent> = None;
    let mut start_edit: Option<NodeId> = None;
    // A text node whose task-list checkbox was toggled this frame, with the
    // node's text already rewritten; committed as an `EditText` after the scene.
    let mut checkbox_edit: Option<(NodeId, String)> = None;
    let mut create_at: Option<Pos2> = None;
    let mut commit_edit = false;
    let mut cancel_edit = false;
    let mut edit = std::mem::replace(&mut notebook.edit, NodeEdit::Idle);
    let canvas = &notebook.canvas;
    let selected = notebook.selected.as_ref();
    let connecting = notebook.connecting.clone();

    egui::Scene::new().show(ui, &mut scene_rect, |ui| {
        // Background handle first (underneath the nodes) covering the visible
        // region, so a click on empty canvas clears the selection.
        let bg = ui.interact(view, ui.id().with("notebook_bg"), egui::Sense::click());
        if bg.clicked() {
            bg_clicked = true;
        }
        // Double-clicking empty canvas drops a fresh text node there to edit.
        if bg.double_clicked() {
            create_at = bg.interact_pointer_pos();
        }

        // Edges next, then nodes on top so node drag handles win interaction.
        // Clicking an edge's midpoint delete handle removes it.
        for (_edge_id, edge) in canvas.get_edges().iter() {
            if let Some(removed) = edge_ui(ui, &rects, edge) {
                disconnect = Some(removed);
            }
        }

        // The id of the node being edited (existing-node editor), if any.
        let editing_id = match &edit {
            NodeEdit::Editing { node, .. } => Some(node.clone()),
            _ => None,
        };

        for (id, node) in canvas.get_nodes().iter() {
            let rect = rects[id];

            // The node being edited renders an inline text field instead of its
            // usual contents; everything else renders normally and can enter
            // edit mode on a double-click.
            if editing_id.as_ref() == Some(id) {
                let NodeEdit::Editing {
                    buffer,
                    request_focus,
                    ..
                } = &mut edit
                else {
                    unreachable!()
                };
                let resp =
                    text_edit_node_ui(ui, node.node().color.as_ref(), rect, buffer, *request_focus);
                *request_focus = false;
                if resp.lost_focus() {
                    // Esc abandons the edit; any other blur commits it.
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel_edit = true;
                    } else {
                        commit_edit = true;
                    }
                }
                continue;
            }

            let egui::InnerResponse {
                response: resp,
                inner: toggled_text,
            } = node_ui(ui, ctx, node, rect, selected == Some(id));
            if let Some(text) = toggled_text {
                checkbox_edit = Some((id.clone(), text));
            }
            if resp.hovered() {
                hovered = Some(id.clone());
            }
            if resp.dragged() {
                dragged = Some((id.clone(), rect.min + resp.drag_delta()));
            }
            // On release, commit the move (its final position is the override
            // recorded by the last drag frame, read after the closure).
            if resp.drag_stopped() {
                drag_stopped = Some(id.clone());
            }
            if resp.clicked() {
                clicked = Some(id.clone());
            }
            if resp.double_clicked() && matches!(node, Node::Text(_)) {
                start_edit = Some(id.clone());
            }
        }

        // A brand-new node being composed renders its editor at its position; it
        // isn't in the canvas yet (it's created only when the edit commits).
        if let NodeEdit::Creating {
            pos,
            buffer,
            request_focus,
        } = &mut edit
        {
            let rect = Rect::from_min_size(*pos, NEW_NODE_SIZE);
            let resp = text_edit_node_ui(ui, None, rect, buffer, *request_focus);
            *request_focus = false;
            if resp.lost_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel_edit = true;
                } else {
                    commit_edit = true;
                }
            }
        }

        // Connection handles: small dots on the sides of the active node(s) that
        // start an edge when dragged. Shown for the selected, hovered and
        // currently-connecting node so they don't clutter the whole canvas — the
        // connecting node is kept in the set so its handle survives the pointer
        // leaving it mid-drag. The gesture is collected into `connect` and
        // resolved (into an edge) after the closure.
        let candidates = [selected, hovered.as_ref(), connecting.as_ref()];
        for i in 0..candidates.len() {
            let Some(nid) = candidates[i] else { continue };
            // Skip nodes off-canvas or already handled (selected == hovered, etc).
            if !rects.contains_key(nid) || candidates[..i].iter().flatten().any(|c| *c == nid) {
                continue;
            }
            let rect = rects[nid];
            for side in sides() {
                let center = side_point(&side, rect);
                let hit = Rect::from_center_size(center, vec2(HANDLE_HIT, HANDLE_HIT));
                let resp = ui.interact(
                    hit,
                    ui.id()
                        .with(("notebook_handle", nid.as_str(), side_str(&side))),
                    egui::Sense::click_and_drag(),
                );
                connection_handle_ui(ui, center, resp.hovered() || resp.dragged());
                let pos = resp.interact_pointer_pos();
                if resp.drag_stopped() {
                    connect = Some(Connect::Released {
                        node: nid.clone(),
                        side,
                        pos: pos.unwrap_or(center),
                    });
                } else if resp.dragged()
                    && let Some(pos) = pos
                {
                    connect = Some(Connect::Dragging {
                        node: nid.clone(),
                        side,
                        pos,
                    });
                }
            }
        }

        // Preview an in-progress connection: a line from the source handle to the
        // pointer, and a highlight on the node it would land on.
        if let Some(Connect::Dragging { node, side, pos }) = &connect
            && let Some(from_rect) = rects.get(node)
        {
            connection_preview_ui(ui, side_point(side, *from_rect), *pos);
            if let Some(target) = node_at(&rects, *pos, node) {
                ui.painter().rect_stroke(
                    rects[target],
                    egui::CornerRadius::same(notedeck::tokens::RADIUS_LG as u8),
                    egui::Stroke::new(
                        notedeck::tokens::STROKE_THICK * 2.0,
                        ui.visuals().selection.stroke.color,
                    ),
                    egui::StrokeKind::Inside,
                );
            }
        }
    });

    notebook.scene_rect = scene_rect;
    // Keep the connecting node's handles alive while a drag is live; clear it
    // otherwise. The completed gesture is turned into an edge intent below.
    notebook.connecting = match &connect {
        Some(Connect::Dragging { node, .. }) => Some(node.clone()),
        _ => None,
    };

    if let Some((id, pos)) = dragged {
        notebook.positions.insert(id, pos);
    }
    if let Some(id) = clicked {
        notebook.selected = Some(id);
    } else if bg_clicked {
        notebook.selected = None;
    }

    // A finished drag commits a move to the node's last recorded override.
    let mut intent = drag_stopped.and_then(|id| {
        notebook.positions.get(&id).map(|pos| UiIntent::Move {
            node: id,
            pos: *pos,
        })
    });

    // A clicked edge delete handle removes that edge. Takes precedence over a
    // move (the two gestures can't realistically coincide).
    if disconnect.is_some() {
        intent = disconnect;
    }

    // A released connection that landed on another node becomes a new edge,
    // anchored from the dragged side to whichever side of the target it faces.
    if let Some(Connect::Released { node, side, pos }) = connect
        && let Some(target) = node_at(&rects, pos, &node)
    {
        let to_side = nearest_side(rects[target], pos);
        intent = Some(UiIntent::Connect {
            from: node,
            from_side: side,
            to: target.clone(),
            to_side,
        });
    }

    // Resolve the edit transition. Commit/cancel close the current editor; a
    // fresh double-click then opens the next one. A commit turns into an edit (or
    // a delete if blanked) for an existing node, or a create for a new one;
    // blank creates and Esc are discarded so stray double-clicks leave no trace.
    if cancel_edit {
        edit = NodeEdit::Idle;
    } else if commit_edit {
        match &edit {
            NodeEdit::Editing { node, buffer, .. } => {
                intent = Some(if buffer.trim().is_empty() {
                    UiIntent::Delete { node: node.clone() }
                } else {
                    UiIntent::EditText {
                        node: node.clone(),
                        text: buffer.clone(),
                    }
                });
            }
            NodeEdit::Creating { pos, buffer, .. } => {
                if !buffer.trim().is_empty() {
                    intent = Some(UiIntent::Create {
                        pos: *pos,
                        text: buffer.clone(),
                    });
                }
            }
            NodeEdit::Idle => {}
        }
        edit = NodeEdit::Idle;
    }

    // A task-list checkbox clicked in a rendered text node persists its flipped
    // text like any other edit. It can't coincide with a drag move or an inline
    // editor commit (those need the body or the open editor, not the checkbox).
    if let Some((node, text)) = checkbox_edit {
        intent = Some(UiIntent::EditText { node, text });
    }

    if let Some(id) = start_edit {
        let buffer = text_node_text(&notebook.canvas, &id);
        edit = NodeEdit::Editing {
            node: id,
            buffer,
            request_focus: true,
        };
    } else if let Some(pos) = create_at {
        edit = NodeEdit::Creating {
            pos,
            buffer: String::new(),
            request_focus: true,
        };
    }
    notebook.edit = edit;

    intent
}

/// Render the inline editor for a text node: a multiline field filling the
/// node's rect, with a selection-colored border. Returns the text field's
/// response so the caller can detect blur. Grabs keyboard focus once when
/// `request_focus`. `accent` tints the fill (the node's color, if any).
fn text_edit_node_ui(
    ui: &mut egui::Ui,
    accent: Option<&Color>,
    rect: Rect,
    buffer: &mut String,
    request_focus: bool,
) -> egui::Response {
    let base_fill = ui.visuals().extreme_bg_color;
    let accent = accent
        .map(canvas_color)
        .unwrap_or_else(|| ui.visuals().selection.stroke.color);
    let fill = blend(base_fill, accent, 0.12);

    let mut text_resp = None;
    ui.put(rect, |ui: &mut egui::Ui| {
        egui::Frame::default()
            .fill(fill)
            .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_LG as i8))
            .corner_radius(egui::CornerRadius::same(notedeck::tokens::RADIUS_LG as u8))
            .stroke(egui::Stroke::new(
                notedeck::tokens::STROKE_THICK * 2.0,
                ui.visuals().selection.stroke.color,
            ))
            .show(ui, |ui| {
                let resp = ui.add_sized(
                    ui.available_size(),
                    egui::TextEdit::multiline(buffer).frame(false),
                );
                if request_focus {
                    resp.request_focus();
                }
                text_resp = Some(resp);
            })
            .response
    });
    text_resp.expect("frame body always runs")
}

/// The text of a text node, or empty if it isn't one / doesn't exist.
fn text_node_text(canvas: &JsonCanvas, id: &NodeId) -> String {
    match canvas.get_nodes().get(id) {
        Some(Node::Text(node)) => node.text().to_string(),
        _ => String::new(),
    }
}

/// Resolve a JSONCanvas color (preset palette index or hex) to an egui color.
///
/// Preset values follow Obsidian's canvas palette ordering.
fn canvas_color(color: &Color) -> Color32 {
    match color {
        Color::Preset(preset) => match preset {
            PresetColor::Red => Color32::from_rgb(0xE0, 0x31, 0x31),
            PresetColor::Orange => Color32::from_rgb(0xE6, 0x77, 0x00),
            PresetColor::Yellow => Color32::from_rgb(0xE0, 0xAC, 0x00),
            PresetColor::Green => Color32::from_rgb(0x2C, 0xA0, 0x2C),
            PresetColor::Cyan => Color32::from_rgb(0x00, 0xA0, 0xBE),
            PresetColor::Purple => Color32::from_rgb(0x96, 0x50, 0xC8),
        },
        Color::Color(hex) => Color32::from_rgb(hex.r, hex.g, hex.b),
    }
}

/// Linear blend from `base` toward `accent` by `t` (0..=1).
fn blend(base: Color32, accent: Color32, t: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        lerp(base.r(), accent.r()),
        lerp(base.g(), accent.g()),
        lerp(base.b(), accent.b()),
    )
}

/// The node's rect at its canvas-declared position. Callers that move nodes
/// around (dragging) substitute their own position for `rect.min`.
pub fn node_rect(node: &GenericNode) -> Rect {
    let x = node.x as f32;
    let y = node.y as f32;
    let width = node.width as f32;
    let height = node.height as f32;

    let min = Pos2::new(x, y);
    let max = Pos2::new(x + width, y + height);

    Rect::from_min_max(min, max)
}

fn side_point(side: &Side, rect: Rect) -> Pos2 {
    match side {
        Side::Top => rect.center_top(),
        Side::Left => rect.left_center(),
        Side::Right => rect.right_center(),
        Side::Bottom => rect.center_bottom(),
    }
}

/// a unit vector pointing outward from the given side
fn side_tangent(side: &Side) -> egui::Vec2 {
    match side {
        Side::Top => vec2(0.0, -1.0),
        Side::Bottom => vec2(0.0, 1.0),
        Side::Left => vec2(-1.0, 0.0),
        Side::Right => vec2(1.0, 0.0),
    }
}

/// The topmost node whose rect contains `pos`, other than `exclude` — the node a
/// connection drag would attach to on release. Iteration order is arbitrary, so
/// overlapping nodes resolve to an unspecified one; good enough for picking a
/// drop target.
fn node_at<'a>(
    rects: &'a HashMap<NodeId, Rect>,
    pos: Pos2,
    exclude: &NodeId,
) -> Option<&'a NodeId> {
    rects
        .iter()
        .find(|(id, rect)| *id != exclude && rect.contains(pos))
        .map(|(id, _)| id)
}

/// The side of `rect` that `pos` most faces, so an incoming edge anchors on the
/// edge nearest the source. Compares horizontal vs. vertical offset scaled by the
/// opposite dimension, so a wide box still prefers top/bottom when approached
/// from above or below.
fn nearest_side(rect: Rect, pos: Pos2) -> Side {
    let d = pos - rect.center();
    if d.x.abs() * rect.height() >= d.y.abs() * rect.width() {
        if d.x >= 0.0 { Side::Right } else { Side::Left }
    } else if d.y >= 0.0 {
        Side::Bottom
    } else {
        Side::Top
    }
}

/// Draw a node's connection handle: a small dot, accented and enlarged when
/// active (hovered or being dragged) so it reads as grabbable.
fn connection_handle_ui(ui: &egui::Ui, center: Pos2, active: bool) {
    let color = if active {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    let radius = if active {
        HANDLE_RADIUS + 1.5
    } else {
        HANDLE_RADIUS
    };
    let painter = ui.painter();
    painter.circle_filled(center, radius, color);
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.0, ui.visuals().extreme_bg_color),
    );
}

/// Draw the in-progress connection: a line from the source handle to the pointer
/// with a dot marking where the edge would land.
fn connection_preview_ui(ui: &egui::Ui, from: Pos2, to: Pos2) {
    let color = ui.visuals().selection.stroke.color;
    let painter = ui.painter();
    painter.line_segment([from, to], Stroke::new(4.0, color));
    painter.circle_filled(to, 4.0, color);
}

/// Render one edge as a bezier with an arrow, plus a small midpoint handle that
/// deletes the edge when clicked. Returns a [`UiIntent::DisconnectEdge`] on the
/// frame the handle is clicked.
pub fn edge_ui(ui: &mut egui::Ui, rects: &HashMap<NodeId, Rect>, edge: &Edge) -> Option<UiIntent> {
    let from_rect = *rects.get(edge.from_node())?;
    let to_rect = *rects.get(edge.to_node())?;
    let to_side = edge.to_side()?;
    let from_side = edge.from_side()?;

    // anchor from-side
    let p0 = side_point(from_side, from_rect);

    // anchor b
    let to_anchor = side_point(to_side, to_rect);

    // to-point is slightly offset to accomidate arrow
    let p3 = to_anchor + side_tangent(to_side) * 2.0;

    // bend debug
    //let bend = debug_slider(ui, ui.id().with("bend"), p3, 0.25, 0.0..=1.0);
    let bend = 0.28;

    // How far to pull the tangents.
    // ¼ of the distance between anchors feels very “Obsidian”.
    let d = (p3 - p0).length() * bend;

    // c1 = anchor A + (outward tangent) * d
    let c1 = p0 + side_tangent(from_side) * d;

    // c2 = anchor B + (inward tangent)  * d
    let c2 = p3 - side_tangent(to_side).neg() * d;

    let color = edge
        .color()
        .map(canvas_color)
        .unwrap_or_else(|| ui.visuals().noninteractive().bg_stroke.color);
    let stroke = egui::Stroke::new(4.0, color);
    let bezier = CubicBezierShape::from_points_stroke([p0, c1, c2, p3], false, color, stroke);

    // The curve midpoint, captured before the shape is moved into the painter.
    let mid = bezier.sample(0.5);
    ui.painter().add(Shape::CubicBezier(bezier));
    arrow_ui(ui, to_side, to_anchor, color);

    // Midpoint delete handle: a subtle dot that turns into a red ✕ on hover and
    // removes the edge when clicked.
    let hit = Rect::from_center_size(mid, vec2(HANDLE_HIT, HANDLE_HIT));
    let resp = ui.interact(
        hit,
        ui.id().with(("notebook_edge_del", edge.id().as_str())),
        egui::Sense::click(),
    );
    edge_delete_handle_ui(ui, mid, resp.hovered());
    if resp.clicked() {
        return Some(UiIntent::DisconnectEdge {
            edge_id: edge.id().to_string(),
            from: edge.from_node().clone(),
            to: edge.to_node().clone(),
        });
    }

    None
}

/// Draw an edge's midpoint delete handle: a faint dot at rest, a filled red
/// circle with a white ✕ when hovered (signalling a click removes the edge).
fn edge_delete_handle_ui(ui: &egui::Ui, center: Pos2, active: bool) {
    let painter = ui.painter();
    if active {
        let radius = 8.0;
        painter.circle_filled(center, radius, Color32::from_rgb(0xE0, 0x31, 0x31));
        let d = radius * 0.45;
        let cross = Stroke::new(2.0, Color32::WHITE);
        painter.line_segment([center + vec2(-d, -d), center + vec2(d, d)], cross);
        painter.line_segment([center + vec2(-d, d), center + vec2(d, -d)], cross);
    } else {
        painter.circle_filled(center, 3.0, ui.visuals().widgets.inactive.fg_stroke.color);
        painter.circle_stroke(center, 3.0, Stroke::new(1.0, ui.visuals().extreme_bg_color));
    }
}

/// Paint a tiny triangular “arrow”.
///
/// * `ui`    – the egui `Ui` you’re painting in
/// * `side`  – which edge of the box we’re attaching to
/// * `point` – the exact spot on that edge the arrow’s tip should touch
/// * `fill`  – colour to fill the arrow with (usually your popup’s background)
pub fn arrow_ui(ui: &mut egui::Ui, side: &Side, point: Pos2, fill: egui::Color32) {
    let len: f32 = 12.0; // distance from tip to base
    let width: f32 = 16.0; // length of the base
    let stroke: f32 = 1.0; // length of the base

    let verts = match side {
        Side::Top => [
            point,                                           // tip
            Pos2::new(point.x - width * 0.5, point.y - len), // base‑left (above)
            Pos2::new(point.x + width * 0.5, point.y - len), // base‑right (above)
        ],
        Side::Bottom => [
            point,
            Pos2::new(point.x + width * 0.5, point.y + len), // below
            Pos2::new(point.x - width * 0.5, point.y + len),
        ],
        Side::Left => [
            point,
            Pos2::new(point.x - len, point.y + width * 0.5), // left
            Pos2::new(point.x - len, point.y - width * 0.5),
        ],
        Side::Right => [
            point,
            Pos2::new(point.x + len, point.y - width * 0.5), // right
            Pos2::new(point.x + len, point.y + width * 0.5),
        ],
    };

    ui.painter().add(egui::Shape::convex_polygon(
        verts.to_vec(),
        fill,
        Stroke::new(stroke, fill), // add a stroke here if you want an outline
    ));
}

/// Render `node` at `rect`, returning its whole-node drag/select handle as the
/// [`egui::InnerResponse::response`]. The `inner` is `None` for most node kinds;
/// for a text node whose GFM task-list checkbox was clicked this frame it carries
/// the node text with that box flipped, for the caller to persist.
pub fn node_ui(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    node: &Node,
    rect: Rect,
    selected: bool,
) -> egui::InnerResponse<Option<String>> {
    match node {
        Node::Text(text_node) => text_node_ui(ui, ctx, text_node, rect, selected),
        Node::File(file_node) => {
            egui::InnerResponse::new(None, file_node_ui(ui, file_node, rect, selected))
        }
        Node::Link(link_node) => {
            egui::InnerResponse::new(None, link_node_ui(ui, link_node, rect, selected))
        }
        Node::Group(group_node) => {
            egui::InnerResponse::new(None, group_node_ui(ui, group_node, rect, selected))
        }
    }
}

fn text_node_ui(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    node: &TextNode,
    rect: Rect,
    selected: bool,
) -> egui::InnerResponse<Option<String>> {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        node_text_ui(ui, ctx, node.text())
    })
}

/// Render a text node's body: markdown, with any inline `nostr:` references
/// resolved to their kind renderer and GFM task-list checkboxes made clickable
/// (see [`notedeck_ui::markdown::render_markdown_with_refs_editable`]). Returns
/// the node's text with the checkbox flipped if one was toggled this frame.
fn node_text_ui(ui: &mut egui::Ui, ctx: &mut AppContext, text: &str) -> Option<String> {
    let mut source = text.to_string();
    notedeck_ui::markdown::render_markdown_with_refs_editable(ui, ctx, &mut source)
        .then_some(source)
}

fn file_node_ui(ui: &mut egui::Ui, node: &FileNode, rect: Rect, selected: bool) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        ui.label("file node");
    })
    .response
}

fn link_node_ui(ui: &mut egui::Ui, node: &LinkNode, rect: Rect, selected: bool) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        ui.label("link node");
    })
    .response
}

fn group_node_ui(
    ui: &mut egui::Ui,
    node: &GroupNode,
    rect: Rect,
    selected: bool,
) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        ui.label("group node");
    })
    .response
}

/// Render a node's frame and contents at `rect`. The [`egui::InnerResponse`]'s
/// `response` is a click-and-drag handle covering the whole node (so the caller
/// can move/select it) and its `inner` is whatever the `contents` closure
/// produced.
///
/// The drag/select handle is registered *before* the content so that any
/// interactive content — e.g. a task-list checkbox — sits on top and wins
/// clicks, while non-interactive content (labels, which only sense hover) lets
/// clicks fall through to the handle so dragging the body still moves the node.
fn node_box_ui<R>(
    ui: &mut egui::Ui,
    node: &GenericNode,
    rect: Rect,
    selected: bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    // Colored nodes get an accent border and a faint accent-tinted fill; plain
    // nodes fall back to the neutral theme colors. Selected nodes get a
    // brighter, thicker border.
    let base_fill = ui.visuals().noninteractive().weak_bg_fill;
    let base_stroke = ui.visuals().noninteractive().bg_stroke.color;
    let (fill, accent) = match node.color.as_ref().map(canvas_color) {
        Some(accent) => (blend(base_fill, accent, 0.12), accent),
        None => (base_fill, base_stroke),
    };
    let (stroke_width, stroke_color) = if selected {
        (
            notedeck::tokens::STROKE_THICK * 2.0,
            ui.visuals().selection.stroke.color,
        )
    } else {
        (notedeck::tokens::STROKE_THICK, accent)
    };

    // Handle first (underneath); see the doc comment for why ordering matters.
    let resp = ui.interact(
        rect,
        ui.id().with(("notebook_node", node.id.as_str())),
        egui::Sense::click_and_drag(),
    );

    let mut out = None;
    ui.put(rect, |ui: &mut egui::Ui| {
        egui::Frame::default()
            .fill(fill)
            .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_LG as i8))
            .corner_radius(egui::CornerRadius::same(notedeck::tokens::RADIUS_LG as u8))
            .stroke(egui::Stroke::new(stroke_width, stroke_color))
            .show(ui, |ui| {
                let inner = ui.available_rect_before_wrap();
                ui.allocate_at_least(ui.available_size(), egui::Sense::hover());
                ui.put(inner, |ui: &mut egui::Ui| {
                    out = Some(contents(ui));
                    ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
                });
            })
            .response
    });

    egui::InnerResponse::new(out.expect("frame body always runs"), resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    use jsoncanvas::TextNode;
    use std::cell::RefCell;

    /// A node's body lays a full-rect `click_and_drag` handle over its contents
    /// (so dragging the body moves the node). A task-list checkbox rendered in
    /// that body must still receive a real pointer click rather than have the
    /// handle swallow it — the bug behind "checkboxes aren't clickable in the
    /// notebook". [`node_box_ui`] registers the handle *under* the content to
    /// guarantee this; with the old ordering (handle last/on top) the source
    /// below would stay `- [ ]`.
    ///
    /// `simulate_click()` is essential: it sends a geometric pointer press at the
    /// box, exercising egui's hit-testing. `.click()` (an accesskit action aimed
    /// straight at the node) would bypass the z-order entirely and pass even when
    /// the real app is broken.
    #[test]
    fn task_checkbox_in_text_node_toggles_despite_drag_handle() {
        let node = TextNode::new("node1".parse().unwrap(), 0, 0, 220, 90, None, String::new());
        let source = RefCell::new(String::from("- [ ] task\n"));

        let mut harness = Harness::new_ui(|ui| {
            let rect = Rect::from_min_size(ui.max_rect().min, vec2(220.0, 90.0));
            let mut s = source.borrow_mut();
            // Mirrors node_text_ui's editable render, minus the nostr-ref pass
            // (which would need an AppContext); the z-order under test is the same.
            node_box_ui(ui, node.node(), rect, false, |ui| {
                notedeck_ui::markdown::render_markdown_editable(&mut s, ui)
            });
        });
        harness.run();

        harness.get_by_role(Role::CheckBox).simulate_click();
        harness.run();

        assert_eq!(
            *source.borrow(),
            "- [x] task\n",
            "the node drag handle swallowed the checkbox click"
        );
    }
}
