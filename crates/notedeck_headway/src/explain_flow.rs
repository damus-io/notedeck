//! explain's ` ```flow ` fence, ported from `explain/flow.py` and
//! `explain/layout.py`: boxes and arrows laid out here, never by hand.
//!
//! ```text
//! id: Title | second line | third {brass}     tone: brass slate moss alert dim
//! a -> b: label        arrow                   a --> b   dashed arrow
//! a -- b               line                    a .. b    dashed line
//! same: a b            same rank, side by side
//! ```
//!
//! `flow dir=right` lays out left to right. Like explain, two drawings are
//! made: the one asked for, and (when that is wider than a phone) a
//! narrower top-down one with its text wrapped tighter. The pane draws the
//! first where it fits and the second where it doesn't, and never scales
//! either, so diagram text stays at explain's size ([`mono_size`]).
//!
//! Each drawing is an SVG in explain's `var(--…)` tokens, rasterised and
//! themed by the same loader a hand-drawn figure goes through.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use crate::explain_diagram::{
    BACKDROP, Diagram, Drawing, EdgeOp, PHONE, diagram_tokens, edge_line, esc, id_len, is_js_ws,
    mono_size, text_w, wrap,
};
use crate::review_figures::{Tone, fence_attr, take_tone};

/// A node's line height.
const LINE: f64 = 17.0;

/// `body` (a flow fence without its caption) drawn, its options read from
/// `info`, the fence's info string: `dir=right` (or `lr`), `wrap=N` (the
/// characters a node line wraps at, 40 by default) and `label="…"`.
pub(crate) fn render_flow(body: &str, info: &str) -> Diagram {
    let spec = parse_flow(body);
    let label = fence_attr(info, "label")
        .map(str::to_owned)
        .unwrap_or_else(|| describe(&spec));
    let down = !matches!(fence_attr(info, "dir"), Some("right" | "lr"));
    let wrap = fence_attr(info, "wrap")
        .and_then(|w| w.parse::<f64>().ok())
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(40.0);
    let wide = draw(&spec, down, wrap, false, &label);
    if wide.width <= PHONE {
        return Diagram {
            wide: wide.drawing,
            narrow: None,
            label,
        };
    }
    // As explain: tighter wraps first, then one box per rank.
    let tries = [34.0, 28.0, 24.0]
        .map(|w| Try {
            wrap: w,
            stack: false,
        })
        .into_iter()
        .chain([34.0, 28.0, 24.0, 20.0, 16.0].map(|w| Try {
            wrap: w,
            stack: true,
        }));
    let mut narrow = None;
    for Try { wrap, stack } in tries {
        let drawn = draw(&spec, true, wrap, stack, &label);
        let fits = drawn.width <= PHONE;
        narrow = Some(drawn);
        if fits {
            break;
        }
    }
    Diagram {
        wide: wide.drawing,
        narrow: narrow.map(|n| n.drawing),
        label,
    }
}

/// One narrow layout to try.
struct Try {
    wrap: f64,
    stack: bool,
}

/// How an edge is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeKind {
    /// `->`
    Arrow,
    /// `-->`
    DashedArrow,
    /// `--`
    Line,
    /// `..`
    DashedLine,
}

impl EdgeKind {
    /// Its operators, in the order explain's regex tries them.
    const OPS: [EdgeOp<Self>; 4] = [
        EdgeOp {
            text: "-->",
            kind: Self::DashedArrow,
        },
        EdgeOp {
            text: "->",
            kind: Self::Arrow,
        },
        EdgeOp {
            text: "--",
            kind: Self::Line,
        },
        EdgeOp {
            text: "..",
            kind: Self::DashedLine,
        },
    ];

    fn arrow(self) -> bool {
        matches!(self, Self::Arrow | Self::DashedArrow)
    }

    fn dashed(self) -> bool {
        matches!(self, Self::DashedArrow | Self::DashedLine)
    }
}

/// A node: its lines (the first its title) and tone.
#[derive(Debug)]
struct Node {
    id: String,
    lines: Vec<String>,
    tone: Option<Tone>,
}

/// An edge between two nodes, by index.
#[derive(Debug)]
struct Edge {
    from: usize,
    to: usize,
    kind: EdgeKind,
    label: String,
    tone: Option<Tone>,
}

/// A flow's graph as written.
#[derive(Debug, Default)]
struct Spec {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Groups of nodes put on one rank, side by side.
    same: Vec<Vec<usize>>,
}

impl Spec {
    /// Node `id`'s index, adding it (its id its only line) on first use.
    fn node(&mut self, id: &str) -> usize {
        if let Some(i) = self.nodes.iter().position(|n| n.id == id) {
            return i;
        }
        self.nodes.push(Node {
            id: id.to_owned(),
            lines: vec![id.to_owned()],
            tone: None,
        });
        self.nodes.len() - 1
    }
}

/// A flow's body read as explain's `parse_flow` does. A line it can't read
/// is skipped (explain warns about it).
fn parse_flow(src: &str) -> Spec {
    let mut spec = Spec::default();
    for raw in src.split('\n') {
        let line = raw.trim_matches(is_js_ws);
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let same = line
            .strip_prefix("same:")
            .map(|s| s.trim_start_matches(is_js_ws))
            .filter(|s| !s.is_empty());
        if let Some(ids) = same {
            let group = ids
                .split(|c: char| is_js_ws(c) || c == ',')
                .filter(|id| !id.is_empty())
                .map(|id| spec.node(id))
                .collect();
            spec.same.push(group);
            continue;
        }
        if let Some(e) = edge_line(line, &EdgeKind::OPS) {
            let toned = take_tone(e.label);
            let from = spec.node(e.from);
            let to = spec.node(e.to);
            spec.edges.push(Edge {
                from,
                to,
                kind: e.kind,
                label: toned.text.trim_matches(is_js_ws).to_owned(),
                tone: toned.tone,
            });
            continue;
        }
        let id = id_len(line);
        let Some(text) = line[id..].trim_start_matches(is_js_ws).strip_prefix(':') else {
            tracing::debug!("flow: cannot read line {line:?}");
            continue;
        };
        if id == 0 {
            continue;
        }
        let toned = take_tone(text.trim_start_matches(is_js_ws));
        let n = spec.node(&line[..id]);
        let node = &mut spec.nodes[n];
        node.lines = toned
            .text
            .split('|')
            .map(|s| s.trim_matches(is_js_ws))
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        node.tone = toned.tone;
    }
    spec
}

