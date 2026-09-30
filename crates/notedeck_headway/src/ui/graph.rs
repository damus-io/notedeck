//! The epic dependency-graph view: a pannable/zoomable scene of the epic's
//! cards as [`graph_node_ui`] nodes joined by blocking edges, with drag-to-connect
//! and hover-to-delete edge editing.

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{
    RADIUS_MD, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_MEDIUM, STROKE_THIN,
};

use super::widgets::{StatusIcon, progress_pill, status_icon_ui};
use super::{BoardUiState, find_card};
use crate::event::{BoardView, ColumnPos};
use crate::store::BoardAction;

/// Render the epic's dependency graph as a full-pane, pannable/zoomable
/// [`egui::Scene`]: the epic's cards as positioned [`graph_node_ui`] nodes, and a
/// blocking arrow ([`notedeck_ui::graph::draw_edge`]) per dependency, laid out by
/// the layered [`layout`](notedeck_ui::graph::layout::layered_layout).
///
/// The model ([`headway::graph::dependency_graph`]) and layout are rebuilt each
/// frame off the freshly folded `view` — an epic's graph is small, and rebuilding
/// keeps the drawing in lockstep with live board edits (a `headway move`, a new
/// blocker) exactly like the detail sheet's per-frame fold. Node ids are node
/// indices, so a layout [`Rect`] and its [`GraphNode`] share the loop index.
///
/// Beyond viewing, the graph edits blocking edges directly: dragging from a
/// node's side handle onto another node draws a `blocker → blocked` edge
/// ([`BoardAction::Block`], cycle-/duplicate-filtered by [`graph_can_connect`]),
/// and an edge's midpoint delete handle removes it ([`BoardAction::Unblock`]) —
/// the same actions the detail pane's blocker editor drives, routed back through
/// `board_ui`. Returns the edit produced this frame, or `None` on a frame that
/// only panned/hovered/opened. Dismissing (back / ✕ / Escape) clears
/// [`BoardUiState::graph_epic`], falling back to the epic's detail.
#[profiling::function]
pub(super) fn graph_view_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let epic = state.graph_epic?;

    // Escape backs out. Consumed so it doesn't also fall through to Chrome's
    // Escape handler (which would toggle the side menu), mirroring the detail pane.
    let mut close = ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

    // Build the model and lay it out. `GRAPH_NODE_SIZE` overrides the layout's
    // default box height so the reserved rect matches the drawn node exactly (the
    // node renderer's landing note): a mismatch would leave gaps or overlaps.
    let graph = headway::graph::collapsed_dependency_graph(view, epic.bytes());
    let edges: Vec<(usize, usize)> = graph.edges.iter().map(|e| (e.from, e.to)).collect();
    let cfg = notedeck_ui::graph::layout::LayoutConfig {
        node_size: GRAPH_NODE_SIZE,
        ..Default::default()
    };
    let rects = notedeck_ui::graph::layout::layered_layout(graph.nodes.len(), &edges, &cfg);

    // Cleared/done dependencies dim so the unfinished critical path is what pops;
    // active ones use a strong line so the arrows read against the node borders.
    let active_edge = theme.border_strong;
    let done_edge = theme.border_default.gamma_multiply(0.5);

    // A leaf node or the topbar's card link clicked this frame; opens that card's
    // detail after the scene closes (we can't touch `state` while the closure
    // borrows the graph and rects).
    let mut open: Option<NoteId> = None;

    // An expandable node (one standing in for a subtree) clicked this frame;
    // drills the graph into that card — re-runs the collapsed model with it as the
    // epic and pushes a graph entry, so a global-back climbs back out a level.
    let mut drill: Option<NoteId> = None;

    // The blocking-edge edit this frame's interactions produced — a drag between
    // two nodes (draw) or a click on an edge's delete handle (remove) — applied
    // after the scene closes. Returned as this view's `BoardAction`.
    let mut edit: Option<BoardAction> = None;

    // The connect-drag in flight, read from state so its source node's handles
    // stay live after the pointer leaves it. The scene closure refreshes it to the
    // node still being dragged from (or none once the button releases).
    let connecting = state.graph_connecting;
    let mut next_connecting: Option<NoteId> = None;

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            graph_topbar_ui(ui, theme, view, epic, &mut close, &mut open);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_MD);

            if graph.nodes.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "This epic has no sub-issues yet, so there's no dependency graph to show.",
                    )
                    .color(theme.text_muted),
                );
                return;
            }

            // Seed the scene to frame the whole graph on first open; thereafter the
            // persisted rect follows the user's panning/zooming.
            let mut scene_rect = state
                .graph_scene_rect
                .unwrap_or_else(|| graph_bounds(&rects).unwrap_or(ui.available_rect_before_wrap()));

            egui::Scene::new().show(ui, &mut scene_rect, |ui| {
                // Which node the pointer sits over, so its incident edges can be
                // highlighted below. This is a geometric test in scene space, not
                // egui hover: the edges are painted before the nodes are allocated,
                // so their responses don't exist yet this frame. Project the global
                // pointer through the scene's layer transform (mirroring notebook's
                // handle hit-test) and test it against each node rect; scan topmost
                // (last-drawn) first so an overlap resolves to the visible node.
                let ptr = ui.ctx().pointer_latest_pos().map(|p| {
                    ui.ctx()
                        .layer_transform_from_global(ui.layer_id())
                        .map_or(p, |t| t * p)
                });
                let hovered = ptr.and_then(|p| {
                    (0..graph.nodes.len())
                        .rev()
                        .find(|&i| !graph.nodes[i].ghost && rects[i].contains(p))
                });

                // Edges first, under the nodes, so the arrowheads tuck beneath the
                // boxes rather than painting over their borders. While a node is
                // hovered its incident edges jump to the accent colour and the rest
                // recede, so the hovered card's dependencies read at a glance.
                for edge in &graph.edges {
                    let (from_rect, to_rect) = (rects[edge.from], rects[edge.to]);
                    let (from_side, to_side) = edge_sides(from_rect, to_rect);
                    let incident = hovered == Some(edge.from) || hovered == Some(edge.to);
                    let base = if edge.done { done_edge } else { active_edge };
                    let color = match hovered {
                        Some(_) if incident => theme.accent,
                        Some(_) => base.gamma_multiply(0.35),
                        None => base,
                    };
                    let width = if incident {
                        notedeck_ui::graph::EDGE_STROKE * 1.6
                    } else {
                        notedeck_ui::graph::EDGE_STROKE
                    };
                    let drawn = notedeck_ui::graph::draw_edge(
                        ui.painter(),
                        from_rect,
                        from_side,
                        to_rect,
                        to_side,
                        color,
                        egui::Stroke::new(width, color),
                    );

                    // A midpoint delete handle removes the edge. Only offered when the
                    // *blocked* endpoint is a live card on this board — that card's
                    // blocker set is what an `Unblock` republishes, so a downstream
                    // ghost (blocked card off this board) can't be edited from here.
                    // And only when the arrow is a *direct* block between these two
                    // cards: a collapsed edge can instead summarise blocks between the
                    // two nodes' subtrees, which an `Unblock` here wouldn't touch —
                    // drill in to edit those.
                    if !graph.nodes[edge.to].ghost
                        && graph_edge_is_direct(
                            view,
                            graph.nodes[edge.from].id,
                            graph.nodes[edge.to].id,
                        )
                        && graph_edge_delete_ui(ui, theme, (edge.from, edge.to), &drawn)
                    {
                        edit = Some(BoardAction::Unblock {
                            card: graph.nodes[edge.to].id,
                            on: graph.nodes[edge.from].id,
                        });
                    }
                }

                // Nodes on top. Title/blocked come straight off the live card
                // (borrowed, no per-frame clone); column/ghost off the model node.
                // A ghost off this board has no card here — an empty title and an
                // unstarted icon, which the node renderer already handles.
                for (i, node) in graph.nodes.iter().enumerate() {
                    let card = find_card(view, node.id).map(|(_, c)| c);
                    let node_view = GraphNodeView {
                        title: card.map(|c| c.title.as_str()).unwrap_or(""),
                        column: node.column,
                        progress: node.progress,
                        blocked: card.is_some_and(|c| c.is_blocked()),
                        ghost: node.ghost,
                    };
                    let resp = graph_node_ui(ui, theme, rects[i], &node_view);
                    // Clicking a node acts on that card: an expandable node (one
                    // standing in for a subtree) drills the graph in a level, a leaf
                    // opens its detail. Ghost context nodes aren't the epic's own work
                    // (and may be off this board), so they stay inert.
                    if resp.clicked() && !node.ghost {
                        if node.progress.is_some() {
                            drill = Some(node.id);
                        } else {
                            open = Some(node.id);
                        }
                    }
                }

                // Connection handles: dots on the sides of a node that draw a new
                // blocking edge when dragged onto another node. Like notebook, they
                // only appear on the node under the pointer and the node a drag is
                // currently coming from, so they don't clutter the whole graph.
                // Ghosts (context, possibly off-board) never sprout handles — they
                // aren't the epic's own work to wire up.
                let connecting_idx = connecting
                    .and_then(|id| graph.nodes.iter().position(|n| n.id == id))
                    .filter(|&i| !graph.nodes[i].ghost);
                let handle_nodes = [hovered.filter(|&i| !graph.nodes[i].ghost), connecting_idx];
                // The live drag this frame: (source node index, source side, pointer
                // pos), and — separately — whether the button is merely held on a
                // handle pre-threshold, so the source survives into next frame.
                let mut dragging: Option<(usize, notedeck_ui::graph::Side, egui::Pos2)> = None;
                let mut released: Option<(usize, egui::Pos2)> = None;
                let mut pressed: Option<usize> = None;
                for slot in 0..handle_nodes.len() {
                    let Some(ni) = handle_nodes[slot] else {
                        continue;
                    };
                    // Skip a node already handled in an earlier slot (hovered == connecting).
                    if handle_nodes[..slot].iter().flatten().any(|&j| j == ni) {
                        continue;
                    }
                    let rect = rects[ni];
                    let id = graph.nodes[ni].id;
                    for (si, side) in GRAPH_SIDES.iter().copied().enumerate() {
                        let center = notedeck_ui::graph::side_point(side, rect);
                        let hit = egui::Rect::from_center_size(
                            center,
                            egui::vec2(GRAPH_HANDLE_HIT, GRAPH_HANDLE_HIT),
                        );
                        let resp = ui.interact(
                            hit,
                            ui.scope_id().with(("hw-graph-handle", id.bytes(), si)),
                            egui::Sense::click_and_drag(),
                        );
                        graph_handle_ui(ui, theme, center, resp.hovered() || resp.dragged());
                        if resp.is_pointer_button_down_on() {
                            pressed = Some(ni);
                        }
                        let pos = resp.interact_pointer_pos();
                        if resp.drag_stopped() {
                            released = Some((ni, pos.unwrap_or(center)));
                        } else if resp.dragged()
                            && let Some(pos) = pos
                        {
                            dragging = Some((ni, side, pos));
                        }
                    }
                }

                // Preview an in-progress drag: a line from the source handle to the
                // pointer, plus a highlight on the node it would legally land on.
                if let Some((ni, side, pos)) = dragging {
                    graph_connection_preview_ui(
                        ui,
                        theme,
                        notedeck_ui::graph::side_point(side, rects[ni]),
                        pos,
                    );
                    if let Some(ti) = graph_node_at(&graph.nodes, &rects, pos, graph.nodes[ni].id)
                        .filter(|&ti| {
                            graph_can_connect(view, graph.nodes[ni].id, graph.nodes[ti].id)
                        })
                    {
                        ui.painter().rect_stroke(
                            rects[ti],
                            egui::CornerRadius::same(RADIUS_MD as u8),
                            egui::Stroke::new(STROKE_MEDIUM, theme.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                }

                // A released drag that landed on a legal target draws the edge:
                // the source is the blocker, the target the blocked card, so the
                // arrow follows the drag (blocker → blocked), matching the layout.
                if let Some((ni, pos)) = released {
                    let from = graph.nodes[ni].id;
                    if let Some(to) = graph_node_at(&graph.nodes, &rects, pos, from)
                        .map(|ti| graph.nodes[ti].id)
                        .filter(|&to| graph_can_connect(view, from, to))
                    {
                        edit = Some(BoardAction::Block { card: to, on: from });
                    }
                }

                // Keep the source node's handles alive while its handle is dragged
                // or merely held (pre-threshold); cleared once the button releases.
                next_connecting = dragging
                    .map(|(ni, _, _)| graph.nodes[ni].id)
                    .or_else(|| pressed.map(|ni| graph.nodes[ni].id));
            });

            state.graph_scene_rect = Some(scene_rect);
            state.graph_connecting = next_connecting;
        });

    if close {
        state.graph_epic = None;
        state.graph_scene_rect = None;
        state.graph_connecting = None;
    }

    // Drill into an expandable node: re-open the graph on that card (reframing the
    // scene). `graph_epic` moving to a new card reads as a graph→graph step, which
    // this frame's nav reconcile pushes as a new entry, so a global-back climbs
    // back out to the parent graph.
    if let Some(child) = drill {
        state.open_graph(child);
    }

    // Open a clicked node's card: leave the graph and select the card so this
    // frame's nav reconcile (see `reconcile_nav`) pushes its detail on top of the
    // graph entry — a back then returns to the graph. The scene rect is left intact
    // so returning keeps the graph's pan/zoom.
    if let Some(card) = open {
        state.graph_epic = None;
        state.graph_connecting = None;
        state.selected = Some(card);
    }

    // Any blocking-edge edit (draw / remove) the interactions produced this frame,
    // routed back through `board_ui` to `store::apply` like the detail pane's
    // blocker editor. `None` on a frame that only panned/hovered/opened.
    edit
}

