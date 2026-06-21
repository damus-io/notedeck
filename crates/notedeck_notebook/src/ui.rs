use crate::{NodeEdit, Notebook};
use egui::{Color32, Pos2, Rect, Shape, Stroke, epaint::CubicBezierShape, vec2};
use enostr::NoteId;
use jsoncanvas::{
    FileNode, GroupNode, JsonCanvas, LinkNode, Node, NodeId, TextNode,
    color::{Color, PresetColor},
    edge::{Edge, Side},
    node::GenericNode,
};
use nostrdb::{Filter, Note, Transaction};
use notedeck::AppContext;
use std::collections::HashMap;
use std::ops::Neg;

/// Render the notebook canvas: a pannable/zoomable scene of nodes and edges,
/// with draggable, selectable nodes. Position and selection changes are written
/// back into `notebook`.
pub fn notebook_ui(notebook: &mut Notebook, ctx: &AppContext, ui: &mut egui::Ui) {
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
    let mut clicked: Option<NodeId> = None;
    let mut bg_clicked = false;
    let mut start_edit: Option<NodeId> = None;
    let mut create_at: Option<Pos2> = None;
    let mut commit_edit = false;
    let mut cancel_edit = false;
    let mut edit = std::mem::replace(&mut notebook.edit, NodeEdit::Idle);
    let canvas = &notebook.canvas;
    let selected = notebook.selected.as_ref();

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
        for (_edge_id, edge) in canvas.get_edges().iter() {
            edge_ui(ui, &rects, edge);
        }

        for (id, node) in canvas.get_nodes().iter() {
            let rect = rects[id];

            // The node being edited renders an inline text field instead of its
            // usual contents; everything else renders normally and can enter
            // edit mode on a double-click.
            let editing_this = matches!(&edit, NodeEdit::Editing { node, .. } if node == id);
            if editing_this {
                let NodeEdit::Editing {
                    buffer,
                    request_focus,
                    ..
                } = &mut edit
                else {
                    unreachable!()
                };
                let resp = text_edit_node_ui(ui, node.node(), rect, buffer, *request_focus);
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

            let resp = node_ui(ui, ctx, node, rect, selected == Some(id));
            if resp.dragged() {
                dragged = Some((id.clone(), rect.min + resp.drag_delta()));
            }
            if resp.clicked() {
                clicked = Some(id.clone());
            }
            if resp.double_clicked() && matches!(node, Node::Text(_)) {
                start_edit = Some(id.clone());
            }
        }
    });

    notebook.scene_rect = scene_rect;

    if let Some((id, pos)) = dragged {
        notebook.positions.insert(id, pos);
    }
    if let Some(id) = clicked {
        notebook.selected = Some(id);
    } else if bg_clicked {
        notebook.selected = None;
    }

    // Apply edit transitions. Commit/cancel resolve the current edit; a fresh
    // double-click then opens the next one (committing the previous first, since
    // moving focus blurs it). Edits that end up blank drop their node so stray
    // double-clicks don't litter the canvas with empty boxes.
    if cancel_edit {
        if let NodeEdit::Editing { node, .. } = &edit
            && text_node_text(&notebook.canvas, node).trim().is_empty()
        {
            remove_node(notebook, node);
        }
        edit = NodeEdit::Idle;
    } else if commit_edit {
        if let NodeEdit::Editing { node, buffer, .. } = &edit {
            if buffer.trim().is_empty() {
                remove_node(notebook, node);
            } else {
                set_text_node_text(&mut notebook.canvas, node, buffer.clone());
            }
        }
        edit = NodeEdit::Idle;
    }
    if let Some(id) = start_edit {
        let buffer = text_node_text(&notebook.canvas, &id);
        edit = NodeEdit::Editing {
            node: id,
            buffer,
            request_focus: true,
        };
    } else if let Some(pos) = create_at {
        let id = new_text_node(&mut notebook.canvas, pos);
        notebook.selected = Some(id.clone());
        edit = NodeEdit::Editing {
            node: id,
            buffer: String::new(),
            request_focus: true,
        };
    }
    notebook.edit = edit;
}

