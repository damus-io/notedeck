//! explain's ` ```sequence ` fence, ported from `explain/sequence.py`:
//! lifelines and the messages between them, top to bottom.
//!
//! ```text
//! client: Nostr client          a participant (optional; order = columns)
//! client -> relay: REQ sub1     a message          -->  a dashed reply
//! relay -> relay: check         a message to itself
//! note relay: text              a note on one lifeline ("note a,b: text" across two)
//! ```
//!
//! A message's label can end in a `{tone}`. `gap=N` on the fence line sets
//! the column gap (150 by default) and `label="…"` the accessible label.
//! Like a flow, a sequence wider than a phone gets a second drawing with its
//! columns squeezed to a phone's width and its labels wrapped to the span
//! they sit over; the pane picks between them ([`Diagram::fitting`]).

use std::fmt::Write as _;

use crate::explain_diagram::{
    Diagram, Drawing, EdgeOp, PHONE, diagram_tokens, edge_line, esc, id_len, is_js_ws, mono_size,
    text_w, wrap,
};
use crate::review_figures::{Tone, fence_attr, take_tone};

/// A label's line height (11px text).
const LH: f64 = 15.0;

/// A participant: its id and the label its head shows.
#[derive(Debug)]
struct Part {
    id: String,
    label: String,
}

/// A message from one lifeline to another (or itself), by index.
#[derive(Debug)]
struct Message {
    from: usize,
    to: usize,
    /// `-->`: a reply.
    dash: bool,
    label: String,
    tone: Option<Tone>,
}

/// A note over one lifeline, or across two.
#[derive(Debug)]
struct Note {
    parts: Vec<usize>,
    label: String,
}

/// One line of a sequence, top to bottom.
#[derive(Debug)]
enum Step {
    Message(Message),
    Note(Note),
}

/// A sequence as written.
#[derive(Debug, Default)]
struct Spec {
    parts: Vec<Part>,
    steps: Vec<Step>,
}

impl Spec {
    /// Participant `id`'s index, adding it (labelled with its id) on first
    /// use.
    fn part(&mut self, id: &str) -> usize {
        if let Some(i) = self.parts.iter().position(|p| p.id == id) {
            return i;
        }
        self.parts.push(Part {
            id: id.to_owned(),
            label: id.to_owned(),
        });
        self.parts.len() - 1
    }
}

/// A sequence's message operators, in the order explain's regex tries them;
/// each says whether it is dashed.
const OPS: [EdgeOp<bool>; 2] = [
    EdgeOp {
        text: "-->",
        kind: true,
    },
    EdgeOp {
        text: "->",
        kind: false,
    },
];

/// `body` (a sequence fence without its caption) drawn, its options read
/// from `info`, the fence's info string: `gap=N` and `label="…"`. `None`
/// when it names no participant, so there is nothing to draw.
pub(crate) fn render_sequence(body: &str, info: &str) -> Option<Diagram> {
    let spec = parse_sequence(body);
    if spec.parts.is_empty() {
        return None;
    }
    let label = fence_attr(info, "label")
        .map(str::to_owned)
        .unwrap_or_else(|| describe(&spec));
    let gap = fence_attr(info, "gap")
        .and_then(|g| g.parse::<f64>().ok())
        .filter(|g| g.is_finite() && *g > 0.0)
        .unwrap_or(150.0);
    let wide = draw(&spec, gap, false, &label);
    if f64::from(wide.width) <= PHONE {
        return Some(Diagram {
            wide,
            narrow: None,
            label,
        });
    }
    // More than five lifelines won't fit a phone; it scrolls (explain warns).
    let col_w = ((PHONE - 12.0) / spec.parts.len() as f64).max(64.0);
    let narrow = draw(&spec, col_w, true, &label);
    Some(Diagram {
        wide,
        narrow: Some(narrow),
        label,
    })
}

/// A `note a: text` or `note a,b: text` line, as read.
struct NoteLine<'a> {
    first: &'a str,
    second: Option<&'a str>,
    label: &'a str,
}