/// The graph view's top bar: a back affordance and the epic's title as a
/// breadcrumb, matching the detail pane's chrome. `close` is raised on back / ✕,
/// and `open` by its "open card" link — the epic has no live node in its own
/// graph (at most an inert ghost, when an edge drags it in), so nothing on the
/// canvas can reach its detail.
fn graph_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    epic: NoteId,
    close: &mut bool,
    open: &mut Option<NoteId>,
) {
    ui.horizontal(|ui| {
        let back = egui::Button::new(egui::RichText::new("← Back").color(theme.text_secondary))
            .fill(egui::Color32::TRANSPARENT)
            .frame(false);
        if ui.add(back).clicked() {
            *close = true;
        }
        ui.label(egui::RichText::new("›").color(theme.text_muted));
        // Borrowed, not cloned: this runs every frame.
        let title = find_card(view, epic)
            .map(|(_, c)| c.title.as_str())
            .filter(|t| !t.is_empty());
        ui.label(
            egui::RichText::new(title.unwrap_or("Dependency graph"))
                .strong()
                .color(theme.text_primary),
        );
        ui.label(
            egui::RichText::new("· dependency graph")
                .small()
                .color(theme.text_muted),
        );
        // A labelled, accent-coloured link rather than a clickable breadcrumb: the
        // title is this view's heading and reads as one, so a route hidden inside
        // it is a route nobody finds. It's the accent counterpart of the "View
        // dependency graph" action on the card detail that leads here. A titleless
        // epic (archived, or a card off this board) has no detail pane to open, so
        // it gets no link.
        if title.is_some() {
            let link = egui::Link::new(
                egui::RichText::new("↗ Open card")
                    .small()
                    .color(theme.accent),
            );
            if ui
                .add(link)
                .on_hover_text("Open this epic's card detail")
                .clicked()
            {
                *open = Some(epic);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let x = egui::Button::new(egui::RichText::new("✕").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(x).clicked() {
                *close = true;
            }
        });
    });
}

/// Pick which sides a dependency arrow anchors to, from the two laid-out node
/// rects. Ranks stack downward, so a blocker normally sits above the card it
/// blocks: the arrow leaves the blocker's bottom and enters the blocked card's
/// top. A rare back-edge (blocked above blocker) flips so the arrow still runs
/// the short way between the boxes.
fn edge_sides(
    from: egui::Rect,
    to: egui::Rect,
) -> (notedeck_ui::graph::Side, notedeck_ui::graph::Side) {
    use notedeck_ui::graph::Side;
    if to.center().y >= from.center().y {
        (Side::Bottom, Side::Top)
    } else {
        (Side::Top, Side::Bottom)
    }
}

/// The bounding rect of a laid-out graph, expanded by a margin so the framed
/// scene doesn't crop the outermost nodes. `None` for an empty graph.
fn graph_bounds(rects: &[egui::Rect]) -> Option<egui::Rect> {
    let mut it = rects.iter().copied();
    let first = it.next()?;
    Some(
        it.fold(first, |acc, r| acc.union(r))
            .expand(SPACING_LG * 2.0),
    )
}

/// The four sides a graph node's connection handles sit on. A fixed array so the
/// handle loop can key each side's handle by its index.
const GRAPH_SIDES: [notedeck_ui::graph::Side; 4] = [
    notedeck_ui::graph::Side::Top,
    notedeck_ui::graph::Side::Right,
    notedeck_ui::graph::Side::Bottom,
    notedeck_ui::graph::Side::Left,
];
/// Visible radius of a node's connection handle, in scene pixels.
const GRAPH_HANDLE_RADIUS: f32 = 3.5;
/// Click/drag target size of a connection handle — larger than it looks so it's
/// easy to grab on a node's border, mirroring notebook's `HANDLE_HIT`.
const GRAPH_HANDLE_HIT: f32 = 18.0;

/// The topmost non-ghost node whose rect contains `pos`, other than `exclude` —
/// where a connect-drag would land. Scans last-drawn first so an overlap resolves
/// to the visible node, mirroring the hover hit-test. Ghosts are context (possibly
/// off-board) and never a drop target.
fn graph_node_at(
    nodes: &[headway::graph::GraphNode],
    rects: &[egui::Rect],
    pos: egui::Pos2,
    exclude: NoteId,
) -> Option<usize> {
    (0..nodes.len())
        .rev()
        .find(|&i| !nodes[i].ghost && nodes[i].id != exclude && rects[i].contains(pos))
}

/// Whether a blocking edge `blocker → blocked` may be drawn — the write path's
/// rule ([`store::apply`](crate::store::apply)) pre-checked so an illegal drop is refused before it
/// emits a [`BoardAction`] the reducer would decline. The *blocked* card must be a
/// live card on this board (its blocker set is what an `Add` republishes), the
/// edge must be new, and it mustn't close a dependency cycle
/// ([`store::would_block_cycle`](crate::store::would_block_cycle), which also rejects a self-edge). Mirrors the
/// detail pane's blocker picker (`grid::card_blocker_menu`).
fn graph_can_connect(view: &BoardView, blocker: NoteId, blocked: NoteId) -> bool {
    find_card(view, blocked).is_some_and(|(_, c)| !c.blocked_by.iter().any(|e| e.id == blocker))
        && !crate::store::would_block_cycle(view, blocked, blocker)
}

/// Whether `blocker` *directly* blocks `blocked` — i.e. `blocked`'s own blocker
/// set names `blocker`. The collapsed graph can draw an arrow between two nodes
/// that only summarises blocks between their subtrees; such an aggregated edge is
/// not a direct card-level block and so can't be removed with a single `Unblock`.
fn graph_edge_is_direct(view: &BoardView, blocker: NoteId, blocked: NoteId) -> bool {
    find_card(view, blocked).is_some_and(|(_, c)| c.blocked_by.iter().any(|e| e.id == blocker))
}

/// Draw a node's connection handle: a small dot on a side that starts a blocking
/// edge when dragged. Brightens and grows while grabbable or being dragged from,
/// mirroring notebook's `connection_handle_ui`. Neutral-toned (the text palette)
/// so it doesn't read as a stray accent dot on the graph.
fn graph_handle_ui(ui: &egui::Ui, theme: &ColorTheme, center: egui::Pos2, active: bool) {
    let (color, radius) = if active {
        (theme.text_primary, GRAPH_HANDLE_RADIUS + 1.5)
    } else {
        (theme.text_muted, GRAPH_HANDLE_RADIUS)
    };
    let painter = ui.painter();
    painter.circle_filled(center, radius, color);
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(1.0_f32, theme.surface_primary),
    );
}