/// The graph described, for its label when the fence gives none.
fn describe(spec: &Spec) -> String {
    let name = |i: usize| {
        spec.nodes[i]
            .lines
            .first()
            .map(String::as_str)
            .unwrap_or("undefined")
    };
    let mut out = String::new();
    for (i, e) in spec.edges.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let with = if e.kind.arrow() { "to" } else { "with" };
        let _ = write!(out, "{} {with} {}", name(e.from), name(e.to));
        if !e.label.is_empty() {
            let _ = write!(out, " ({})", e.label);
        }
    }
    out
}

/// One line of a sized node.
struct NodeLine {
    text: String,
    title: bool,
}

/// A node's box size and its wrapped lines.
struct Sized {
    w: f64,
    h: f64,
    lines: Vec<NodeLine>,
}

/// Each node's lines wrapped at `max` and its box sized to them.
fn sized(spec: &Spec, max: f64) -> Vec<Sized> {
    spec.nodes
        .iter()
        .map(|n| {
            let lines: Vec<NodeLine> = n
                .lines
                .iter()
                .enumerate()
                .flat_map(|(i, l)| {
                    wrap(l, max).into_iter().map(move |text| NodeLine {
                        text,
                        title: i == 0,
                    })
                })
                .collect();
            let widest = lines
                .iter()
                .map(|l| text_w(&l.text, 12.0))
                .fold(f64::NEG_INFINITY, f64::max);
            let w = if lines.is_empty() {
                56.0
            } else {
                (widest + 24.0).ceil().max(56.0)
            };
            Sized {
                w,
                h: lines.len() as f64 * LINE + 14.0,
                lines,
            }
        })
        .collect()
}

/// A drawing and how wide it is, before its numbers are narrowed to `f32`.
struct Drawn {
    drawing: Drawing,
    width: f64,
}

/// `spec` laid out (`down` or left to right) with its lines wrapped at
/// `wrap`, as an SVG in explain's tokens, styled as its `.dg` CSS.
fn draw(spec: &Spec, down: bool, wrap: f64, stack: bool, label: &str) -> Drawn {
    let ns = sized(spec, wrap);
    let edges: Vec<LayoutEdge> = spec
        .edges
        .iter()
        .map(|e| LayoutEdge {
            from: e.from,
            to: e.to,
            label_w: if e.label.is_empty() {
                0.0
            } else {
                text_w(&e.label, 11.0)
            },
        })
        .collect();
    let sizes: Vec<NodeSize> = ns.iter().map(|n| NodeSize { w: n.w, h: n.h }).collect();
    let l = layout(&Graph {
        nodes: &sizes,
        edges: &edges,
        same: &spec.same,
        down,
        stack,
    });

    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" role=\"img\" aria-label=\"",
        w = l.width,
        h = l.height
    );
    esc(&mut svg, label);
    svg.push_str(
        "\"><defs><marker id=\"ah\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto\">\
         <path d=\"M0 0L10 5L0 10z\" fill=\"var(--ink-3)\"/></marker></defs>\
         <g font-family=\"var(--mono)\" font-size=\"",
    );
    let _ = write!(svg, "{}\">", mono_size(12.0));
    for r in &l.routes {
        let e = &spec.edges[r.edge];
        let tokens = e.tone.map(diagram_tokens);
        let stroke = tokens.as_ref().map_or("ink-3", |t| t.line);
        svg.push_str("<path d=\"");
        for (i, p) in r.pts.iter().enumerate() {
            let _ = write!(
                svg,
                "{}{:.1} {:.1}",
                if i == 0 { 'M' } else { 'L' },
                p.x,
                p.y
            );
        }
        let _ = write!(
            svg,
            "\" fill=\"none\" stroke=\"var(--{stroke})\" stroke-width=\"1.3\"{}{}/>",
            if e.kind.dashed() {
                " stroke-dasharray=\"4 3\""
            } else {
                ""
            },
            if e.kind.arrow() {
                " marker-end=\"url(#ah)\""
            } else {
                ""
            },
        );
        let Some(at) = &r.label else {
            continue;
        };
        let w = edges[r.edge].label_w;
        let x0 = at.x - at.anchor.before(w);
        let _ = write!(
            svg,
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"15\" fill=\"{BACKDROP}\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\" font-size=\"{:.1}\" fill=\"var(--{})\"{}>",
            x0 - 2.0,
            at.y - 11.0,
            w + 4.0,
            at.x,
            at.y,
            mono_size(11.0),
            tokens.as_ref().map_or("ink-3", |t| t.ink),
            at.anchor.svg(),
        );
        esc(&mut svg, &e.label);
        svg.push_str("</text>");
    }
    for (n, b) in ns.iter().zip(&l.boxes) {
        let tokens = spec.nodes[b.node].tone.map(diagram_tokens);
        let (fill, stroke, title) = match &tokens {
            Some(t) => (t.soft, t.line, t.ink),
            None => ("surface-2", "rule-strong", "ink"),
        };
        let _ = write!(
            svg,
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{}\" height=\"{}\" rx=\"3\" fill=\"var(--{fill})\" stroke=\"var(--{stroke})\"/>",
            b.x, b.y, b.w, b.h
        );
        for (i, line) in n.lines.iter().enumerate() {
            let (ink, weight) = if line.title {
                (title, " font-weight=\"600\"")
            } else {
                ("ink-2", "")
            };
            let _ = write!(
                svg,
                "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"var(--{ink})\"{weight}>",
                b.x + 12.0,
                b.y + 19.5 + i as f64 * LINE
            );
            esc(&mut svg, &line.text);
            svg.push_str("</text>");
        }
    }
    svg.push_str("</g></svg>");
    Drawn {
        drawing: Drawing {
            svg,
            width: l.width as f32,
            height: l.height as f32,
        },
        width: l.width,
    }
}

// ---- layout: a port of explain's layout.py

/// Between neighbouring units on a rank.
const NODE_GAP: f64 = 28.0;
/// When one side is a long edge passing through.
const DUMMY_GAP: f64 = 12.0;
/// Between members of a same-rank group, before labels.
const FLAT_GAP: f64 = 24.0;
const RANK_GAP: f64 = 40.0;
/// Spacing of bend tracks inside a rank gap.
const TRACK: f64 = 9.0;
const LABEL_H: f64 = 18.0;
const MARGIN: f64 = 8.0;

/// A node's box size, for the layout.
struct NodeSize {
    w: f64,
    h: f64,
}

/// An edge, for the layout: its ends and its label's width (0 for none).
struct LayoutEdge {
    from: usize,
    to: usize,
    label_w: f64,
}