/// `line` read as a note, as explain's regex reads it: `note`, whitespace,
/// one id or two split by a comma, a colon and the text.
fn note_line(line: &str) -> Option<NoteLine<'_>> {
    let after = line.strip_prefix("note")?;
    let rest = after.trim_start_matches(is_js_ws);
    if rest.len() == after.len() {
        return None;
    }
    let a = id_len(rest);
    if a == 0 {
        return None;
    }
    let mut tail = rest[a..].trim_start_matches(is_js_ws);
    let mut second = None;
    if let Some(more) = tail.strip_prefix(',') {
        let more = more.trim_start_matches(is_js_ws);
        let b = id_len(more);
        if b == 0 {
            return None;
        }
        second = Some(&more[..b]);
        tail = more[b..].trim_start_matches(is_js_ws);
    }
    let label = tail.strip_prefix(':')?.trim_start_matches(is_js_ws);
    Some(NoteLine {
        first: &rest[..a],
        second,
        label,
    })
}

/// A sequence's body read as explain's `parse_sequence` does: a note, then
/// a message, then a participant. A line it can't read is skipped (explain
/// warns about it).
fn parse_sequence(src: &str) -> Spec {
    let mut spec = Spec::default();
    for raw in src.split('\n') {
        let line = raw.trim_matches(is_js_ws);
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(n) = note_line(line) {
            let mut parts = vec![spec.part(n.first)];
            if let Some(second) = n.second {
                parts.push(spec.part(second));
            }
            spec.steps.push(Step::Note(Note {
                parts,
                label: n.label.to_owned(),
            }));
            continue;
        }
        if let Some(e) = edge_line(line, &OPS) {
            let toned = take_tone(e.label);
            let from = spec.part(e.from);
            let to = spec.part(e.to);
            spec.steps.push(Step::Message(Message {
                from,
                to,
                dash: e.kind,
                label: toned.text.trim_matches(is_js_ws).to_owned(),
                tone: toned.tone,
            }));
            continue;
        }
        let id = id_len(line);
        let label = line[id..]
            .trim_start_matches(is_js_ws)
            .strip_prefix(':')
            .filter(|_| id > 0);
        let Some(label) = label else {
            tracing::debug!("sequence: cannot read line {line:?}");
            continue;
        };
        let p = spec.part(&line[..id]);
        spec.parts[p].label = label.trim_start_matches(is_js_ws).to_owned();
    }
    spec
}

/// The sequence described, for its label when the fence gives none.
fn describe(spec: &Spec) -> String {
    let name = |i: usize| spec.parts[i].label.as_str();
    let mut out = String::new();
    for (i, step) in spec.steps.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let _ = match step {
            Step::Note(n) => {
                out.push_str("note on ");
                for (k, &p) in n.parts.iter().enumerate() {
                    if k > 0 {
                        out.push_str(" and ");
                    }
                    out.push_str(name(p));
                }
                write!(out, ": {}", n.label)
            }
            Step::Message(m) => write!(out, "{} to {}: {}", name(m.from), name(m.to), m.label),
        };
    }
    out
}

/// A sequence laid out: every position its drawing needs, in explain's
/// numbers.
struct Laid {
    width: f64,
    height: f64,
    /// Each participant's head width, all one size.
    head_w: f64,
    head_h: f64,
    /// Each lifeline's x.
    xs: Vec<f64>,
    /// Each head's wrapped label.
    heads: Vec<Vec<String>>,
    /// The steps, in order.
    steps: Vec<Placed>,
}

/// A step, placed.
enum Placed {
    Note(PlacedNote),
    Message(PlacedMessage),
}

/// A note's box and its wrapped lines.
struct PlacedNote {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    lines: Vec<String>,
}

/// A message's label lines (the first baseline at `label_y`) and its arrow.
struct PlacedMessage {
    /// Its index in the spec's steps.
    step: usize,
    lines: Vec<String>,
    label_x: f64,
    label_y: f64,
    /// A message to itself: a loop out to the right and back, its label
    /// left-aligned; otherwise a straight arrow under a centred label.
    loops: bool,
    /// The arrow's tail, its head and its height.
    x0: f64,
    x1: f64,
    y: f64,
}