/// Draw an in-progress connect-drag: a line from the source handle to the pointer
/// with a dot marking where the edge would land.
fn graph_connection_preview_ui(
    ui: &egui::Ui,
    theme: &ColorTheme,
    from: egui::Pos2,
    to: egui::Pos2,
) {
    let painter = ui.painter();
    painter.line_segment(
        [from, to],
        egui::Stroke::new(notedeck_ui::graph::EDGE_STROKE, theme.text_muted),
    );
    painter.circle_filled(to, GRAPH_HANDLE_RADIUS, theme.text_muted);
}

/// Draw and interact an edge's midpoint delete handle, returning `true` on the
/// frame it's clicked. Like notebook's `edge_ui`, the handle only shows while the
/// pointer is near the edge's *curve* (not just its bounding box) or over the
/// handle itself, and reads as a subtle dot that turns into a red ✕ under the
/// pointer. `key` (the edge's node-index pair) gives the interactions a stable id.
fn graph_edge_delete_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    key: (usize, usize),
    drawn: &notedeck_ui::graph::DrawnEdge,
) -> bool {
    // Hover the curve itself, not its (often large) bounding box: interact over the
    // bounds for a pointer position, then measure distance to the flattened curve.
    let bounds =
        egui::Rect::from_points(&drawn.polyline).expand(notedeck_ui::graph::EDGE_HOVER_DIST);
    let hover = ui.interact(
        bounds,
        ui.scope_id().with(("hw-graph-edge", key)),
        egui::Sense::hover(),
    );
    let over_edge = hover.hover_pos().is_some_and(|p| {
        notedeck_ui::graph::dist_to_polyline(&drawn.polyline, p)
            <= notedeck_ui::graph::EDGE_HOVER_DIST
    });

    let hit =
        egui::Rect::from_center_size(drawn.mid, egui::vec2(GRAPH_HANDLE_HIT, GRAPH_HANDLE_HIT));
    let resp = ui.interact(
        hit,
        ui.scope_id().with(("hw-graph-edge-del", key)),
        egui::Sense::click(),
    );
    if over_edge || resp.hovered() {
        graph_edge_delete_handle_ui(ui.painter(), theme, drawn.mid, resp.hovered());
    }
    resp.clicked()
}