/// What the layout lays out.
struct Graph<'a> {
    nodes: &'a [NodeSize],
    edges: &'a [LayoutEdge],
    same: &'a [Vec<usize>],
    /// Top-down, or left to right.
    down: bool,
    /// One unit per rank: the phone fallback.
    stack: bool,
}

/// A laid-out flow.
struct Layout {
    width: f64,
    height: f64,
    /// One per node, in node order.
    boxes: Vec<BoxRect>,
    /// One per edge that isn't a self-loop, in edge order.
    routes: Vec<Route>,
}

/// A node's box.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BoxRect {
    node: usize,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pt {
    x: f64,
    y: f64,
}

/// An edge's polyline and where its label goes.
#[derive(Debug)]
struct Route {
    edge: usize,
    pts: Vec<Pt>,
    label: Option<LabelAt>,
}

/// A label's anchor point and alignment.
#[derive(Debug)]
struct LabelAt {
    x: f64,
    y: f64,
    anchor: Anchor,
}

/// SVG's `text-anchor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Start,
    Middle,
    End,
}

impl Anchor {
    /// How much of a `w` wide label sits before its anchor.
    fn before(self, w: f64) -> f64 {
        match self {
            Self::Start => 0.0,
            Self::Middle => w / 2.0,
            Self::End => w,
        }
    }

    fn svg(self) -> &'static str {
        match self {
            Self::Start => "",
            Self::Middle => " text-anchor=\"middle\"",
            Self::End => " text-anchor=\"end\"",
        }
    }
}

/// A unit: a same-rank group laid out as one, or a dummy a long edge passes
/// through.
struct Unit {
    members: Vec<usize>,
    idx: f64,
    dummy: bool,
    rank: usize,
    size: f64,
    /// Its centre on the cross axis.
    c: f64,
}

/// An edge in the layout's own terms.
struct LEdge {
    frm: usize,
    to: usize,
    label_w: f64,
    /// A back edge, ranked as if reversed.
    rev: bool,
    /// The units it passes, tail first.
    chain: Vec<usize>,
    /// Its arcs, one per link of `chain`.
    arcs: Vec<usize>,
}

/// A unit-to-unit link between neighbouring ranks.
struct LArc {
    a: usize,
    b: usize,
    e: usize,
    k: usize,
    track: usize,
    pa: f64,
    pb: f64,
}

/// One side of a node (or dummy) that arcs leave or enter.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Side {
    Out(usize),
    In(usize),
    DummyOut(usize),
    DummyIn(usize),
}

/// One end of an arc at a [`Side`].
struct PortEnd {
    arc: usize,
    /// Its `a` end, else its `b` end.
    at_a: bool,
}

/// A rank gap's labels and bends.
struct GapInfo {
    label: f64,
    segs: Vec<usize>,
    size: f64,
}

/// A block of the place step's isotonic regression.
struct Block {
    v: f64,
    w: f64,
    n: usize,
}

/// A unit's wanted cross position, and how strongly.
struct Want {
    at: f64,
    weight: f64,
}

/// The layout's working state.
struct Lay<'a> {
    g: &'a Graph<'a>,
    units: Vec<Unit>,
    /// Each node's unit.
    unit_of: Vec<usize>,
    /// Each node's offset from its unit's centre.
    off: Vec<f64>,
    edges: Vec<LEdge>,
    arcs: Vec<LArc>,
}

impl Lay<'_> {
    /// Main-axis size.
    fn ms(&self, n: usize) -> f64 {
        let s = &self.g.nodes[n];
        if self.g.down { s.h } else { s.w }
    }

    /// Cross-axis size.
    fn cs(&self, n: usize) -> f64 {
        let s = &self.g.nodes[n];
        if self.g.down { s.w } else { s.h }
    }

    fn tail(&self, e: usize) -> usize {
        let e = &self.edges[e];
        self.unit_of[if e.rev { e.to } else { e.frm }]
    }

    fn head(&self, e: usize) -> usize {
        let e = &self.edges[e];
        self.unit_of[if e.rev { e.frm } else { e.to }]
    }

    /// Which member of unit `u` arc `arc` touches; none for a dummy.
    fn attach(&self, u: usize, arc: usize) -> Option<usize> {
        if self.units[u].dummy {
            return None;
        }
        let a = &self.arcs[arc];
        let e = &self.edges[a.e];
        let (head, tail) = if e.rev { (e.frm, e.to) } else { (e.to, e.frm) };
        Some(if u == a.a { tail } else { head })
    }

    fn point(&self, u: usize, arc: usize) -> f64 {
        self.units[u].c + self.attach(u, arc).map_or(0.0, |m| self.off[m])
    }

    fn sep(&self, a: usize, b: usize) -> f64 {
        let (a, b) = (&self.units[a], &self.units[b]);
        (a.size + b.size) / 2.0
            + if a.dummy || b.dummy {
                DUMMY_GAP
            } else {
                NODE_GAP
            }
    }
}

/// Each unit's index in its layer.
fn positions(layers: &[Vec<usize>], n_units: usize) -> Vec<usize> {
    let mut p = vec![0; n_units];
    for l in layers {
        for (i, &u) in l.iter().enumerate() {
            p[u] = i;
        }
    }
    p
}

/// Crossings between neighbouring ranks under `layers`' order.
fn crossings(layers: &[Vec<usize>], arcs: &[LArc], units: &[Unit]) -> usize {
    let p = positions(layers, units.len());
    let mut c = 0;
    for r in 0..layers.len().saturating_sub(1) {
        let here: Vec<&LArc> = arcs.iter().filter(|x| units[x.a].rank == r).collect();
        for i in 0..here.len() {
            for j in i + 1..here.len() {
                let da = p[here[i].a] as i64 - p[here[j].a] as i64;
                let db = p[here[i].b] as i64 - p[here[j].b] as i64;
                if da * db < 0 {
                    c += 1;
                }
            }
        }
    }
    c
}

