//! Convert a reduced [`CanvasView`] into a [`jsoncanvas::JsonCanvas`] for
//! rendering. This is the bridge from the nostr-backed model ([`crate::event`])
//! to the renderer in [`crate::ui`], which is built around the `jsoncanvas`
//! types. Node ids in the produced canvas are the hex of each node's nostr event
//! id, so the UI can map an interaction back to a [`enostr::NoteId`].

use std::path::PathBuf;
use std::str::FromStr;

use jsoncanvas::{
    Background, BackgroundStyle, EdgeId, FileNode, GroupNode, JsonCanvas, LinkNode, Node, NodeId,
    TextNode,
    color::{Color, HexColor, PresetColor},
    edge::{Edge, End, Side},
};
use url::Url;

use crate::event::{CanvasView, EdgeView, NodeKind, NodeView};

/// Build a renderable [`JsonCanvas`] from a folded [`CanvasView`].
pub fn view_to_canvas(view: &CanvasView) -> JsonCanvas {
    let mut canvas = JsonCanvas::default();
    for node in &view.nodes {
        if let Some(n) = to_node(node) {
            let _ = canvas.add_node(n);
        }
    }
    // Edges reference node ids, so add them after the nodes.
    for edge in &view.edges {
        if let Some(e) = to_edge(edge) {
            let _ = canvas.add_edge(e);
        }
    }
    canvas
}

/// The jsoncanvas node id for a node: the hex of its nostr event id.
pub fn node_id(id: &enostr::NoteId) -> Option<NodeId> {
    id.hex().parse().ok()
}

fn to_node(n: &NodeView) -> Option<Node> {
    let id = node_id(&n.id)?;
    let (x, y, w, h) = (n.geo.x, n.geo.y, n.geo.w, n.geo.h);
    let color = n.color.as_deref().and_then(to_color);
    let c = &n.content;

    Some(match n.kind {
        NodeKind::Text => TextNode::new(id, x, y, w, h, color, c.text.clone()).into(),
        NodeKind::File => FileNode::new(
            id,
            x,
            y,
            w,
            h,
            color,
            PathBuf::from(c.file.clone().unwrap_or_default()),
            c.subpath.clone(),
        )
        .into(),
        // A link node needs a valid URL; if it doesn't parse, fall back to a
        // text node showing the raw value so the node still appears.
        NodeKind::Link => match c.url.as_deref().and_then(|u| Url::parse(u).ok()) {
            Some(url) => LinkNode::new(id, x, y, w, h, color, url).into(),
            None => TextNode::new(id, x, y, w, h, color, c.url.clone().unwrap_or_default()).into(),
        },
        NodeKind::Group => {
            let background = c.background.as_ref().map(|img| {
                Background::new(
                    PathBuf::from(img),
                    c.background_style.as_deref().and_then(to_bg_style),
                )
            });
            GroupNode::new(id, x, y, w, h, color, c.label.clone(), background).into()
        }
    })
}

fn to_edge(e: &EdgeView) -> Option<Edge> {
    let id = EdgeId::from_str(&e.id).ok()?;
    let from = node_id(&e.from)?;
    let to = node_id(&e.to)?;
    Some(Edge::new(
        id,
        from,
        e.ends.from_side.as_deref().and_then(to_side),
        e.ends.from_end.as_deref().and_then(to_end),
        to,
        e.ends.to_side.as_deref().and_then(to_side),
        e.ends.to_end.as_deref().and_then(to_end),
        e.ends.color.as_deref().and_then(to_color),
        e.ends.label.clone(),
    ))
}

/// Map a canvasColor string (preset `"1".."6"` or `"#rrggbb"`) to a color.
fn to_color(s: &str) -> Option<Color> {
    match s {
        "1" => Some(PresetColor::Red.into()),
        "2" => Some(PresetColor::Orange.into()),
        "3" => Some(PresetColor::Yellow.into()),
        "4" => Some(PresetColor::Green.into()),
        "5" => Some(PresetColor::Cyan.into()),
        "6" => Some(PresetColor::Purple.into()),
        _ => HexColor::from_str(s).ok().map(Color::Color),
    }
}

fn to_side(s: &str) -> Option<Side> {
    match s {
        "top" => Some(Side::Top),
        "left" => Some(Side::Left),
        "right" => Some(Side::Right),
        "bottom" => Some(Side::Bottom),
        _ => None,
    }
}

fn to_end(s: &str) -> Option<End> {
    match s {
        "arrow" => Some(End::Arrow),
        "none" => Some(End::None),
        _ => None,
    }
}

fn to_bg_style(s: &str) -> Option<BackgroundStyle> {
    match s {
        "cover" => Some(BackgroundStyle::Cover),
        "ratio" => Some(BackgroundStyle::Ratio),
        "repeat" => Some(BackgroundStyle::Repeat),
        _ => None,
    }
}