/// Draw an edge's midpoint delete handle: a faint dot at rest, a filled red circle
/// with a white ✕ under the pointer (signalling a click removes the edge). Mirrors
/// notebook's `edge_delete_handle_ui`, themed off [`ColorTheme::destructive`].
fn graph_edge_delete_handle_ui(
    painter: &egui::Painter,
    theme: &ColorTheme,
    center: egui::Pos2,
    active: bool,
) {
    if active {
        let radius = 8.0;
        painter.circle_filled(center, radius, theme.destructive);
        let d = radius * 0.45;
        let cross = egui::Stroke::new(2.0_f32, egui::Color32::WHITE);
        painter.line_segment(
            [center + egui::vec2(-d, -d), center + egui::vec2(d, d)],
            cross,
        );
        painter.line_segment(
            [center + egui::vec2(-d, d), center + egui::vec2(d, -d)],
            cross,
        );
    } else {
        painter.circle_filled(center, 3.0, theme.text_muted);
        painter.circle_stroke(
            center,
            3.0,
            egui::Stroke::new(1.0_f32, theme.surface_primary),
        );
    }
}

/// The fixed on-screen size of a dependency-graph node box.
///
/// A graph node is drawn at a rect the layered layout hands it, so the layout
/// has to know how much space to reserve *before* any node renders — it can't
/// measure an immediate-mode widget ahead of time. Pinning the size to this
/// constant (fed into [`notedeck_ui::graph::layout::LayoutConfig::node_size`])
/// keeps the reserved rect and the drawn box in lockstep: every node is this big,
/// so titles truncate rather than reflowing the graph. One text row plus the
/// icon, the ⊘/title gaps, and the box's own margins fit inside the height.
pub const GRAPH_NODE_SIZE: egui::Vec2 = egui::vec2(220.0, 56.0);

/// How far a *done* graph node's surface fades toward the pane behind it:
/// `0.0` leaves it a normal card, `1.0` dissolves it into the background.
///
/// Picked off a rendered busy graph rather than by taste. In the dark theme it
/// takes the box from `0x44` to `0x25` against a `0x1F` pane — past the ghost
/// surface (`0x2C`), so finished work sits *behind* out-of-subtree context
/// rather than level with it, and the unfinished nodes are the only bright
/// boxes left on a crowded graph.
const GRAPH_DONE_FILL_FADE: f32 = 0.85;

/// How far a *done* node's border fades — deliberately much less than
/// [`GRAPH_DONE_FILL_FADE`].
///
/// The two cues can't fade at the same rate because they don't carry the same
/// weight in both themes: the light theme's elevated surface *is* the pane
/// colour (both white), so there the border is the only thing holding the box
/// together. Faded as hard as the fill, a done node would stop being a box at
/// all in light mode. At this strength it lands about where a ghost's border
/// already sits in the light theme — the established "still a card, just
/// quiet" weight.
const GRAPH_DONE_BORDER_FADE: f32 = 0.45;

/// The content opacity of a *done* node — status circle, ⊘, title, progress
/// pill. Lower than [`GRAPH_GHOST_OPACITY`] because the box underneath has
/// faded too, so the content has to come down with it or the node reads as a
/// bright label floating on a washed-out card.
const GRAPH_DONE_OPACITY: f32 = 0.35;