/// Render the inline editor for a text node: a multiline field filling the node's
/// rect, with a selection-colored border. Returns the text field's response so
/// the caller can detect blur. Grabs keyboard focus once when `request_focus`.
fn text_edit_node_ui(
    ui: &mut egui::Ui,
    node: &GenericNode,
    rect: Rect,
    buffer: &mut String,
    request_focus: bool,
) -> egui::Response {
    let base_fill = ui.visuals().extreme_bg_color;
    let accent = node
        .color
        .as_ref()
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

/// Add a new, empty text node at `pos` (canvas coords) and return its id.
fn new_text_node(canvas: &mut JsonCanvas, pos: Pos2) -> NodeId {
    let id = unique_node_id(canvas);
    let node = TextNode::new(
        id.clone(),
        pos.x as i64,
        pos.y as i64,
        250,
        120,
        None,
        String::new(),
    );
    let _ = canvas.add_node(node.into());
    id
}

/// A `notebook-N` id not already present in the canvas.
fn unique_node_id(canvas: &JsonCanvas) -> NodeId {
    let mut n = 0u64;
    loop {
        let candidate = format!("notebook-{n}");
        if !canvas.get_nodes().keys().any(|k| k.as_str() == candidate) {
            return candidate.parse().expect("non-empty id");
        }
        n += 1;
    }
}

/// Drop a node from the canvas along with any drag override / selection on it.
fn remove_node(notebook: &mut Notebook, id: &NodeId) {
    notebook.canvas.get_mut_nodes().remove(id);
    notebook.positions.remove(id);
    if notebook.selected.as_ref() == Some(id) {
        notebook.selected = None;
    }
}

/// The text of a text node, or empty if it isn't one / doesn't exist.
fn text_node_text(canvas: &JsonCanvas, id: &NodeId) -> String {
    match canvas.get_nodes().get(id) {
        Some(Node::Text(node)) => node.text().to_string(),
        _ => String::new(),
    }
}

/// Replace a text node's text in place, preserving its geometry and color.
/// `TextNode` exposes no text setter, so we rebuild the node and swap it in.
fn set_text_node_text(canvas: &mut JsonCanvas, id: &NodeId, text: String) {
    let geom = {
        let Some(Node::Text(node)) = canvas.get_nodes().get(id) else {
            return;
        };
        let g = node.node();
        (
            g.x,
            g.y,
            g.width,
            g.height,
            g.color.as_ref().map(clone_color),
        )
    };
    let (x, y, width, height, color) = geom;
    let node = TextNode::new(id.clone(), x, y, width, height, color, text);
    canvas.get_mut_nodes().insert(id.clone(), node.into());
}

/// `jsoncanvas::Color` isn't `Clone`, so rebuild it by hand.
fn clone_color(color: &Color) -> Color {
    match color {
        Color::Preset(preset) => Color::Preset(match preset {
            PresetColor::Red => PresetColor::Red,
            PresetColor::Orange => PresetColor::Orange,
            PresetColor::Yellow => PresetColor::Yellow,
            PresetColor::Green => PresetColor::Green,
            PresetColor::Cyan => PresetColor::Cyan,
            PresetColor::Purple => PresetColor::Purple,
        }),
        Color::Color(hex) => Color::Color(*hex),
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

pub fn edge_ui(
    ui: &mut egui::Ui,
    rects: &HashMap<NodeId, Rect>,
    edge: &Edge,
) -> Option<egui::Response> {
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

    ui.painter().add(Shape::CubicBezier(bezier));
    arrow_ui(ui, to_side, to_anchor, color);

    None
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

pub fn node_ui(
    ui: &mut egui::Ui,
    ctx: &AppContext,
    node: &Node,
    rect: Rect,
    selected: bool,
) -> egui::Response {
    match node {
        Node::Text(text_node) => text_node_ui(ui, ctx, text_node, rect, selected),
        Node::File(file_node) => file_node_ui(ui, file_node, rect, selected),
        Node::Link(link_node) => link_node_ui(ui, link_node, rect, selected),
        Node::Group(group_node) => group_node_ui(ui, group_node, rect, selected),
    }
}

fn text_node_ui(
    ui: &mut egui::Ui,
    ctx: &AppContext,
    node: &TextNode,
    rect: Rect,
    selected: bool,
) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        node_text_ui(ui, ctx, node.text());
    })
}

/// Render a text node's body, splicing in inline widgets for any `nostr:`
/// references. Plain text outside references is rendered as markdown, so a note
/// reads the same as before unless it actually links to a nostr entity. Scans in
/// place — no per-frame allocation for the common reference-free case.
fn node_text_ui(ui: &mut egui::Ui, ctx: &AppContext, text: &str) {
    let mut rest = text;
    while let Some(pos) = rest.find("nostr:") {
        let after = &rest[pos + "nostr:".len()..];
        // The bech32 token is a run of lowercase letters/digits (hrp + data).
        let end = after
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
            .unwrap_or(after.len());
        if end == 0 {
            // A bare "nostr:" with no entity after it: keep it as text.
            let upto = pos + "nostr:".len();
            notedeck_ui::markdown::render_markdown(&rest[..upto], ui);
            rest = &rest[upto..];
            continue;
        }
        if pos > 0 {
            notedeck_ui::markdown::render_markdown(&rest[..pos], ui);
        }
        nostr_ref_ui(ui, ctx, &after[..end]);
        rest = &after[end..];
    }
    if !rest.is_empty() {
        notedeck_ui::markdown::render_markdown(rest, ui);
    }
}

/// Resolve a `nostr:` reference to a note and hand it to the registered
/// renderer for its kind. Falls back to plain link text when the entity can't be
/// parsed, isn't in the db yet, or has no renderer.
fn nostr_ref_ui(ui: &mut egui::Ui, ctx: &AppContext, bech: &str) {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        nostr_ref_fallback_ui(ui, bech);
        return;
    };
    let Some(note) = resolve_nostr_ref(ctx.ndb, &txn, bech) else {
        nostr_ref_fallback_ui(ui, bech);
        return;
    };
    // TODO: per-kind default renderer id from settings (see "Settings UI" card).
    match ctx.kind_renderers.default_for(note.kind(), None) {
        Some(renderer) => {
            renderer.render(ui, ctx.ndb, &txn, &note);
        }
        None => nostr_ref_fallback_ui(ui, bech),
    }
}