/// A small layered (Sugiyama-style) layout: rank, order, place, route.
/// explain wrote its own rather than use ELK because a flow needs one thing
/// ELK's layered algorithm won't do: a node beside another on one rank (a
/// decision and its side outcome) with an edge between them. Every loop,
/// sort and tie-break keeps explain's order, so a flow is laid out as on
/// the page.
///
/// It works in rank space: "main" runs along the ranks (y for down, x for
/// right), "cross" across them; converted to x/y at the end.
fn layout(g: &Graph<'_>) -> Layout {
    let down = g.down;
    let n_nodes = g.nodes.len();

    // ---- groups: a same-rank group is laid out as one unit
    let mut parent: Vec<usize> = (0..n_nodes).collect();
    fn find(parent: &mut [usize], k: usize) -> usize {
        if parent[k] != k {
            parent[k] = find(parent, parent[k]);
        }
        parent[k]
    }
    for grp in g.same {
        let Some(&first) = grp.first() else {
            continue;
        };
        for &k in &grp[1..] {
            let root = find(&mut parent, k);
            parent[root] = find(&mut parent, first);
        }
    }
    let mut units: Vec<Unit> = Vec::new();
    let mut unit_by_root: HashMap<usize, usize> = HashMap::new();
    let mut unit_of = vec![0; n_nodes];
    for (n, slot) in unit_of.iter_mut().enumerate() {
        let r = find(&mut parent, n);
        let u = *unit_by_root.entry(r).or_insert_with(|| {
            units.push(Unit {
                members: Vec::new(),
                idx: n as f64,
                dummy: false,
                rank: 0,
                size: 0.0,
                c: 0.0,
            });
            units.len() - 1
        });
        units[u].members.push(n);
        *slot = u;
    }
    let n_real = units.len();

    // ---- edges: flat (inside a unit) or layered (between units)
    let edges: Vec<LEdge> = g
        .edges
        .iter()
        .map(|e| LEdge {
            frm: e.from,
            to: e.to,
            label_w: e.label_w,
            rev: false,
            chain: Vec::new(),
            arcs: Vec::new(),
        })
        .collect();
    let flat: Vec<usize> = (0..edges.len())
        .filter(|&i| unit_of[edges[i].frm] == unit_of[edges[i].to] && edges[i].frm != edges[i].to)
        .collect();
    let layered: Vec<usize> = (0..edges.len())
        .filter(|&i| unit_of[edges[i].frm] != unit_of[edges[i].to])
        .collect();
    let mut lay = Lay {
        g,
        units,
        unit_of,
        off: vec![0.0; n_nodes],
        edges,
        arcs: Vec::new(),
    };

    // ---- break cycles: a DFS back edge is ranked as if reversed
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n_real];
    for &e in &layered {
        out[lay.unit_of[lay.edges[e].frm]].push(e);
    }
    fn dfs(u: usize, out: &[Vec<usize>], lay: &mut Lay<'_>, state: &mut [u8]) {
        state[u] = 1;
        for &e in &out[u] {
            let v = lay.unit_of[lay.edges[e].to];
            if state[v] == 1 {
                lay.edges[e].rev = true;
            } else if state[v] == 0 {
                dfs(v, out, lay, state);
            }
        }
        state[u] = 2;
    }
    let mut state = vec![0u8; n_real];
    for u in 0..n_real {
        if state[u] == 0 {
            dfs(u, &out, &mut lay, &mut state);
        }
    }

    // ---- rank: longest path, then pull sources down next to what they feed
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n_real];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n_real];
    for &e in &layered {
        let (t, h) = (lay.tail(e), lay.head(e));
        succs[t].push(h);
        preds[h].push(t);
    }
    let mut indeg: Vec<usize> = preds.iter().map(Vec::len).collect();
    let mut topo = Vec::with_capacity(n_real);
    let mut queue: VecDeque<usize> = (0..n_real).filter(|&u| indeg[u] == 0).collect();
    while let Some(u) = queue.pop_front() {
        topo.push(u);
        for &v in &succs[u] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push_back(v);
            }
        }
    }
    let mut rank = vec![0i64; n_real];
    for &u in &topo {
        rank[u] = preds[u]
            .iter()
            .map(|&p| rank[p] + 1)
            .max()
            .unwrap_or(0)
            .max(0);
    }
    for &u in topo.iter().rev() {
        if preds[u].is_empty() && !succs[u].is_empty() {
            rank[u] = succs[u].iter().map(|&s| rank[s]).min().unwrap_or(0) - 1;
        }
    }
    if g.stack {
        for (i, &u) in topo.iter().enumerate() {
            rank[u] = i as i64;
        }
    }
    let min_rank = topo.iter().map(|&u| rank[u]).min().unwrap_or(0);
    for &u in &topo {
        lay.units[u].rank = (rank[u] - min_rank) as usize;
    }
    let n_ranks = topo
        .iter()
        .map(|&u| lay.units[u].rank + 1)
        .max()
        .unwrap_or(0);

    // ---- chains: long edges get a dummy unit on every rank they cross
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); n_ranks];
    for u in 0..n_real {
        layers[lay.units[u].rank].push(u);
    }
    for &e in &layered {
        let (a, b) = (lay.tail(e), lay.head(e));
        let mut chain = vec![a];
        let (first, end) = (lay.units[a].rank + 1, lay.units[b].rank);
        let idx = lay.units[a].idx + 0.5;
        for (r, layer) in layers.iter_mut().enumerate().take(end).skip(first) {
            lay.units.push(Unit {
                members: Vec::new(),
                idx,
                dummy: true,
                rank: r,
                size: 0.0,
                c: 0.0,
            });
            let d = lay.units.len() - 1;
            layer.push(d);
            chain.push(d);
        }
        chain.push(b);
        for k in 0..chain.len() - 1 {
            lay.arcs.push(LArc {
                a: chain[k],
                b: chain[k + 1],
                e,
                k,
                track: 0,
                pa: 0.0,
                pb: 0.0,
            });
            lay.edges[e].arcs.push(lay.arcs.len() - 1);
        }
        lay.edges[e].chain = chain;
    }
    let n_units = lay.units.len();

    // ---- order: barycenter sweeps, keep the order with fewest crossings
    for l in &mut layers {
        l.sort_by(|&a, &b| lay.units[a].idx.total_cmp(&lay.units[b].idx));
    }
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n_units];
    let mut dn: Vec<Vec<usize>> = vec![Vec::new(); n_units];
    for arc in &lay.arcs {
        dn[arc.a].push(arc.b);
        up[arc.b].push(arc.a);
    }
    let mut best = layers.clone();
    let mut best_c = crossings(&layers, &lay.arcs, &lay.units);
    let mut it = 0;
    while it < 12 && best_c > 0 {
        let mut p = positions(&layers, n_units);
        let even = it % 2 == 0;
        let sweep: Vec<usize> = if even {
            (1..n_ranks).collect()
        } else {
            (0..n_ranks.saturating_sub(1)).rev().collect()
        };
        let nb = if even { &up } else { &dn };
        for r in sweep {
            let l = &mut layers[r];
            let mut bary = vec![0.0; n_units];
            for (i, &u) in l.iter().enumerate() {
                let ns = &nb[u];
                bary[u] = if ns.is_empty() {
                    i as f64
                } else {
                    ns.iter().map(|&v| p[v] as f64).sum::<f64>() / ns.len() as f64
                };
            }
            l.sort_by(|&a, &b| bary[a].total_cmp(&bary[b]));
            for (i, &u) in l.iter().enumerate() {
                p[u] = i;
            }
        }
        let c = crossings(&layers, &lay.arcs, &lay.units);
        if c < best_c {
            best_c = c;
            best = layers.clone();
        }
        it += 1;
    }
    let layers = best;

    // ---- unit geometry: members side by side, flat-edge labels widen the gap
    let flat_gap = |lay: &Lay<'_>, a: usize, b: usize| -> f64 {
        let mut lw: f64 = 0.0;
        for &e in &flat {
            let e = &lay.edges[e];
            if (e.frm == a && e.to == b) || (e.frm == b && e.to == a) {
                lw = lw.max(e.label_w);
            }
        }
        let want = if down {
            lw + 16.0
        } else if lw > 0.0 {
            LABEL_H + 12.0
        } else {
            0.0
        };
        FLAT_GAP.max(want)
    };
    for l in &layers {
        for &u in l {
            if lay.units[u].dummy {
                lay.units[u].size = 0.0;
                continue;
            }
            let members = std::mem::take(&mut lay.units[u].members);
            let mut off = 0.0;
            for (i, &m) in members.iter().enumerate() {
                if i > 0 {
                    off += flat_gap(&lay, members[i - 1], m);
                }
                lay.off[m] = off + lay.cs(m) / 2.0;
                off += lay.cs(m);
            }
            for &m in &members {
                lay.off[m] -= off / 2.0;
            }
            lay.units[u].size = off;
            lay.units[u].members = members;
        }
    }

    // ---- place: pull each unit toward its neighbours, keep order and
    // spacing. Ordered least squares is isotonic regression after removing
    // the spacing (PAVA).
    for l in &layers {
        let mut x = 0.0;
        for (i, &u) in l.iter().enumerate() {
            if i > 0 {
                x += lay.sep(l[i - 1], u);
            }
            lay.units[u].c = x;
        }
    }
    let mut nb_arcs: Vec<Vec<usize>> = vec![Vec::new(); n_units];
    for (i, arc) in lay.arcs.iter().enumerate() {
        nb_arcs[arc.a].push(i);
        nb_arcs[arc.b].push(i);
    }
    let other = |lay: &Lay<'_>, u: usize, arc: usize| -> usize {
        let a = &lay.arcs[arc];
        if a.a == u { a.b } else { a.a }
    };
    for it in 0..40 {
        let order: Vec<usize> = if it % 2 == 1 {
            (0..layers.len()).rev().collect()
        } else {
            (0..layers.len()).collect()
        };
        for r in order {
            let l = &layers[r];
            let want: Vec<Want> = l
                .iter()
                .map(|&u| {
                    let ns = &nb_arcs[u];
                    if ns.is_empty() {
                        return Want {
                            at: lay.units[u].c,
                            weight: 0.05,
                        };
                    }
                    let (mut s, mut w) = (0.0, 0.0);
                    for &arc in ns {
                        let v = other(&lay, u, arc);
                        let wt = match (lay.units[u].dummy, lay.units[v].dummy) {
                            (true, true) => 4.0,
                            (true, false) | (false, true) => 2.0,
                            (false, false) => 1.0,
                        };
                        s += wt * (lay.point(v, arc) - (lay.point(u, arc) - lay.units[u].c));
                        w += wt;
                    }
                    Want {
                        at: s / w,
                        weight: w,
                    }
                })
                .collect();
            let mut shift = vec![0.0];
            for i in 1..l.len() {
                shift.push(shift[i - 1] + lay.sep(l[i - 1], l[i]));
            }
            let mut blocks: Vec<Block> = Vec::new();
            for (i, w) in want.iter().enumerate() {
                blocks.push(Block {
                    v: w.at - shift[i],
                    w: w.weight,
                    n: 1,
                });
                while blocks.len() > 1 && blocks[blocks.len() - 2].v > blocks[blocks.len() - 1].v {
                    let b = blocks.pop().expect("two blocks");
                    let a = blocks.pop().expect("two blocks");
                    blocks.push(Block {
                        v: (a.v * a.w + b.v * b.w) / (a.w + b.w),
                        w: a.w + b.w,
                        n: a.n + b.n,
                    });
                }
            }
            let mut i = 0;
            for b in &blocks {
                for _ in 0..b.n {
                    lay.units[l[i]].c = b.v + shift[i];
                    i += 1;
                }
            }
        }
    }

    // ---- main axis: rank bands, with room in each gap for labels and bends
    let thick: Vec<f64> = layers
        .iter()
        .map(|l| {
            l.iter()
                .filter(|&&u| !lay.units[u].dummy)
                .flat_map(|&u| lay.units[u].members.iter().map(|&m| lay.ms(m)))
                .fold(0.0, f64::max)
        })
        .collect();
    let mut gaps: Vec<GapInfo> = (0..n_ranks.saturating_sub(1))
        .map(|_| GapInfo {
            label: 0.0,
            segs: Vec::new(),
            size: 0.0,
        })
        .collect();
    for arc in &lay.arcs {
        let e = &lay.edges[arc.e];
        if arc.k + 2 == e.chain.len() && e.label_w > 0.0 {
            let gi = &mut gaps[lay.units[arc.a].rank];
            let need = if down {
                LABEL_H + 4.0
            } else {
                e.label_w + 14.0
            };
            gi.label = gi.label.max(need);
        }
    }

    // ports: spread the arcs leaving or entering one side of a node
    let mut sides: HashMap<Side, Vec<PortEnd>> = HashMap::new();
    for i in 0..lay.arcs.len() {
        let (a, b) = (lay.arcs[i].a, lay.arcs[i].b);
        let out = lay.attach(a, i).map_or(Side::DummyOut(a), Side::Out);
        let into = lay.attach(b, i).map_or(Side::DummyIn(b), Side::In);
        sides
            .entry(out)
            .or_default()
            .push(PortEnd { arc: i, at_a: true });
        sides.entry(into).or_default().push(PortEnd {
            arc: i,
            at_a: false,
        });
    }
    let mut ports: Vec<Port> = Vec::new();
    for (side, mut list) in sides {
        let far = |p: &PortEnd| {
            let arc = &lay.arcs[p.arc];
            if p.at_a {
                lay.point(arc.b, p.arc)
            } else {
                lay.point(arc.a, p.arc)
            }
        };
        // A stable sort, so equal ones keep arc order as explain's does.
        list.sort_by(|x, y| far(x).total_cmp(&far(y)));
        let node = match side {
            Side::Out(m) | Side::In(m) => Some(m),
            Side::DummyOut(_) | Side::DummyIn(_) => None,
        };
        let n = list.len();
        let span = node.map_or(0.0, |m| (lay.cs(m) * 0.6).min((n as f64 - 1.0) * 22.0));
        for (i, p) in list.iter().enumerate() {
            let arc = &lay.arcs[p.arc];
            let base = if p.at_a {
                lay.point(arc.a, p.arc)
            } else {
                lay.point(arc.b, p.arc)
            };
            let at = if n > 1 {
                base - span / 2.0 + span * i as f64 / (n as f64 - 1.0)
            } else {
                base
            };
            ports.push(Port {
                arc: p.arc,
                at_a: p.at_a,
                at,
            });
        }
    }
    for p in ports {
        let arc = &mut lay.arcs[p.arc];
        if p.at_a {
            arc.pa = p.at;
        } else {
            arc.pb = p.at;
        }
    }

    // tracks: pieces across one gap that overlap get distinct levels
    for (i, arc) in lay.arcs.iter().enumerate() {
        if (arc.pa - arc.pb).abs() < 1.0 {
            continue;
        }
        gaps[lay.units[arc.a].rank].segs.push(i);
    }
    let span_of = |arc: &LArc| Span {
        lo: arc.pa.min(arc.pb),
        hi: arc.pa.max(arc.pb),
    };
    let mut tracks = vec![0; lay.arcs.len()];
    for gi in &mut gaps {
        gi.segs.sort_by(|&a, &b| {
            span_of(&lay.arcs[a])
                .lo
                .total_cmp(&span_of(&lay.arcs[b]).lo)
        });
        let mut levels: Vec<f64> = Vec::new();
        for &s in &gi.segs {
            let Span { lo, hi } = span_of(&lay.arcs[s]);
            tracks[s] = match levels.iter().position(|&end| end < lo - 6.0) {
                Some(k) => {
                    levels[k] = hi;
                    k
                }
                None => {
                    levels.push(hi);
                    levels.len() - 1
                }
            };
        }
        let n = levels.len().max(1) as f64;
        gi.size = RANK_GAP.max(gi.label + n * TRACK + 18.0);
    }
    for (arc, track) in lay.arcs.iter_mut().zip(tracks) {
        arc.track = track;
    }
    let mut start = Vec::with_capacity(n_ranks);
    for r in 0..n_ranks {
        start.push(if r == 0 {
            MARGIN
        } else {
            start[r - 1] + thick[r - 1] + gaps[r - 1].size
        });
    }

    // ---- cross normalisation
    let mut lo = f64::INFINITY;
    for l in &layers {
        for &u in l {
            lo = lo.min(lay.units[u].c - lay.units[u].size / 2.0);
        }
    }
    let dx = MARGIN - lo;
    if dx.is_finite() {
        for l in &layers {
            for &u in l {
                lay.units[u].c += dx;
            }
        }
        for arc in &mut lay.arcs {
            arc.pa += dx;
            arc.pb += dx;
        }
    }

    // ---- boxes
    let mut boxes: Vec<BoxRect> = Vec::with_capacity(n_nodes);
    for l in &layers {
        for &u in l {
            let unit = &lay.units[u];
            for &m in &unit.members {
                let cc = unit.c + lay.off[m];
                let mm = start[unit.rank] + (thick[unit.rank] - lay.ms(m)) / 2.0;
                let NodeSize { w, h } = g.nodes[m];
                let (x, y) = if down {
                    (cc - w / 2.0, mm)
                } else {
                    (mm, cc - h / 2.0)
                };
                boxes.push(BoxRect {
                    node: m,
                    x,
                    y,
                    w,
                    h,
                });
            }
        }
    }
    boxes.sort_by_key(|b| b.node);
    let xy = |main: f64, cross: f64| {
        if down {
            Pt { x: cross, y: main }
        } else {
            Pt { x: main, y: cross }
        }
    };
    let near = |m: usize| {
        let b = &boxes[m];
        if down { b.y } else { b.x }
    };
    let far = |m: usize| {
        let b = &boxes[m];
        if down { b.y + b.h } else { b.x + b.w }
    };

    // ---- routes
    let mut routes = Vec::new();
    for &e in &layered {
        let edge = &lay.edges[e];
        let mut pts = Vec::new();
        let mut label = None;
        let len = edge.chain.len();
        for (k, &u) in edge.chain.iter().enumerate().take(len - 1) {
            let ai = edge.arcs[k];
            let arc = &lay.arcs[ai];
            let r = lay.units[u].rank;
            let m0 = match lay.attach(u, ai) {
                Some(m) => far(m),
                None => start[r] + thick[r],
            };
            let nxt = edge.chain[k + 1];
            let m1 = match lay.attach(nxt, ai) {
                Some(m) => near(m),
                None => start[r + 1],
            };
            let g0 = start[r] + thick[r];
            if k == 0 {
                pts.push(xy(m0, arc.pa));
            } else {
                // passing through a dummy's band
                pts.push(xy(start[r], arc.pa));
                pts.push(xy(m0, arc.pa));
            }
            if (arc.pa - arc.pb).abs() >= 1.0 {
                let tm = g0 + 9.0 + arc.track as f64 * TRACK;
                pts.push(xy(tm, arc.pa));
                pts.push(xy(tm, arc.pb));
            }
            if k + 2 == len {
                if edge.label_w > 0.0 {
                    label = Some(if down {
                        LabelAt {
                            x: arc.pb + 6.0,
                            y: m1 - 6.0,
                            anchor: Anchor::Start,
                        }
                    } else {
                        LabelAt {
                            x: m1 - 9.0,
                            y: arc.pb - 6.0,
                            anchor: Anchor::End,
                        }
                    });
                }
                pts.push(xy(m1, arc.pb));
            }
        }
        if edge.rev {
            pts.reverse();
        }
        routes.push(Route {
            edge: e,
            pts: dedupe(&pts),
            label,
        });
    }
    for &e in &flat {
        let edge = &lay.edges[e];
        let (a, b) = (boxes[edge.frm], boxes[edge.to]);
        let labelled = edge.label_w > 0.0;
        let (pts, label) = if down {
            let y = (a.y.max(b.y) + (a.y + a.h).min(b.y + b.h)) / 2.0;
            let (x0, x1) = if a.x < b.x {
                (a.x + a.w, b.x)
            } else {
                (a.x, b.x + b.w)
            };
            let label = labelled.then_some(LabelAt {
                x: (x0 + x1) / 2.0,
                y: y - 6.0,
                anchor: Anchor::Middle,
            });
            (vec![Pt { x: x0, y }, Pt { x: x1, y }], label)
        } else {
            let x = (a.x.max(b.x) + (a.x + a.w).min(b.x + b.w)) / 2.0;
            let (y0, y1) = if a.y < b.y {
                (a.y + a.h, b.y)
            } else {
                (a.y, b.y + b.h)
            };
            let label = labelled.then_some(LabelAt {
                x: x + 6.0,
                y: (y0 + y1) / 2.0 + 4.0,
                anchor: Anchor::Start,
            });
            (vec![Pt { x, y: y0 }, Pt { x, y: y1 }], label)
        };
        routes.push(Route {
            edge: e,
            pts,
            label,
        });
    }
    routes.sort_by_key(|r| r.edge);

    let (mut w, mut h) = (0.0f64, 0.0f64);
    for b in &boxes {
        w = w.max(b.x + b.w);
        h = h.max(b.y + b.h);
    }
    for r in &routes {
        for p in &r.pts {
            w = w.max(p.x);
            h = h.max(p.y);
        }
        if let Some(at) = &r.label {
            let lw = lay.edges[r.edge].label_w;
            w = w.max(at.x + lw - at.anchor.before(lw));
        }
    }
    Layout {
        width: (w + MARGIN).ceil(),
        height: (h + MARGIN).ceil(),
        boxes,
        routes,
    }
}