/// The content opacity of a *ghost* (out-of-subtree context) node. Its box
/// keeps a card's full weight on a recessed surface, so the content only needs
/// to step back, not disappear.
const GRAPH_GHOST_OPACITY: f32 = 0.55;

/// The display state of one dependency-graph node — everything
/// [`graph_node_ui`] needs to paint a card as a positioned node, resolved by the
/// caller from the graph model and board.
///
/// It is the render-time view of a [`headway::graph::GraphNode`]: `column` and
/// `ghost` come straight off the model node, `title` is looked up from the board
/// by the node's id, and `blocked` is [`CardView::is_blocked`](crate::event::CardView::is_blocked) for that card.
pub struct GraphNodeView<'a> {
    /// The card's title, drawn (truncated) beside the status icon.
    pub title: &'a str,
    /// The card's live [`ColumnPos`], driving the Linear-style status circle and
    /// the node's done/cleared recede. `None` (archived / off-board) draws the
    /// unstarted backlog icon, mirroring [`card_chip_ui`](super::card_chip_ui).
    pub column: Option<ColumnPos>,
    /// The card is held back by an unfinished blocker ([`CardView::is_blocked`](crate::event::CardView::is_blocked));
    /// leads the title with a dim ⊘, the same tell as `grid::card_ui`.
    pub blocked: bool,
    /// This node is a *ghost* — pulled into the graph only to anchor a
    /// cross-subtree edge, not one of the epic's own cards. Drawn as recessed
    /// context (muted surface, dimmed content, no hover affordance).
    pub ghost: bool,
    /// The card's subtree progress ([`headway::graph::GraphNode::progress`]),
    /// `Some` when this node stands in for a whole subtree in the collapsed
    /// graph. Drawn as a right-aligned `done/total` pill and marks the node as
    /// *expandable* (a click drills in rather than opening the card). A full
    /// `done/done` also counts as finished work and fades the node, even when
    /// the standing-in card's own column hasn't reached Done — there is nothing
    /// left under it to do.
    pub progress: Option<headway::graph::SubtreeProgress>,
}