/// `spec` laid out as explain's `draw` does, its columns `col_w` apart.
/// `fixed` (explain's narrow `vn` drawing) keeps every gap at `col_w` and
/// sizes the heads to fit it; otherwise gaps widen to fit the heads and
/// any label over one span.
fn lay_out(spec: &Spec, col_w: f64, fixed: bool) -> Laid {
    let n = spec.parts.len();
    let mut gaps = vec![col_w; n.saturating_sub(1)];
    let head_w = if fixed {
        col_w - 8.0
    } else {
        spec.parts
            .iter()
            .map(|p| text_w(&p.label, 12.0) + 20.0)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    if !fixed {
        for g in &mut gaps {
            *g = g.max(head_w + 16.0);
        }
        for step in &spec.steps {
            let Step::Message(m) = step else {
                continue;
            };
            if m.from == m.to {
                continue;
            }
            let a = m.from.min(m.to);
            let b = m.from.max(m.to);
            let need = text_w(&m.label, 11.0).min(52.0 * 6.6) + 24.0;
            let have: f64 = gaps[a..b].iter().sum();
            if have < need {
                let more = (need - have) / (b - a) as f64;
                for g in &mut gaps[a..b] {
                    *g += more;
                }
            }
        }
    }
    let side = if fixed {
        head_w / 2.0 + 6.0
    } else {
        (head_w / 2.0).max(60.0) + 6.0
    };
    let mut xs = Vec::with_capacity(n);
    xs.push(side);
    for g in &gaps {
        xs.push(xs[xs.len() - 1] + g);
    }
    let width = (xs[n - 1] + side).ceil();
    let head_max = ((head_w - 12.0) / 7.2).floor().max(6.0);
    let heads: Vec<Vec<String>> = spec
        .parts
        .iter()
        .map(|p| wrap(&p.label, head_max))
        .collect();
    let head_h = heads.iter().map(Vec::len).max().unwrap_or(0) as f64 * 16.0 + 14.0;

    let mut y = 8.0 + head_h + 18.0;
    let mut steps = Vec::with_capacity(spec.steps.len());
    for (i, step) in spec.steps.iter().enumerate() {
        match step {
            Step::Note(note) => {
                let lo = note
                    .parts
                    .iter()
                    .map(|&p| xs[p])
                    .fold(f64::INFINITY, f64::min);
                let hi = note
                    .parts
                    .iter()
                    .map(|&p| xs[p])
                    .fold(f64::NEG_INFINITY, f64::max);
                let w = (hi - lo + 80.0).max(if fixed { 120.0 } else { 220.0 });
                let lines = wrap(&note.label, ((w.min(width - 12.0) - 16.0) / 6.6).floor());
                let widest = lines
                    .iter()
                    .map(|l| text_w(l, 11.0))
                    .fold(f64::NEG_INFINITY, f64::max);
                let bw = (width - 12.0).min(widest + 16.0);
                let bx = ((lo + hi) / 2.0 - bw / 2.0).max(6.0).min(width - 6.0 - bw);
                let bh = lines.len() as f64 * LH + 10.0;
                steps.push(Placed::Note(PlacedNote {
                    x: bx,
                    y,
                    w: bw,
                    h: bh,
                    lines,
                }));
                y += bh + 14.0;
            }
            Step::Message(m) => {
                let a = xs[m.from];
                let b = xs[m.to];
                let loops = m.from == m.to;
                let span = if loops { 150.0 } else { (b - a).abs() };
                let lines = if m.label.is_empty() {
                    Vec::new()
                } else {
                    wrap(&m.label, ((span - 12.0) / 6.6).floor().max(8.0))
                };
                let label_x = if loops { a + 8.0 } else { (a + b) / 2.0 };
                let ly = y + lines.len() as f64 * LH + 6.0;
                let x1 = if loops {
                    a + 3.0
                } else {
                    // Stop short of the lifeline, on whichever side it is.
                    b + if b > a { -2.0 } else { 2.0 }
                };
                steps.push(Placed::Message(PlacedMessage {
                    step: i,
                    lines,
                    label_x,
                    label_y: y + 11.0,
                    loops,
                    x0: a,
                    x1,
                    y: ly,
                }));
                y = ly + if loops { 30.0 } else { 18.0 };
            }
        }
    }
    Laid {
        width,
        height: y + 4.0,
        head_w,
        head_h,
        xs,
        heads,
        steps,
    }
}

/// `spec` laid out ([`lay_out`]) as an SVG in explain's tokens, styled as
/// its `.dg` CSS.
fn draw(spec: &Spec, col_w: f64, fixed: bool, label: &str) -> Drawing {
    let l = lay_out(spec, col_w, fixed);
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
    for (x, lines) in l.xs.iter().zip(&l.heads) {
        let _ = write!(
            svg,
            "<rect x=\"{:.1}\" y=\"8\" width=\"{}\" height=\"{}\" rx=\"3\" fill=\"var(--surface-2)\" stroke=\"var(--rule-strong)\"/>",
            x - l.head_w / 2.0,
            l.head_w,
            l.head_h
        );
        for (k, line) in lines.iter().enumerate() {
            let _ = write!(
                svg,
                "<text x=\"{x:.1}\" y=\"{}\" text-anchor=\"middle\" fill=\"var(--ink)\" font-weight=\"600\">",
                8.0 + 21.0 + k as f64 * 16.0
            );
            esc(&mut svg, line);
            svg.push_str("</text>");
        }
        let _ = write!(
            svg,
            "<line x1=\"{x}\" y1=\"{}\" x2=\"{x}\" y2=\"{}\" stroke=\"var(--rule-strong)\" stroke-width=\"1\" stroke-dasharray=\"4 3\"/>",
            8.0 + l.head_h,
            l.height - 4.0
        );
    }
    let small = mono_size(11.0);
    for placed in &l.steps {
        match placed {
            Placed::Note(n) => {
                let _ = write!(
                    svg,
                    "<rect x=\"{:.1}\" y=\"{}\" width=\"{:.1}\" height=\"{}\" rx=\"2\" fill=\"var(--brass-soft)\" stroke=\"var(--brass)\"/>",
                    n.x, n.y, n.w, n.h
                );
                for (i, line) in n.lines.iter().enumerate() {
                    let _ = write!(
                        svg,
                        "<text x=\"{:.1}\" y=\"{}\" font-size=\"{small}\" fill=\"var(--brass-ink)\">",
                        n.x + 8.0,
                        n.y + 16.0 + i as f64 * LH
                    );
                    esc(&mut svg, line);
                    svg.push_str("</text>");
                }
            }
            Placed::Message(m) => {
                let Step::Message(msg) = &spec.steps[m.step] else {
                    continue;
                };
                let tokens = msg.tone.map(diagram_tokens);
                let ink = tokens.as_ref().map_or("ink-3", |t| t.ink);
                let stroke = tokens.as_ref().map_or("ink-3", |t| t.line);
                let anchor = if m.loops {
                    ""
                } else {
                    " text-anchor=\"middle\""
                };
                for (i, line) in m.lines.iter().enumerate() {
                    let _ = write!(
                        svg,
                        "<text x=\"{:.1}\" y=\"{}\" font-size=\"{small}\" fill=\"var(--{ink})\"{anchor}>",
                        m.label_x,
                        m.label_y + i as f64 * LH
                    );
                    esc(&mut svg, line);
                    svg.push_str("</text>");
                }
                svg.push_str("<path d=\"");
                arrow_path(&mut svg, m);
                let _ = write!(
                    svg,
                    "\" fill=\"none\" stroke=\"var(--{stroke})\" stroke-width=\"1.3\"{} marker-end=\"url(#ah)\"/>",
                    if msg.dash {
                        " stroke-dasharray=\"4 3\""
                    } else {
                        ""
                    },
                );
            }
        }
    }
    svg.push_str("</g></svg>");
    Drawing {
        svg,
        width: l.width as f32,
        height: l.height as f32,
    }
}

/// A message's arrow as an SVG path, numbers formatted as explain's: a loop
/// out 28 to the right and 16 down, or a straight line.
fn arrow_path(out: &mut String, m: &PlacedMessage) {
    let _ = if m.loops {
        write!(
            out,
            "M{} {}H{}V{}H{}",
            m.x0,
            m.y,
            m.x0 + 28.0,
            m.y + 16.0,
            m.x1
        )
    } else {
        write!(out, "M{:.1} {}H{:.1}", m.x0, m.y, m.x1)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sequence with every shape: a declared participant and one only
    /// used, a label long enough to wrap and to widen its gap, a message to
    /// itself, a toned reply, a note on one lifeline and one across two,
    /// and a message with no label.
    const SEQ: &str = "\
client: Nostr client
relay: Relay
# a comment
client -> relay: REQ sub1 with a label long enough to wrap past the span
relay -> relay: check the filter {slate}
relay --> client: EVENT {ok}
note relay: indexes it
note client,relay: a note across the two lifelines that is long enough to wrap
db -> client
";

    /// `SEQ` laid out at `col_w`, printed element by element as explain's
    /// own SVG holds them (rects, texts, lines and paths, numbers formatted
    /// as it formats them), so the two can be diffed.
    fn dump(col_w: f64, fixed: bool) -> String {
        let spec = parse_sequence(SEQ);
        let l = lay_out(&spec, col_w, fixed);
        let mut out = format!("size {} {}\n", l.width, l.height);
        for (x, lines) in l.xs.iter().zip(&l.heads) {
            let _ = writeln!(
                out,
                "rect {:.1} 8 {} {}",
                x - l.head_w / 2.0,
                l.head_w,
                l.head_h
            );
            for (k, line) in lines.iter().enumerate() {
                let _ = writeln!(out, "text {x:.1} {} {line}", 29.0 + k as f64 * 16.0);
            }
            let _ = writeln!(out, "line {x} {} {x} {}", 8.0 + l.head_h, l.height - 4.0);
        }
        for placed in &l.steps {
            match placed {
                Placed::Note(n) => {
                    let _ = writeln!(out, "rect {:.1} {} {:.1} {}", n.x, n.y, n.w, n.h);
                    for (i, line) in n.lines.iter().enumerate() {
                        let _ = writeln!(
                            out,
                            "text {:.1} {} {line}",
                            n.x + 8.0,
                            n.y + 16.0 + i as f64 * LH
                        );
                    }
                }
                Placed::Message(m) => {
                    for (i, line) in m.lines.iter().enumerate() {
                        let _ = writeln!(
                            out,
                            "text {:.1} {} {line}",
                            m.label_x,
                            m.label_y + i as f64 * LH
                        );
                    }
                    out.push_str("path ");
                    arrow_path(&mut out, m);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// The layout matches explain's number for number: the expected text is
    /// what explain's `sequence.py` drew for `SEQ`, wide at a 150 gap and
    /// narrow at 154, read back out of its SVG.
    #[test]
    fn layout_matches_explain() {
        assert_eq!(
            dump(150.0, false),
            "\
size 650 306
rect 12.8 8 106.39999999999999 30
text 66.0 29 Nostr client
line 66 38 66 302
rect 380.0 8 106.39999999999999 30
text 433.2 29 Relay
line 433.2 38 433.2 302
rect 530.0 8 106.39999999999999 30
text 583.2 29 db
line 583.2 38 583.2 302
text 249.6 67 REQ sub1 with a label long enough to wrap past the
text 249.6 82 span
path M66.0 92H431.2
text 441.2 121 check the filter
path M433.2 131H461.2V147H436.2
text 249.6 172 EVENT
path M433.2 182H68.0
rect 392.2 200 82.0 25
text 400.2 216 indexes it
rect 46.9 239 405.4 25
text 54.9 255 a note across the two lifelines that is long enough to wrap
path M583.2 284H68.0
"
        );
        assert_eq!(
            dump(154.0, true),
            "\
size 466 336
rect 6.0 8 146 30
text 79.0 29 Nostr client
line 79 38 79 332
rect 160.0 8 146 30
text 233.0 29 Relay
line 233 38 233 332
rect 314.0 8 146 30
text 387.0 29 db
line 387 38 387 332
text 156.0 67 REQ sub1 with a label
text 156.0 82 long enough to wrap
text 156.0 97 past the span
path M79.0 107H231.0
text 241.0 136 check the filter
path M233 146H261V162H236
text 156.0 187 EVENT
path M233.0 197H81.0
rect 192.0 215 82.0 25
text 200.0 231 indexes it
rect 45.7 254 220.6 40
text 53.7 270 a note across the two lifelines
text 53.7 285 that is long enough to wrap
path M387.0 314H81.0
"
        );
    }

    /// Lines read as explain's regexes read them: a note before a message
    /// before a participant, `a-->b` backtracking to the id `a-`, and a line
    /// that is none of them skipped.
    #[test]
    fn parse_reads_explains_lines() {
        let spec = parse_sequence(
            "note a, b: across\na-->b\nc --> a: back {bad}\nnote: a participant\n\
             notes: another\nb: Bee\nnonsense here\n",
        );
        let parts: Vec<String> = spec
            .parts
            .iter()
            .map(|p| format!("{}={}", p.id, p.label))
            .collect();
        assert_eq!(
            parts,
            [
                "a=a",
                "b=Bee",
                "a-=a-",
                "c=c",
                "note=a participant",
                "notes=another"
            ]
        );
        assert_eq!(
            describe(&spec),
            "note on a and Bee: across; a- to Bee: ; c to a: back"
        );
        let Step::Message(back) = &spec.steps[2] else {
            panic!("a message");
        };
        assert!(back.dash);
        assert_eq!(back.tone, Some(Tone::Alert));
        let Step::Message(solid) = &spec.steps[1] else {
            panic!("a message");
        };
        assert!(!solid.dash, "a-->b reads as a- -> b");
    }

    /// A sequence that fits a phone has no narrow drawing; a wider one gets
    /// one with its columns squeezed to a phone; one with no participant
    /// draws nothing.
    #[test]
    fn narrow_drawing_where_the_wide_one_wont_fit() {
        let small = render_sequence("a -> a: self\n", "sequence").expect("drawn");
        assert!(small.narrow.is_none());
        assert_eq!(small.label, "a to a: self");

        let wide = render_sequence("a -> b: hi\nb --> c: hello\n", "sequence label=\"three\"")
            .expect("drawn");
        assert_eq!(wide.label, "three");
        let narrow = wide.narrow.as_ref().expect("a narrow drawing");
        assert!(f64::from(wide.wide.width) > PHONE, "{}", wide.wide.width);
        assert!(f64::from(narrow.width) <= PHONE, "{}", narrow.width);

        let gapped = render_sequence("a -> b\n", "sequence gap=400").expect("drawn");
        assert!(gapped.wide.width > wide.wide.width);

        assert!(render_sequence("# nothing\n", "sequence").is_none());
    }

    /// A drawing is a whole SVG in explain's tokens, so it themes and
    /// rasterises like a hand-drawn figure, with its text escaped.
    #[test]
    fn drawing_rasterises_in_both_themes() {
        let seq = render_sequence(
            "a: <Client> & co\na -> b: req {brass}\nb --> a: ok\nb -> b: loop\nnote a,b: across\n",
            "sequence",
        )
        .expect("drawn");
        let svg = &seq.wide.svg;
        assert!(svg.contains("&lt;Client&gt; &amp; co"), "{svg}");
        assert!(svg.contains("stroke-dasharray"), "{svg}");
        let palette = crate::explain_palette::Palette::default();
        let fonts = crate::review_figures::figure_fonts();
        for dark in [false, true] {
            let themed = crate::review_figures::substitute_vars(svg, &palette, dark);
            assert!(!themed.contains("var("), "{themed}");
            let raster =
                crate::review_figures::rasterize_svg(&themed, &fonts, 1.0).expect("rasterised");
            assert_eq!(raster.size, egui::vec2(seq.wide.width, seq.wide.height));
            assert!(raster.image.pixels.iter().any(|p| p.a() > 0));
        }
    }
}