/// The cross-axis extent of an arc's bend.
struct Span {
    lo: f64,
    hi: f64,
}

/// Where one end of an arc lands on its side.
struct Port {
    arc: usize,
    at_a: bool,
    at: f64,
}

/// `pts` without repeats or the middle of three collinear points.
fn dedupe(pts: &[Pt]) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(pts.len());
    for &p in pts {
        if out
            .last()
            .is_some_and(|q| (q.x - p.x).abs() < 0.5 && (q.y - p.y).abs() < 0.5)
        {
            continue;
        }
        out.push(p);
    }
    let near = |a: f64, b: f64| (a - b).abs() < 0.5;
    (0..out.len())
        .filter(|&i| {
            if i == 0 || i + 1 == out.len() {
                return true;
            }
            let (a, p, b) = (out[i - 1], out[i], out[i + 1]);
            !((near(a.x, p.x) && near(b.x, p.x)) || (near(a.y, p.y) && near(b.y, p.y)))
        })
        .map(|i| out[i])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flow with every shape the layout handles: a same-rank pair with a
    /// labelled edge between them, a long edge that passes a rank, a back
    /// edge (a cycle), a line, toned nodes and a line that wraps.
    const GRAPH: &str = "\
a: Start | the first line {brass}
b: Decide
c: Side outcome {alert}
d: Work | does things that wrap_past_the_limit_ofTheLine
e: End {moss}
a -> b: go
b -> c: no
same: b c
b -> d
a --> d: skip
d -> e
e .. b: retry
c -- e
";

    /// `GRAPH` laid out `down` or right at `wrap`, printed as explain's own
    /// layout prints it in a test script, so the two can be diffed.
    fn dump(down: bool, wrap: f64, stack: bool) -> String {
        let spec = parse_flow(GRAPH);
        let ns = sized(&spec, wrap);
        let sizes: Vec<NodeSize> = ns.iter().map(|n| NodeSize { w: n.w, h: n.h }).collect();
        let edges: Vec<LayoutEdge> = spec
            .edges
            .iter()
            .map(|e| LayoutEdge {
                from: e.from,
                to: e.to,
                label_w: if e.label.is_empty() {
                    0.0
                } else {
                    text_w(&e.label, 11.0)
                },
            })
            .collect();
        let l = layout(&Graph {
            nodes: &sizes,
            edges: &edges,
            same: &spec.same,
            down,
            stack,
        });
        let mut out = format!(
            "{} {} {} {} {}\n",
            if down { "down" } else { "right" },
            wrap,
            if stack { "True" } else { "False" },
            l.width,
            l.height
        );
        let mut boxes: Vec<&BoxRect> = l.boxes.iter().collect();
        boxes.sort_by(|a, b| spec.nodes[a.node].id.cmp(&spec.nodes[b.node].id));
        for b in boxes {
            let _ = writeln!(
                out,
                " box {} {:.2} {:.2} {:.0} {:.0}",
                spec.nodes[b.node].id, b.x, b.y, b.w, b.h
            );
        }
        for r in &l.routes {
            let pts: Vec<String> = r
                .pts
                .iter()
                .map(|p| format!("{:.2},{:.2}", p.x, p.y))
                .collect();
            let label = match &r.label {
                Some(at) => format!(
                    "{:.2},{:.2},{}",
                    at.x,
                    at.y,
                    match at.anchor {
                        Anchor::Start => "start",
                        Anchor::Middle => "middle",
                        Anchor::End => "end",
                    }
                ),
                None => "None".to_owned(),
            };
            let _ = writeln!(out, " route {} {} {label}", r.edge, pts.join(" "));
        }
        out
    }

    /// The layout matches explain's, number for number, top-down,
    /// left to right and stacked: printed by explain's `layout.py` on
    /// [`GRAPH`].
    #[test]
    fn layout_matches_explain() {
        let got = [
            dump(true, 40.0, false),
            dump(false, 40.0, false),
            dump(true, 16.0, true),
        ]
        .concat();
        let want = "\
down 40 False 364 356
 box a 87.72 8.00 125 48
 box b 146.89 105.00 68 31
 box c 244.09 105.00 111 31
 box d 8.00 194.00 233 65
 box e 204.52 317.00 56 31
 route 0 161.22,56.00 161.22,65.00 180.89,65.00 180.89,105.00 186.89,99.00,start
 route 1 214.89,120.50 244.09,120.50 229.49,114.50,middle
 route 2 169.89,136.00 169.89,154.00 135.50,154.00 135.50,194.00 None
 route 3 139.22,56.00 139.22,65.00 134.89,65.00 134.89,145.00 113.50,145.00 113.50,194.00 119.50,188.00,start
 route 4 124.50,259.00 124.50,268.00 215.72,268.00 215.72,317.00 None
 route 5 232.52,317.00 232.52,268.00 253.00,268.00 253.00,145.00 191.89,145.00 191.89,136.00 238.52,311.00,start
 route 6 299.59,136.00 299.59,145.00 266.05,145.00 266.05,277.00 249.32,277.00 249.32,317.00 None
right 40 False 737 146
 box a 8.00 19.12 125 48
 box b 208.70 45.96 68 31
 box c 187.20 106.96 111 31
 box d 365.60 8.00 233 65
 box e 672.60 67.74 56 31
 route 0 133.00,54.12 142.00,54.12 142.00,61.46 208.70,61.46 199.70,55.46,end
 route 1 242.70,76.96 242.70,106.96 248.70,95.96,start
 route 2 276.70,52.16 365.60,51.50 None
 route 3 133.00,32.12 142.00,32.12 142.00,33.96 307.20,33.96 307.20,29.50 365.60,29.50 356.60,23.50,end
 route 4 598.60,40.50 607.60,40.50 607.60,73.94 672.60,73.94 None
 route 5 672.60,83.24 607.60,83.24 607.60,85.00 307.20,85.00 307.20,70.76 276.70,70.76 663.60,77.24,end
 route 6 298.20,122.46 307.20,122.46 307.20,102.85 607.60,102.85 607.60,92.54 672.60,92.54 None
down 16 True 297 355
 box a 21.00 8.00 125 48
 box b 80.17 105.00 68 31
 box c 177.37 105.00 111 31
 box d 8.00 185.00 140 82
 box e 129.72 316.00 56 31
 route 0 94.50,56.00 94.50,65.00 114.17,65.00 114.17,105.00 120.17,99.00,start
 route 1 148.17,120.50 177.37,120.50 162.77,114.50,middle
 route 2 103.17,136.00 103.17,145.00 89.00,145.00 89.00,185.00 None
 route 3 72.50,56.00 72.50,65.00 68.17,65.00 68.17,145.00 67.00,145.00 67.00,185.00 73.00,179.00,start
 route 4 78.00,267.00 78.00,276.00 140.92,276.00 140.92,316.00 None
 route 5 157.72,316.00 157.72,276.00 160.00,276.00 160.00,145.00 125.17,145.00 125.17,136.00 163.72,310.00,start
 route 6 232.87,136.00 232.87,145.00 195.29,145.00 195.29,276.00 174.52,276.00 174.52,316.00 None
";
        assert_eq!(got, want);
    }

    /// Every line shape explain reads: nodes with lines and tones, the four
    /// edges with labels and tones, a same group, comments, a node first
    /// named by an edge, an id holding `-` and `.` and an unreadable line.
    #[test]
    fn parse_reads_explains_lines() {
        let spec = parse_flow(
            "\
# a comment
// another
a: Title | second |  | third {moss}
a -> b: go {bad}
b --> c
c -- d.e: plain
d.e .. a
a-b->c
same: b, c d.e
???
z: {nope}
",
        );
        let nodes: Vec<String> = spec
            .nodes
            .iter()
            .map(|n| format!("{} {:?} {:?}", n.id, n.lines, n.tone))
            .collect();
        assert_eq!(
            nodes,
            [
                "a [\"Title\", \"second\", \"third\"] Some(Moss)",
                "b [\"b\"] None",
                "c [\"c\"] None",
                "d.e [\"d.e\"] None",
                "a-b [\"a-b\"] None",
                "z [\"{nope}\"] None",
            ]
        );
        let edges: Vec<String> = spec
            .edges
            .iter()
            .map(|e| format!("{}{:?}{} {:?} {:?}", e.from, e.kind, e.to, e.label, e.tone))
            .collect();
        assert_eq!(
            edges,
            [
                "0Arrow1 \"go\" Some(Alert)",
                "1DashedArrow2 \"\" None",
                "2Line3 \"plain\" None",
                "3DashedLine0 \"\" None",
                "4Arrow2 \"\" None",
            ]
        );
        assert_eq!(spec.same, [vec![1, 2, 3]]);
        assert_eq!(
            describe(&spec),
            "Title to b (go); b to c; c with d.e (plain); d.e with Title; a-b to c"
        );
    }

    /// A small flow fits a phone and has no narrow layout; a wide one gets
    /// one at most a phone wide, and the pane picks between them by width.
    #[test]
    fn narrow_layout_where_the_wide_one_wont_fit() {
        let small = render_flow("a -> b\n", "flow");
        assert!(small.narrow.is_none());
        assert_eq!(small.label, "a to b");

        let wide = render_flow(
            "a: A rather long first node title\nb: Then it\nc: And a third one here\n\
             a -> b\nb -> c\n",
            "flow dir=right label=\"three in a row\"",
        );
        assert_eq!(wide.label, "three in a row");
        let narrow = wide.narrow.as_ref().expect("a narrow layout");
        assert!(wide.wide.width > PHONE as f32, "{}", wide.wide.width);
        assert!(narrow.width <= PHONE as f32, "{}", narrow.width);
        assert!(std::ptr::eq(wide.fitting(wide.wide.width), &wide.wide));
        assert!(std::ptr::eq(wide.fitting(wide.wide.width - 1.0), narrow));
    }

    /// A drawing is a whole SVG in explain's tokens: every colour a known
    /// palette token, so it themes and rasterises like a hand-drawn figure,
    /// with its text escaped.
    #[test]
    fn drawing_rasterises_in_both_themes() {
        let flow = render_flow(
            "a: <Start> & go {brass}\nb: End {dim}\na --> b: yes {ok}\nb .. a\n",
            "flow",
        );
        let svg = &flow.wide.svg;
        assert!(svg.contains("&lt;Start&gt; &amp; go"), "{svg}");
        assert!(svg.contains("stroke-dasharray"), "{svg}");
        assert!(svg.contains("marker-end"), "{svg}");
        let palette = crate::explain_palette::Palette::default();
        let fonts = crate::review_figures::figure_fonts();
        for dark in [false, true] {
            let themed = crate::review_figures::substitute_vars(
                &svg.replace(BACKDROP, "#000000"),
                &palette,
                dark,
            );
            assert!(!themed.contains("var("), "{themed}");
            let raster =
                crate::review_figures::rasterize_svg(&themed, &fonts, 1.0).expect("rasterised");
            assert_eq!(raster.size, egui::vec2(flow.wide.width, flow.wide.height));
            assert!(raster.image.pixels.iter().any(|p| p.a() > 0));
        }
    }
}