/// Draw a card as a graph node inside `rect` and return its (clickable) response.
///
/// The node reads like the inline [`card_chip_ui`](super::card_chip_ui) — the same status circle and
/// title — but sits in a fixed [`GRAPH_NODE_SIZE`] box with a border so it holds
/// its place in the laid-out graph, and it carries the two graph-only tells the
/// chip has no room for: a leading ⊘ for a [`GraphNodeView::blocked`] card, and a
/// recessed style for nodes that should stay out of the eye's way — *ghost*
/// context nodes and *done* cards, so the unfinished critical path is what pops.
///
/// A done node recedes as a whole box, fill and border included
/// ([`GRAPH_DONE_FILL_FADE`]), not just its content: on a busy graph most of the
/// nodes are finished, and a full-weight rectangle competes with the unfinished
/// work whatever is written inside it.
///
/// The returned [`egui::Response`] senses clicks for every node so a view can
/// open the card; only non-ghost nodes get the hover border and pointing-hand
/// cursor, since ghosts are context rather than the epic's own work. The fade is
/// purely paint: a done node keeps its hover border, its click, its drill-in and
/// its connect handles, all of which gate on `ghost` alone.
pub fn graph_node_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    rect: egui::Rect,
    node: &GraphNodeView,
) -> egui::Response {
    let response = ui.allocate_rect(rect, egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    // Derive the status circle exactly as the inline chip does, so a node and a
    // chip of the same card read identically; the last column is `Done`.
    let icon = match node.column {
        Some(pos) => StatusIcon::for_column(pos.index, pos.count),
        None => StatusIcon::Backlog,
    };
    // Finished work: the card sits in a terminal column, or it stands in for a
    // subtree with nothing left in it. The second case only exists on a
    // collapsed node, and it's the one that clutters a busy graph most — a
    // branch that's wholly done still reserves a full-weight box.
    let subtree_done = node
        .progress
        .is_some_and(|p| p.total > 0 && p.done == p.total);
    let done = matches!(icon, StatusIcon::Done) || subtree_done;

    // Ghost and done both step out of the eye's way, but they mean different
    // things, so they recede along different axes and compose rather than
    // override each other. A *ghost* is context from outside the subtree: it
    // swaps the card's material for the recessed secondary surface, at full
    // weight. A *done* node is the epic's own work, finished: it keeps its
    // material and instead fades it toward the pane behind it. A done ghost
    // therefore fades from the secondary surface, landing dimmer than either.
    let (mut fill, mut border) = if node.ghost {
        (
            theme.surface_secondary,
            theme.border_default.gamma_multiply(0.6),
        )
    } else {
        (theme.surface_elevated, theme.border_default)
    };
    if done {
        fill = fill.lerp_to_gamma(theme.surface_primary, GRAPH_DONE_FILL_FADE);
        border = border.lerp_to_gamma(theme.surface_primary, GRAPH_DONE_BORDER_FADE);
    }
    // The fade is painted, not applied as widget opacity, so the box stays
    // opaque: edges are drawn *under* the nodes, and a translucent done node
    // would let an arrowhead show through its own box.
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(RADIUS_MD as u8),
        fill,
        egui::Stroke::new(STROKE_THIN, border),
        egui::StrokeKind::Inside,
    );

    // Hover affordance for the epic's own cards only; ghosts stay inert.
    if response.hovered() && !node.ghost {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(RADIUS_MD as u8),
            egui::Stroke::new(STROKE_MEDIUM, theme.border_strong),
            egui::StrokeKind::Inside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Content: status icon, an optional ⊘, then the truncated title, laid out in
    // a child clipped to the box's interior so a long title can't overflow it.
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(SPACING_SM))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // The content dims with the box under it, and on the same compose rule: a
    // done ghost takes the lower *done* opacity, matching a fill that has
    // already faded past the plain ghost surface.
    if done {
        content.set_opacity(GRAPH_DONE_OPACITY);
    } else if node.ghost {
        content.set_opacity(GRAPH_GHOST_OPACITY);
    }
    content.spacing_mut().item_spacing.x = SPACING_XS;
    let row_height = content.text_style_height(&egui::TextStyle::Body);
    let icon_size = (row_height * 0.85).round();
    content.allocate_ui_with_layout(
        egui::vec2(icon_size, row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            status_icon_ui(ui, theme, icon, icon_size);
        },
    );
    if node.blocked {
        content
            .label(egui::RichText::new("⊘").small().color(theme.text_muted))
            .on_hover_text("Blocked by unfinished work");
    }
    // The title fills the row. A collapsed node — one standing in for a whole
    // subtree — also shows a right-aligned done/total pill and reads as
    // expandable; a plain node (leaf or ghost) keeps the title spanning the row.
    match node.progress {
        Some(p) => {
            content.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                progress_pill(ui, theme, p.done, p.total)
                    .on_hover_text(format!("{} of {} sub-issues done", p.done, p.total));
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(node.title).color(theme.text_primary))
                            .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                });
            });
        }
        None => {
            content.add(
                egui::Label::new(egui::RichText::new(node.title).color(theme.text_primary))
                    .wrap_mode(egui::TextWrapMode::Truncate),
            );
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{self, ColumnView};
    use crate::ui::tests::BOARD;
    use egui_kittest::Harness;

    /// A bare harness on egui's default fonts, which lack the graph's `←` and
    /// `⊘`: they draw as tofu here, as they did before egui 0.36 made a
    /// missing glyph panic under kittest. The app's bundled fonts have them
    /// (`tests/glyphs.rs` checks that).
    fn graph_harness<'a>(app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
        Harness::builder().allow_missing_glyphs().build_ui(app)
    }

    /// Every node variant — plain, blocked, done, and ghost — renders through a
    /// live frame without panicking and reports back exactly the fixed
    /// [`GRAPH_NODE_SIZE`] rect it was handed, so the layout's reserved space and
    /// the drawn box stay in lockstep. Exercises the whole paint path (status
    /// icon, ⊘ glyph, recede opacity) the geometry alone can't.
    #[test]
    fn graph_node_renders_variants_at_its_rect() {
        let three = |idx: usize| {
            Some(ColumnPos {
                index: idx,
                count: 3,
            })
        };
        let cases = [
            GraphNodeView {
                title: "plain in-progress node",
                column: three(1),
                blocked: false,
                ghost: false,
                progress: None,
            },
            GraphNodeView {
                title: "blocked node",
                column: three(0),
                blocked: true,
                ghost: false,
                progress: None,
            },
            GraphNodeView {
                title: "done node recedes",
                column: three(2),
                blocked: false,
                ghost: false,
                progress: None,
            },
            GraphNodeView {
                title: "off-board ghost node",
                column: None,
                blocked: false,
                ghost: true,
                progress: None,
            },
            GraphNodeView {
                title: "collapsed parent shows progress pill",
                column: three(1),
                blocked: false,
                ghost: false,
                progress: Some(headway::graph::SubtreeProgress { done: 2, total: 5 }),
            },
        ];

        for node in &cases {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), GRAPH_NODE_SIZE);
            let mut got = None;
            let mut harness = graph_harness(|ui| {
                let theme = ColorTheme::current(ui.ctx());
                got = Some(graph_node_ui(ui, &theme, rect, node).rect);
            });
            harness.run();
            // Drop the harness so its closure releases its borrow of `got`.
            drop(harness);
            assert_eq!(
                got.expect("node body always runs"),
                rect,
                "{}: node should occupy exactly the rect it was given",
                node.title
            );
        }
    }

    /// A card with a distinct id (`n`), the given column placement, parent,
    /// sub-issues and blockers — enough to fold a small dependency graph.
    fn graph_card(
        n: u8,
        parent: Option<u8>,
        subissues: &[u8],
        blocked_by: &[u8],
    ) -> event::CardView {
        let eref = |m: &u8| event::EdgeRef {
            id: NoteId::new([*m; 32]),
            title: format!("card {m}"),
            done: false,
        };
        let subv = |m: &u8| event::SubissueView {
            id: NoteId::new([*m; 32]),
            title: format!("card {m}"),
            column: Some("backlog".to_string()),
            done: false,
            archived: false,
            seq: None,
        };
        event::CardView {
            id: NoteId::new([n; 32]),
            author: [0u8; 32],
            title: format!("card {n}"),
            description: String::new(),
            labels: vec![],
            priority: headway::event::Priority::None,
            due: None,
            estimate: None,
            rank: "m".to_string(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            reviews: vec![],
            activity: vec![],
            parent: parent.map(|p| NoteId::new([p; 32])),
            subissues: subissues.iter().map(subv).collect(),
            blocked_by: blocked_by.iter().map(eref).collect(),
            blocks: vec![],
            related: vec![],
        }
    }

    /// A one-column board holding `cards`, so the whole set folds and every card
    /// resolves a live [`ColumnPos`].
    fn graph_board(cards: Vec<event::CardView>) -> BoardView {
        BoardView {
            id: BOARD.to_string(),
            author: [0u8; 32],
            title: "b".to_string(),
            description: String::new(),
            created_at: 0,
            columns: vec![ColumnView {
                id: "backlog".to_string(),
                name: "Backlog".to_string(),
                terminal: false,
                cards,
            }],
            archived: vec![],
        }
    }

    /// The assembled graph view renders an epic's chain through a live frame
    /// without panicking and, on first draw, seeds the persisted scene rect so
    /// pan/zoom carries across frames.
    #[test]
    fn graph_view_renders_and_seeds_scene() {
        // Epic E(1) owns A(2) and B(3); B is blocked by A — one internal edge.
        let epic = graph_card(1, None, &[2, 3], &[]);
        let a = graph_card(2, Some(1), &[], &[]);
        let b = graph_card(3, Some(1), &[], &[2]);
        let view = graph_board(vec![epic, a, b]);
        let epic_id = NoteId::new([1u8; 32]);

        let mut state = BoardUiState::default();
        state.open_graph(epic_id);
        assert_eq!(state.graph_epic(), Some(epic_id));
        assert!(
            state.graph_scene_rect.is_none(),
            "scene rect unseeded on open"
        );

        let mut harness = graph_harness(|ui| {
            let theme = ColorTheme::current(ui.ctx());
            let action = graph_view_ui(ui, &theme, &view, &mut state);
            assert!(
                action.is_none(),
                "a plain render frame (no edge drag/delete) mutates nothing"
            );
        });
        harness.run();
        drop(harness);

        assert!(
            state.graph_scene_rect.is_some(),
            "first draw frames the graph into the scene rect"
        );
    }

    /// Clicking a node in the graph opens that card: the click lands on the node's
    /// [`egui::Response`], which sets `selected` and closes graph mode so the frame's
    /// nav reconcile (see `crate::reconcile_nav`) pushes the card's detail on top of
    /// the graph. Drives a real click through the `egui::Scene`: accesskit reports
    /// node boxes in scene-local space, so the click point is mapped to global
    /// through the scene layer's `to_global` transform, and delivered as move-then-
    /// press so egui resolves it against the node (see the node-interaction card
    /// headway:headway/hybrid-blossom-menu).
    #[test]
    fn graph_node_click_selects_card_and_closes_graph() {
        use std::cell::RefCell;

        // Epic E(1) owns A(2) and B(3); B is blocked by A — one internal edge.
        let epic = graph_card(1, None, &[2, 3], &[]);
        let a = graph_card(2, Some(1), &[], &[]);
        let b = graph_card(3, Some(1), &[], &[2]);
        let view = graph_board(vec![epic, a, b]);
        let epic_id = NoteId::new([1u8; 32]);
        let a_id = NoteId::new([2u8; 32]);

        // Rebuild the model + layout the view uses so we know where node A lands in
        // scene coordinates without scraping it back out of the render.
        let graph = headway::graph::dependency_graph(&view, epic_id.bytes());
        let edges: Vec<(usize, usize)> = graph.edges.iter().map(|e| (e.from, e.to)).collect();
        let cfg = notedeck_ui::graph::layout::LayoutConfig {
            node_size: GRAPH_NODE_SIZE,
            ..Default::default()
        };
        let rects = notedeck_ui::graph::layout::layered_layout(graph.nodes.len(), &edges, &cfg);
        let a_idx = graph
            .nodes
            .iter()
            .position(|n| n.id == a_id)
            .expect("A is one of the epic's nodes");
        let a_center = rects[a_idx].center();

        let state = RefCell::new(BoardUiState::default());
        state.borrow_mut().open_graph(epic_id);

        let mut harness = graph_harness(|ui| {
            let theme = ColorTheme::current(ui.ctx());
            graph_view_ui(ui, &theme, &view, &mut state.borrow_mut());
        });
        // First frame seeds + settles the scene transform.
        harness.run();

        // Map A's scene-local centre to global through the scene layer transform.
        let to_global = harness
            .ctx
            .memory(|m| {
                m.to_global
                    .values()
                    .find(|t| **t != egui::emath::TSTransform::IDENTITY)
                    .copied()
            })
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let target = to_global * a_center;

        // Move onto the node first so egui resolves the following click against it.
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(target));
        harness.run();
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        harness.run();

        let state = state.borrow();
        assert_eq!(
            state.selected(),
            Some(a_id),
            "clicking node A selects its card"
        );
        assert_eq!(state.graph_epic(), None, "opening a card leaves graph mode");
    }

    /// The topbar's link opens the epic's own detail: the graph draws the epic's
    /// sub-issues, not the epic, so the top bar is its only route in.
    #[test]
    fn graph_topbar_link_opens_the_epic() {
        use egui_kittest::kittest::Queryable;
        use std::cell::RefCell;

        let epic = graph_card(1, None, &[2], &[]);
        let a = graph_card(2, Some(1), &[], &[]);
        let view = graph_board(vec![epic, a]);
        let epic_id = NoteId::new([1u8; 32]);

        let state = RefCell::new(BoardUiState::default());
        state.borrow_mut().open_graph(epic_id);

        let mut harness = graph_harness(|ui| {
            let theme = ColorTheme::current(ui.ctx());
            graph_view_ui(ui, &theme, &view, &mut state.borrow_mut());
        });
        harness.run();
        // The link sits in the topbar above the scene, after the breadcrumb.
        harness.get_by_label("↗ Open card").click_accesskit();
        harness.run();

        let state = state.borrow();
        assert_eq!(
            state.selected(),
            Some(epic_id),
            "the topbar link selects the epic's card"
        );
        assert_eq!(state.graph_epic(), None, "opening a card leaves graph mode");
    }

    /// [`graph_can_connect`] mirrors the write path: a new, acyclic edge to a live
    /// on-board card is allowed; a duplicate, a self-edge, a cycle-closing edge, or
    /// an edge into an unknown card is refused.
    #[test]
    fn graph_can_connect_matches_write_rule() {
        // A(2) blocks B(3): B already has A as a blocker. C(4) is unconnected.
        let a = graph_card(2, None, &[], &[]);
        let b = graph_card(3, None, &[], &[2]);
        let c = graph_card(4, None, &[], &[]);
        let view = graph_board(vec![a, b, c]);
        let a_id = NoteId::new([2u8; 32]);
        let b_id = NoteId::new([3u8; 32]);
        let c_id = NoteId::new([4u8; 32]);
        let unknown = NoteId::new([9u8; 32]);

        // A fresh acyclic edge C → A (A blocked by C) is fine.
        assert!(graph_can_connect(&view, c_id, a_id));
        // A → B already exists — refused as a duplicate.
        assert!(!graph_can_connect(&view, a_id, b_id));
        // B → A would close the A → B → A loop — refused as a cycle.
        assert!(!graph_can_connect(&view, b_id, a_id));
        // A self-edge is refused (would_block_cycle rejects card == on).
        assert!(!graph_can_connect(&view, a_id, a_id));
        // An edge into a card not on this board can't be edited here.
        assert!(!graph_can_connect(&view, a_id, unknown));
    }

    /// Dragging from one node's side handle onto another node draws a blocking
    /// edge: the source is the blocker, the target the blocked card, so the view
    /// emits `Block { card: target, on: source }` — the same action the detail
    /// pane's blocker picker drives. Drives a real drag through the `egui::Scene`,
    /// mapping scene-local anchors to global through the layer transform and
    /// delivering move → press → move → release (see the node-interaction card
    /// headway:headway/hybrid-blossom-menu for the transform gotcha).
    #[test]
    fn graph_drag_draws_block_edge() {
        use std::cell::RefCell;

        // Epic E(1) owns A(2), B(3), C(4) with no blockers — a legal A → C draw.
        let epic = graph_card(1, None, &[2, 3, 4], &[]);
        let a = graph_card(2, Some(1), &[], &[]);
        let b = graph_card(3, Some(1), &[], &[]);
        let c = graph_card(4, Some(1), &[], &[]);
        let view = graph_board(vec![epic, a, b, c]);
        let epic_id = NoteId::new([1u8; 32]);
        let a_id = NoteId::new([2u8; 32]);
        let c_id = NoteId::new([4u8; 32]);

        // Rebuild the model + layout to find where A's handles and C sit.
        let graph = headway::graph::dependency_graph(&view, epic_id.bytes());
        let edges: Vec<(usize, usize)> = graph.edges.iter().map(|e| (e.from, e.to)).collect();
        let cfg = notedeck_ui::graph::layout::LayoutConfig {
            node_size: GRAPH_NODE_SIZE,
            ..Default::default()
        };
        let rects = notedeck_ui::graph::layout::layered_layout(graph.nodes.len(), &edges, &cfg);
        let idx = |id: NoteId| graph.nodes.iter().position(|n| n.id == id).unwrap();
        let (a_idx, c_idx) = (idx(a_id), idx(c_id));
        let c_center = rects[c_idx].center();
        // Start the drag from A's handle nearest C, in scene-local space.
        let a_handle = GRAPH_SIDES
            .iter()
            .map(|&s| notedeck_ui::graph::side_point(s, rects[a_idx]))
            .min_by(|p, q| p.distance(c_center).total_cmp(&q.distance(c_center)))
            .unwrap();

        let state = RefCell::new(BoardUiState::default());
        state.borrow_mut().open_graph(epic_id);
        let captured: RefCell<Option<BoardAction>> = RefCell::new(None);

        let mut harness = graph_harness(|ui| {
            let theme = ColorTheme::current(ui.ctx());
            if let Some(action) = graph_view_ui(ui, &theme, &view, &mut state.borrow_mut()) {
                *captured.borrow_mut() = Some(action);
            }
        });
        harness.run();

        let to_global = harness
            .ctx
            .memory(|m| {
                m.to_global
                    .values()
                    .find(|t| **t != egui::emath::TSTransform::IDENTITY)
                    .copied()
            })
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let from = to_global * a_handle;
        let to = to_global * c_center;

        // Hover the source handle so A's handles are laid out, then press it.
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(from));
        harness.run();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.run();
        // Drag across to C (crosses the drag threshold), then release on it.
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(to));
        harness.run();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run();

        match captured.borrow().as_ref() {
            Some(BoardAction::Block { card, on }) => {
                assert_eq!(*card, c_id, "the blocked card is the drop target C");
                assert_eq!(*on, a_id, "the blocker is the dragged source A");
            }
            _ => panic!("dragging A → C should emit a Block edge"),
        }
    }

    /// Clicking an edge's midpoint delete handle removes that edge: the view emits
    /// `Unblock { card: blocked, on: blocker }`, mirroring the detail pane's ✕.
    #[test]
    fn graph_edge_delete_handle_unblocks() {
        use std::cell::RefCell;

        // Epic E(1) owns A(2) and B(3); B is blocked by A — one internal edge.
        let epic = graph_card(1, None, &[2, 3], &[]);
        let a = graph_card(2, Some(1), &[], &[]);
        let b = graph_card(3, Some(1), &[], &[2]);
        let view = graph_board(vec![epic, a, b]);
        let epic_id = NoteId::new([1u8; 32]);
        let a_id = NoteId::new([2u8; 32]);
        let b_id = NoteId::new([3u8; 32]);

        let graph = headway::graph::dependency_graph(&view, epic_id.bytes());
        let edges: Vec<(usize, usize)> = graph.edges.iter().map(|e| (e.from, e.to)).collect();
        let cfg = notedeck_ui::graph::layout::LayoutConfig {
            node_size: GRAPH_NODE_SIZE,
            ..Default::default()
        };
        let rects = notedeck_ui::graph::layout::layered_layout(graph.nodes.len(), &edges, &cfg);
        let idx = |id: NoteId| graph.nodes.iter().position(|n| n.id == id).unwrap();
        // The blocker (A) ranks above the blocked card (B): the edge leaves A's
        // bottom for B's top, so its midpoint sits between them in the gap where no
        // node occludes the delete handle (within its 18px hit of the true bezier
        // midpoint).
        let (a_idx, b_idx) = (idx(a_id), idx(b_id));
        let top = rects[a_idx].center_bottom();
        let bottom = rects[b_idx].center_top();
        let mid = top + (bottom - top) * 0.5;

        let state = RefCell::new(BoardUiState::default());
        state.borrow_mut().open_graph(epic_id);
        let captured: RefCell<Option<BoardAction>> = RefCell::new(None);

        let mut harness = graph_harness(|ui| {
            let theme = ColorTheme::current(ui.ctx());
            if let Some(action) = graph_view_ui(ui, &theme, &view, &mut state.borrow_mut()) {
                *captured.borrow_mut() = Some(action);
            }
        });
        harness.run();

        let to_global = harness
            .ctx
            .memory(|m| {
                m.to_global
                    .values()
                    .find(|t| **t != egui::emath::TSTransform::IDENTITY)
                    .copied()
            })
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let target = to_global * mid;

        // Hover the edge so the delete handle appears, then click it.
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(target));
        harness.run();
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        harness.run();

        match captured.borrow().as_ref() {
            Some(BoardAction::Unblock { card, on }) => {
                assert_eq!(*card, b_id, "the unblocked card is the blocked endpoint B");
                assert_eq!(*on, a_id, "the removed blocker is A");
            }
            _ => panic!("clicking the delete handle should emit an Unblock edge"),
        }
    }

    /// A blocker above the card it blocks anchors bottom→top; a back-edge (blocked
    /// above its blocker) flips to top→bottom so the arrow runs the short way.
    #[test]
    fn edge_sides_follow_rank_stacking() {
        use notedeck_ui::graph::Side;
        let upper = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), GRAPH_NODE_SIZE);
        let lower = egui::Rect::from_min_size(egui::pos2(0.0, 300.0), GRAPH_NODE_SIZE);
        assert_eq!(edge_sides(upper, lower), (Side::Bottom, Side::Top));
        assert_eq!(edge_sides(lower, upper), (Side::Top, Side::Bottom));
    }

    /// The framed scene bounds enclose every node with margin to spare, and an
    /// empty layout frames nothing.
    #[test]
    fn graph_bounds_encloses_nodes() {
        let rects = [
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), GRAPH_NODE_SIZE),
            egui::Rect::from_min_size(egui::pos2(300.0, 200.0), GRAPH_NODE_SIZE),
        ];
        let bounds = graph_bounds(&rects).expect("non-empty layout has bounds");
        assert!(bounds.contains_rect(rects[0]) && bounds.contains_rect(rects[1]));
        assert!(
            bounds.min.x < 0.0 && bounds.min.y < 0.0,
            "expanded past the nodes"
        );
        assert!(graph_bounds(&[]).is_none(), "empty layout frames nothing");
    }
}