/// Resolve a bech32 entity to a concrete note: `nevent`/`note` directly, `naddr`
/// to the latest replaceable event for its coordinate.
fn resolve_nostr_ref<'a>(ndb: &nostrdb::Ndb, txn: &'a Transaction, bech: &str) -> Option<Note<'a>> {
    if bech.starts_with("nevent1") {
        let id = NoteId::from_nevent_bech(bech)?;
        ndb.get_note_by_id(txn, id.bytes()).ok()
    } else if bech.starts_with("note1") {
        let id = NoteId::from_bech(bech)?;
        ndb.get_note_by_id(txn, id.bytes()).ok()
    } else if bech.starts_with("naddr1") {
        use nostr::nips::nip19::FromBech32;
        let coord = nostr::nips::nip01::Coordinate::from_bech32(bech).ok()?;
        let author = coord.public_key.to_bytes();
        let filter = Filter::new()
            .authors([&author])
            .kinds([coord.kind.as_u16() as u64])
            .tags([coord.identifier.as_str()], 'd')
            .limit(1)
            .build();
        ndb.query(txn, &[filter], 1)
            .ok()?
            .into_iter()
            .next()
            .map(|r| r.note)
    } else {
        None
    }
}

/// Plain, unobtrusive representation of a `nostr:` reference we couldn't render.
fn nostr_ref_fallback_ui(ui: &mut egui::Ui, bech: &str) {
    ui.weak(format!("nostr:{bech}"));
}

fn file_node_ui(ui: &mut egui::Ui, node: &FileNode, rect: Rect, selected: bool) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        ui.label("file node");
    })
}

fn link_node_ui(ui: &mut egui::Ui, node: &LinkNode, rect: Rect, selected: bool) -> egui::Response {
    node_box_ui(ui, node.node(), rect, selected, |ui| {
        ui.label("link node");
    })
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
}

/// Render a node's frame and contents at `rect`, returning a click-and-drag
/// response covering the whole node so the caller can move/select it. The
/// background handle is registered before the content, so non-interactive
/// content widgets (labels) fall through to it and dragging the body moves the
/// node.
fn node_box_ui(
    ui: &mut egui::Ui,
    node: &GenericNode,
    rect: Rect,
    selected: bool,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
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
                    contents(ui);
                    ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
                });
            })
            .response
    });

    // Drag/select handle covering the whole node, registered last so it's the
    // topmost interactive widget — the content above is non-interactive and
    // would otherwise swallow nothing, but a top-level handle is unambiguous.
    ui.interact(
        rect,
        ui.id().with(("notebook_node", node.id.as_str())),
        egui::Sense::click_and_drag(),
    )
}
