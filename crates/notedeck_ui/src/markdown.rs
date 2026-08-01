//! Markdown rendering using egui.
//!
//! Originally written for streaming assistant messages in `notedeck_dave`;
//! lives here so any crate (notebook text nodes, dave, ...) can reuse it.

use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, RichText, TextFormat, Ui};
use md_stream::{
    parse_inline, CodeBlock, InlineElement, InlineStyle, ListItem, MdElement, Partial, PartialKind,
    Span, StreamParser,
};
use nostrdb::Transaction;
use notedeck::AppContext;

/// Theme for markdown rendering, derived from egui visuals.
pub struct MdTheme {
    pub heading_sizes: [f32; 6],
    pub code_bg: Color32,
    pub code_text: Color32,
    pub link_color: Color32,
    pub blockquote_border: Color32,
    pub blockquote_bg: Color32,
}

impl MdTheme {
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        let bg = visuals.panel_fill;
        // Code bg: slightly lighter than panel background
        let code_bg = Color32::from_rgb(
            bg.r().saturating_add(25),
            bg.g().saturating_add(25),
            bg.b().saturating_add(25),
        );
        Self {
            heading_sizes: [24.0, 20.0, 18.0, 16.0, 14.0, 12.0],
            code_bg,
            code_text: Color32::from_rgb(0xD4, 0xA5, 0x74), // Muted amber/sand
            link_color: Color32::from_rgb(100, 149, 237),   // Cornflower blue
            blockquote_border: visuals.widgets.noninteractive.bg_stroke.color,
            blockquote_bg: visuals.faint_bg_color,
        }
    }
}

/// Parse and render a complete markdown string.
///
/// Convenience entry point for callers that have a finished string (e.g. a
/// notebook text node) rather than a streaming buffer.
pub fn render_markdown(text: &str, ui: &mut Ui) {
    let mut parser = StreamParser::new();
    parser.push(text);
    parser.finalize();
    let (elements, source) = parser.into_parts();
    render_parsed_markdown(&elements, None, &source, None, ui);
}

/// Render `source` as markdown with **interactive** GFM task-list checkboxes.
///
/// Like [`render_markdown`], but each `- [ ]`/`- [x]` checkbox is clickable;
/// clicking one flips its state byte in `source` (`[ ]` <-> `[x]`) in place and
/// returns `true` so the caller can persist the edit. Returns `false` when
/// nothing was toggled this frame. Both state chars (` ` and `x`) are one ASCII
/// byte, so a toggle never shifts later spans — multiple boxes coexist safely.
pub fn render_markdown_editable(source: &mut String, ui: &mut Ui) -> bool {
    let toggled = collect_checkbox_toggles(source, ui);
    apply_checkbox_toggles(source, &toggled)
}

/// Render `text` with interactive checkboxes and return the byte offsets of any
/// boxes toggled this frame. Offsets index a fresh parse of `text`, which for a
/// single push is byte-identical to `text` (so they index `text` too). Shared by
/// the plain ([`render_markdown_editable`]) and ref-aware
/// ([`render_markdown_with_refs_editable`]) editable renderers.
fn collect_checkbox_toggles(text: &str, ui: &mut Ui) -> Vec<usize> {
    let mut parser = StreamParser::new();
    parser.push(text);
    parser.finalize();
    let (elements, buffer) = parser.into_parts();

    let mut edits = CheckboxEdits::default();
    render_md_elements(&elements, None, &buffer, Some(&mut edits), None, ui);
    edits.toggled
}

/// Flip the single state byte (` ` <-> `x`) under each toggled checkbox in
/// `source`, returning whether anything changed. Each offset must index the ` `
/// or `x` of a `[ ]`/`[x]` marker in `source`; both states are one ASCII byte,
/// so a flip never shifts later offsets and multiple toggles compose safely.
fn apply_checkbox_toggles(source: &mut String, toggled: &[usize]) -> bool {
    if toggled.is_empty() {
        return false;
    }
    let mut bytes = std::mem::take(source).into_bytes();
    for &off in toggled {
        if let Some(b) = bytes.get_mut(off) {
            *b = if *b == b' ' { b'x' } else { b' ' };
        }
    }
    *source = String::from_utf8(bytes).expect("toggling an ascii state byte keeps utf8 valid");
    true
}

/// Collects task-list checkbox toggles during an editable render pass. Threaded
/// as `Option<&mut CheckboxEdits>`: `None` renders read-only (disabled) boxes,
/// `Some` renders enabled boxes and records the state-char byte offset of each
/// box clicked this frame (see [`md_stream::TaskMarker::state_offset`]).
#[derive(Default)]
struct CheckboxEdits {
    toggled: Vec<usize>,
}

/// Render markdown `text`, drawing an inline widget for any reference a
/// registered [`ReferenceParser`](notedeck::ReferenceParser) recognizes. Parses
/// `text` the same as [`render_markdown`], then renders through the ref-aware
/// inline path (see [`render_inlines`]): a reference flows *within* its
/// paragraph, each resolved to a nostr entity and drawn by the registered
/// [`KindRenderer`](notedeck::KindRenderer) for its kind. The built-in `nostr:`
/// parser keeps bech32 references rendering exactly as before; the common
/// reference-free body allocates nothing beyond the parse.
pub fn render_markdown_with_refs(ui: &mut Ui, ctx: &mut AppContext, text: &str) {
    let mut parser = StreamParser::new();
    parser.push(text);
    parser.finalize();
    let (elements, source) = parser.into_parts();
    render_parsed_markdown(&elements, None, &source, Some(ctx), ui);
}

/// Like [`render_markdown_with_refs`], but the GFM task-list checkboxes are
/// **interactive**: clicking one flips its state byte in `source` (`[ ]` <->
/// `[x]`) in place and returns `true` so the caller can persist the edit.
///
/// `source` is parsed once and rendered through the same ref-aware path, so a
/// toggle's byte offset indexes `source` directly — references render inline
/// without splitting the buffer, so there is no offset remapping to do (see
/// [`apply_checkbox_toggles`]).
pub fn render_markdown_with_refs_editable(
    ui: &mut Ui,
    ctx: &mut AppContext,
    source: &mut String,
) -> bool {
    let mut parser = StreamParser::new();
    parser.push(source);
    parser.finalize();
    let (elements, buffer) = parser.into_parts();

    let mut edits = CheckboxEdits::default();
    render_md_elements(&elements, None, &buffer, Some(&mut edits), Some(ctx), ui);
    apply_checkbox_toggles(source, &edits.toggled)
}

/// A reference located in a run of text: its byte range within the scanned
/// string and the [id](notedeck::ReferenceParser::id) of the parser that matched
/// it (so the match can be resolved without re-scanning).
///
/// A named struct (not a tuple) so the range and the parser id can't be
/// transposed at a call site. Holds no borrow of the text — just offsets and a
/// `'static` id — so it never pins the immutable scan borrow across the mutable
/// draw below.
struct RefMatch {
    /// Byte range of the whole matched reference within the scanned string.
    range: std::ops::Range<usize>,
    /// [`id`](notedeck::ReferenceParser::id) of the parser that matched.
    parser: &'static str,
}

/// The reference filling `text` in its entirety (ignoring surrounding
/// whitespace), or `None` if `text` is more than a lone reference.
///
/// This is the inline-code (backtick) rule: models habitually wrap a reference
/// in backticks (`` `board#word-word-word` ``), so a code span whose *whole*
/// content is one reference is drawn as its chip rather than as literal code.
/// Requiring the match to span the trimmed content keeps genuine code — a
/// `#define`, a `board#word-word-word` buried in a longer snippet — rendering as
/// code untouched.
fn whole_reference(text: &str, parsers: &notedeck::ReferenceParserRegistry) -> Option<RefMatch> {
    let trimmed = text.trim();
    let m = next_reference(trimmed, parsers)?;
    (m.range.start == 0 && m.range.end == trimmed.len()).then_some(m)
}

/// The leftmost reference in `text` recognized by any parser in `parsers`, or
/// `None` if `text` holds none.
///
/// Each parser owns its whole grammar via
/// [`find`](notedeck::ReferenceParser::find); this asks every parser for its next
/// match and keeps the earliest (longest on a tie) — the one shared primitive
/// both the read-only and editable scans walk with. Allocation-free: `find`
/// returns byte ranges into `text` and this holds no per-frame `Vec`.
fn next_reference(text: &str, parsers: &notedeck::ReferenceParserRegistry) -> Option<RefMatch> {
    let mut best: Option<RefMatch> = None;
    for parser in parsers.iter() {
        let Some(range) = parser.find(text) else {
            continue;
        };
        let better = match &best {
            Some(b) => {
                range.start < b.range.start
                    || (range.start == b.range.start && range.len() > b.range.len())
            }
            None => true,
        };
        if better {
            best = Some(RefMatch {
                range,
                parser: parser.id(),
            });
        }
    }
    best
}

/// Resolve `matched` via the parser registered under `parser_id` and draw the
/// resolved entity with the registered [`KindRenderer`](notedeck::KindRenderer)
/// for its kind, pushing any action it raises (e.g. a click asking to open the
/// entity) onto [`app_actions`](notedeck::AppContext::app_actions).
///
/// Returns `true` after flushing `job` and drawing the widget, or `false`
/// *without touching `job`* when the reference can't be resolved or its kind has
/// no renderer — so the caller renders `matched` as ordinary text, keeping a
/// loose `find` false-positive invisible rather than a broken chip.
fn draw_reference(
    job: &mut LayoutJob,
    ui: &mut Ui,
    ctx: &mut AppContext,
    parser_id: &str,
    matched: &str,
) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    // The registries are a `&'a` reference held in AppContext; borrow the parser
    // out of it and finish resolving before the mut reborrow `note_context()`
    // takes of ctx's other fields below.
    let Some(parser) = ctx.registries.reference_parsers.get(parser_id) else {
        return false;
    };
    let resolve_ctx = notedeck::ReferenceResolveCtx {
        ndb: ctx.ndb,
        txn: &txn,
        selected_account: Some(*ctx.accounts.selected_account_pubkey()),
    };
    let Some(resolved) = parser.resolve(matched, &resolve_ctx) else {
        return false;
    };
    let Ok(note) = ctx.ndb.get_note_by_id(&txn, resolved.note_id.bytes()) else {
        return false;
    };
    // TODO: per-kind default renderer id from settings (see "Settings UI" card).
    let Some(renderer) = ctx.registries.kind_renderers.default_for(note.kind(), None) else {
        return false;
    };
    // Committed to drawing: flush the pending text run so the widget breaks out
    // of it, then draw. `note_context` mut-borrows `ctx`, so scope it and pull the
    // owned action out before pushing onto `ctx.app_actions`.
    flush_job(job, ui);
    let req = notedeck::KindRenderRequest {
        txn: &txn,
        note: &note,
        context: notedeck::RenderContext::default(),
    };
    let action = {
        let mut note_context = ctx.note_context();
        renderer.render(ui, &mut note_context, &req).action
    };
    if let Some(action) = action {
        ctx.app_actions.push(action);
    }
    true
}

/// Render already-parsed markdown `elements` plus any streaming `partial` tail.
///
/// The shared read-only entry point behind [`render_markdown`] and
/// [`render_markdown_with_refs`], and the one Dave feeds its streaming
/// [`StreamParser`] output. Pass `Some(ctx)` to resolve inline `scheme:token`
/// references (see [`render_inlines`]); `None` renders references as plain text.
pub fn render_parsed_markdown(
    elements: &[MdElement],
    partial: Option<&Partial>,
    buffer: &str,
    ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    render_md_elements(elements, partial, buffer, None, ctx, ui);
}

/// Shared rendering core. `edits` is `None` for read-only renders and `Some`
/// for an editable pass (interactive checkboxes); see [`render_markdown_editable`].
fn render_md_elements(
    elements: &[MdElement],
    partial: Option<&Partial>,
    buffer: &str,
    mut edits: Option<&mut CheckboxEdits>,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    let theme = MdTheme::from_visuals(ui.visuals());

    ui.vertical(|ui| {
        for element in elements {
            render_element(
                element,
                &theme,
                buffer,
                edits.as_deref_mut(),
                ctx.as_deref_mut(),
                ui,
            );
        }

        // Render partial (speculative) content for immediate feedback. Partials
        // only arise while streaming, which is always a read-only render.
        if let Some(partial) = partial {
            render_partial(partial, &theme, buffer, ctx, ui);
        }
    });
}

fn render_element(
    element: &MdElement,
    theme: &MdTheme,
    buffer: &str,
    mut edits: Option<&mut CheckboxEdits>,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    match element {
        MdElement::Heading { level, content } => {
            let size = theme.heading_sizes[(*level as usize).saturating_sub(1).min(5)];
            ui.add(
                egui::Label::new(RichText::new(content.resolve(buffer)).size(size).strong()).wrap(),
            );
            ui.add_space(4.0);
        }

        MdElement::Paragraph(inlines) => {
            ui.horizontal_wrapped(|ui| {
                render_inlines(inlines, theme, buffer, ctx.as_deref_mut(), ui);
            });
            ui.add_space(notedeck::tokens::SPACING_SM);
        }

        MdElement::CodeBlock(CodeBlock { language, content }) => {
            render_code_block(
                language.map(|s| s.resolve(buffer)),
                content.resolve(buffer),
                theme,
                ui,
            );
        }

        MdElement::BlockQuote(nested) => {
            egui::Frame::default()
                .fill(theme.blockquote_bg)
                .stroke(egui::Stroke::new(
                    notedeck::tokens::STROKE_THICK,
                    theme.blockquote_border,
                ))
                .inner_margin(egui::Margin::symmetric(
                    notedeck::tokens::SPACING_SM as i8,
                    notedeck::tokens::SPACING_XS as i8,
                ))
                .show(ui, |ui| {
                    for elem in nested {
                        render_element(
                            elem,
                            theme,
                            buffer,
                            edits.as_deref_mut(),
                            ctx.as_deref_mut(),
                            ui,
                        );
                    }
                });
            ui.add_space(notedeck::tokens::SPACING_SM);
        }

        MdElement::UnorderedList(items) => {
            render_list_items(false, 1, items, theme, buffer, edits, ctx, ui);
        }

        MdElement::OrderedList { start, items } => {
            render_list_items(true, *start, items, theme, buffer, edits, ctx, ui);
        }

        MdElement::Table { headers, rows } => {
            render_table(headers, rows, theme, buffer, ui);
        }

        MdElement::ThematicBreak => {
            ui.separator();
            ui.add_space(notedeck::tokens::SPACING_SM);
        }

        MdElement::Text(span) => {
            ui.label(span.resolve(buffer));
        }
    }
}

/// Flush a LayoutJob as a wrapped label if it has any content.
fn flush_job(job: &mut LayoutJob, ui: &mut Ui) {
    if !job.text.is_empty() {
        job.wrap.max_width = ui.available_width();
        ui.add(egui::Label::new(std::mem::take(job)).wrap());
    }
}

/// Append `text` to `job`, splicing an inline reference widget wherever a
/// registered parser recognizes one: plain runs accumulate into `job`, and at
/// each resolved reference the job is flushed and the widget drawn in the
/// surrounding `horizontal_wrapped` — the same seam bold/link inlines flush
/// through. A match that doesn't resolve is appended back as ordinary text, so a
/// loose `find` false-positive is invisible. Walks `&str` subslices with no
/// per-frame allocation for the reference-free case.
fn append_text_with_refs(
    job: &mut LayoutJob,
    text: &str,
    fmt: &TextFormat,
    ctx: &mut AppContext,
    ui: &mut Ui,
) {
    let mut rest = text;
    while let Some(m) = next_reference(rest, &ctx.registries.reference_parsers) {
        job.append(&rest[..m.range.start], 0.0, fmt.clone());
        let matched = &rest[m.range.clone()];
        if !draw_reference(job, ui, ctx, m.parser, matched) {
            job.append(matched, 0.0, fmt.clone());
        }
        rest = &rest[m.range.end..];
    }
    job.append(rest, 0.0, fmt.clone());
}

/// Render a run of inline elements into the current `horizontal_wrapped` layout.
///
/// When `ctx` is `Some`, each [`InlineElement::Text`] span is scanned for a
/// reference any registered parser recognizes ([`next_reference`]) and a resolved
/// match is drawn inline as its kind widget ([`draw_reference`]) rather than plain
/// text, so a reference flows *within* the paragraph. `None` renders every span
/// as plain text (no registry to resolve against).
fn render_inlines(
    inlines: &[InlineElement],
    theme: &MdTheme,
    buffer: &str,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    // Inline runs carry their own spaces in the span text, and bold/link/image
    // runs are flushed as separate widgets mid-line. The default horizontal gap
    // would then show as a stray space on each side of every such run (e.g.
    // "for the  quarter , with"), so zero it — wrapping still breaks on rows.
    ui.spacing_mut().item_spacing.x = 0.0;

    let font_size = ui.style().text_styles[&egui::TextStyle::Body].size;
    let text_color = ui.visuals().text_color();

    let text_fmt = TextFormat {
        font_id: FontId::new(font_size, FontFamily::Proportional),
        color: text_color,
        ..Default::default()
    };

    let code_fmt = TextFormat {
        font_id: FontId::new(font_size, FontFamily::Monospace),
        color: theme.code_text,
        background: theme.code_bg,
        ..Default::default()
    };

    let italic_fmt = TextFormat {
        font_id: FontId::new(font_size, FontFamily::Proportional),
        color: text_color,
        italics: true,
        ..Default::default()
    };

    let strikethrough_fmt = TextFormat {
        font_id: FontId::new(font_size, FontFamily::Proportional),
        color: text_color,
        strikethrough: egui::Stroke::new(notedeck::tokens::STROKE_THIN, text_color),
        ..Default::default()
    };

    let mut job = LayoutJob::default();

    for inline in inlines {
        match inline {
            InlineElement::Text(span) => {
                let text = span.resolve(buffer);
                match ctx.as_deref_mut() {
                    Some(ctx) => append_text_with_refs(&mut job, text, &text_fmt, ctx, ui),
                    None => job.append(text, 0.0, text_fmt.clone()),
                }
            }

            InlineElement::Code(span) => {
                // A code span whose whole content is one reference is a
                // backtick-wrapped ref (models love `board#word-word-word`);
                // draw it as its chip. Anything else stays literal code.
                let text = span.resolve(buffer);
                let mut drawn = false;
                if let Some(ctx) = ctx.as_deref_mut() {
                    let trimmed = text.trim();
                    if let Some(m) = whole_reference(trimmed, &ctx.registries.reference_parsers) {
                        drawn = draw_reference(&mut job, ui, ctx, m.parser, trimmed);
                    }
                }
                if !drawn {
                    job.append(text, 0.0, code_fmt.clone());
                }
            }

            InlineElement::Styled { style, content } => {
                let text = content.resolve(buffer);
                match style {
                    InlineStyle::Italic => {
                        job.append(text, 0.0, italic_fmt.clone());
                    }
                    InlineStyle::Strikethrough => {
                        job.append(text, 0.0, strikethrough_fmt.clone());
                    }
                    InlineStyle::Bold | InlineStyle::BoldItalic => {
                        // TextFormat has no bold/weight — flush and render as separate label
                        flush_job(&mut job, ui);
                        let rt = if matches!(style, InlineStyle::BoldItalic) {
                            RichText::new(text).strong().italics()
                        } else {
                            RichText::new(text).strong()
                        };
                        ui.label(rt);
                    }
                }
            }

            InlineElement::Link { text, url } => {
                flush_job(&mut job, ui);
                ui.hyperlink_to(
                    RichText::new(text.resolve(buffer)).color(theme.link_color),
                    url.resolve(buffer),
                );
            }

            InlineElement::Image { alt, url } => {
                flush_job(&mut job, ui);
                ui.hyperlink_to(
                    format!("[Image: {}]", alt.resolve(buffer)),
                    url.resolve(buffer),
                );
            }

            InlineElement::LineBreak => {
                flush_job(&mut job, ui);
                ui.end_row();
            }
        }
    }

    flush_job(&mut job, ui);
}

/// Sand-themed syntax highlighting colors (warm, Claude-Code-esque palette)
pub struct SandCodeTheme {
    comment: Color32,
    keyword: Color32,
    literal: Color32,
    string: Color32,
    punctuation: Color32,
    plain: Color32,
}

impl SandCodeTheme {
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        if visuals.dark_mode {
            Self {
                comment: Color32::from_rgb(0x8A, 0x80, 0x72), // Warm gray-brown
                keyword: Color32::from_rgb(0xD4, 0xA5, 0x74), // Amber sand
                literal: Color32::from_rgb(0xC4, 0x8A, 0x6A), // Terra cotta
                string: Color32::from_rgb(0xC6, 0xB4, 0x6A),  // Golden wheat
                punctuation: Color32::from_rgb(0xA0, 0x96, 0x88), // Light sand
                plain: Color32::from_rgb(0xD5, 0xCE, 0xC4),   // Warm off-white
            }
        } else {
            Self {
                comment: Color32::from_rgb(0x8A, 0x7E, 0x6E), // Warm gray
                keyword: Color32::from_rgb(0x9A, 0x60, 0x2A), // Dark amber
                literal: Color32::from_rgb(0x8B, 0x4C, 0x30), // Dark terra cotta
                string: Color32::from_rgb(0x6B, 0x5C, 0x1A),  // Dark golden
                punctuation: Color32::from_rgb(0x6E, 0x64, 0x56), // Dark sand
                plain: Color32::from_rgb(0x3A, 0x35, 0x2E),   // Dark brown-black
            }
        }
    }

    pub fn format(&self, token: SandToken, font_id: &FontId) -> TextFormat {
        let color = match token {
            SandToken::Comment => self.comment,
            SandToken::Keyword => self.keyword,
            SandToken::Literal => self.literal,
            SandToken::String => self.string,
            SandToken::Punctuation => self.punctuation,
            SandToken::Plain => self.plain,
            SandToken::Whitespace => Color32::TRANSPARENT,
        };
        TextFormat::simple(font_id.clone(), color)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandToken {
    Comment,
    Keyword,
    Literal,
    String,
    Punctuation,
    Plain,
    Whitespace,
}

struct LangConfig<'a> {
    keywords: &'a [&'a str],
    double_slash_comments: bool,
    hash_comments: bool,
}

impl<'a> LangConfig<'a> {
    fn from_language(language: &str) -> Option<Self> {
        match language.to_lowercase().as_str() {
            "rs" | "rust" => Some(Self {
                keywords: &[
                    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                    "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop",
                    "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
                    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
                    "while",
                ],
                double_slash_comments: true,
                hash_comments: false,
            }),
            "c" | "h" | "hpp" | "cpp" | "c++" => Some(Self {
                keywords: &[
                    "auto",
                    "break",
                    "case",
                    "char",
                    "const",
                    "continue",
                    "default",
                    "do",
                    "double",
                    "else",
                    "enum",
                    "extern",
                    "false",
                    "float",
                    "for",
                    "goto",
                    "if",
                    "inline",
                    "int",
                    "long",
                    "namespace",
                    "new",
                    "nullptr",
                    "return",
                    "short",
                    "signed",
                    "sizeof",
                    "static",
                    "struct",
                    "switch",
                    "template",
                    "this",
                    "true",
                    "typedef",
                    "union",
                    "unsigned",
                    "using",
                    "virtual",
                    "void",
                    "volatile",
                    "while",
                    "class",
                    "public",
                    "private",
                    "protected",
                ],
                double_slash_comments: true,
                hash_comments: false,
            }),
            "py" | "python" => Some(Self {
                keywords: &[
                    "and", "as", "assert", "break", "class", "continue", "def", "del", "elif",
                    "else", "except", "False", "finally", "for", "from", "global", "if", "import",
                    "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                    "return", "True", "try", "while", "with", "yield",
                ],
                double_slash_comments: false,
                hash_comments: true,
            }),
            "toml" => Some(Self {
                keywords: &[],
                double_slash_comments: false,
                hash_comments: true,
            }),
            "bash" | "sh" | "zsh" => Some(Self {
                keywords: &[
                    "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until",
                    "do", "done", "in", "function", "return", "local", "export", "set", "unset",
                ],
                double_slash_comments: false,
                hash_comments: true,
            }),
            _ => None,
        }
    }
}

/// Tokenize source code into (token_type, text_slice) pairs.
/// Separated from rendering so it can be unit tested.
pub fn tokenize_code<'a>(code: &'a str, language: &str) -> Vec<(SandToken, &'a str)> {
    let Some(lang) = LangConfig::from_language(language) else {
        return vec![(SandToken::Plain, code)];
    };

    let mut tokens = Vec::new();
    let mut text = code;

    while !text.is_empty() {
        if (lang.double_slash_comments && text.starts_with("//"))
            || (lang.hash_comments && text.starts_with('#'))
        {
            let end = text.find('\n').unwrap_or(text.len());
            tokens.push((SandToken::Comment, &text[..end]));
            text = &text[end..];
        } else if text.starts_with('"') {
            let end = text[1..]
                .find('"')
                .map(|i| i + 2)
                .or_else(|| text.find('\n'))
                .unwrap_or(text.len());
            tokens.push((SandToken::String, &text[..end]));
            text = &text[end..];
        } else if text.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            let end = text[1..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map_or_else(|| text.len(), |i| i + 1);
            let word = &text[..end];
            let token = if lang.keywords.contains(&word) {
                SandToken::Keyword
            } else {
                SandToken::Literal
            };
            tokens.push((token, word));
            text = &text[end..];
        } else if text.starts_with(|c: char| c.is_ascii_whitespace()) {
            let end = text[1..]
                .find(|c: char| !c.is_ascii_whitespace())
                .map_or_else(|| text.len(), |i| i + 1);
            tokens.push((SandToken::Whitespace, &text[..end]));
            text = &text[end..];
        } else {
            let mut it = text.char_indices();
            it.next();
            let end = it.next().map_or(text.len(), |(idx, _)| idx);
            tokens.push((SandToken::Punctuation, &text[..end]));
            text = &text[end..];
        }
    }

    tokens
}

/// Simple syntax highlighter with sand-colored theme.
/// Supports Rust, C/C++, Python, TOML, bash, and falls back to plain text.
fn highlight_sand(code: &str, language: &str, ui: &Ui) -> LayoutJob {
    let theme = SandCodeTheme::from_visuals(ui.visuals());
    let font_id = ui
        .style()
        .override_font_id
        .clone()
        .unwrap_or_else(|| egui::TextStyle::Monospace.resolve(ui.style()));

    let mut job = LayoutJob::default();
    for (token, text) in tokenize_code(code, language) {
        job.append(text, 0.0, theme.format(token, &font_id));
    }
    job
}

fn render_code_block(language: Option<&str>, content: &str, theme: &MdTheme, ui: &mut Ui) {
    egui::Frame::default()
        .fill(theme.code_bg)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            if let Some(lang) = language {
                ui.label(RichText::new(lang).small().weak());
            }

            let lang = language.unwrap_or("text");
            let layout_job = highlight_sand(content, lang, ui);
            ui.add(egui::Label::new(layout_job).wrap());
        });
    ui.add_space(8.0);
}

/// Render a list's items with bullets (unordered) or incrementing numbers
/// (ordered, counting up from `start`). Shared by the completed-element and
/// streaming-partial paths.
#[allow(clippy::too_many_arguments)]
fn render_list_items(
    ordered: bool,
    start: u32,
    items: &[ListItem],
    theme: &MdTheme,
    buffer: &str,
    mut edits: Option<&mut CheckboxEdits>,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    for (i, item) in items.iter().enumerate() {
        if ordered {
            let marker = format!("{}.", start + i as u32);
            render_list_item(
                item,
                &marker,
                theme,
                buffer,
                edits.as_deref_mut(),
                ctx.as_deref_mut(),
                ui,
            );
        } else {
            render_list_item(
                item,
                "\u{2022}",
                theme,
                buffer,
                edits.as_deref_mut(),
                ctx.as_deref_mut(),
                ui,
            );
        }
    }
    ui.add_space(notedeck::tokens::SPACING_SM);
}

fn render_list_item(
    item: &ListItem,
    marker: &str,
    theme: &MdTheme,
    buffer: &str,
    mut edits: Option<&mut CheckboxEdits>,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    ui.horizontal(|ui| {
        // GFM task-list items render a checkbox in place of the bullet/number
        // marker; plain items keep their marker. In an editable pass the box is
        // enabled and a click records the toggle; otherwise it's read-only.
        if let Some(task) = item.checkbox {
            let mut checked = task.checked;
            match edits.as_deref_mut() {
                Some(edits) => {
                    if ui.add(egui::Checkbox::without_text(&mut checked)).changed() {
                        edits.toggled.push(task.state_offset());
                    }
                }
                None => {
                    ui.add_enabled(false, egui::Checkbox::without_text(&mut checked));
                }
            }
        } else {
            ui.label(RichText::new(marker).weak());
        }
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                render_inlines(&item.content, theme, buffer, ctx.as_deref_mut(), ui);
            });
            // Render nested list if present
            if let Some(nested) = &item.nested {
                ui.indent("nested", |ui| {
                    render_element(nested, theme, buffer, edits, ctx, ui);
                });
            }
        });
    });
}

fn render_table(headers: &[Span], rows: &[Vec<Span>], theme: &MdTheme, buffer: &str, ui: &mut Ui) {
    let num_cols = headers.len();
    if num_cols == 0 {
        return;
    }

    let cell_padding = egui::Margin::symmetric(8, 4);

    // Use first header's byte offset as id_salt so multiple tables don't clash
    let salt = headers.first().map_or(0, |h| h.start);

    // Cap column width to prevent overflow, but let Grid auto-size narrower.
    let table_width = ui.available_width();
    let spacing = ui.spacing().item_spacing.x;
    let total_spacing = spacing * (num_cols - 1) as f32;
    let max_col = ((table_width - total_spacing) / num_cols as f32).max(20.0);

    let header_bg = theme.code_bg;

    // Wrap in horizontal scroll so wide tables don't break layout on small screens.
    // Use egui::Grid so rows auto-size to fit wrapped text content
    // rather than truncating at a fixed height.
    egui::ScrollArea::horizontal()
        .id_salt(("md_table_scroll", salt))
        .show(ui, |ui| {
            egui::Grid::new(salt)
                .num_columns(num_cols)
                .max_col_width(max_col)
                .with_row_color(
                    move |row, _style| {
                        if row == 0 {
                            Some(header_bg)
                        } else {
                            None
                        }
                    },
                )
                .spacing([spacing, 0.0])
                .show(ui, |ui| {
                    // Header row
                    for h in headers {
                        egui::Frame::NONE.inner_margin(cell_padding).show(ui, |ui| {
                            ui.strong(h.resolve(buffer));
                        });
                    }
                    ui.end_row();

                    // Data rows
                    for row in rows {
                        for i in 0..num_cols {
                            egui::Frame::NONE.inner_margin(cell_padding).show(ui, |ui| {
                                if let Some(cell) = row.get(i) {
                                    ui.label(cell.resolve(buffer));
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    ui.add_space(8.0);
}

fn render_partial(
    partial: &Partial,
    theme: &MdTheme,
    buffer: &str,
    mut ctx: Option<&mut AppContext>,
    ui: &mut Ui,
) {
    // A streaming list keeps its completed items in `partial.kind` (its content
    // span stays empty), so render those for progressive feedback before the
    // empty-content guard below would bail out.
    if let PartialKind::List {
        ordered,
        start,
        items,
    } = &partial.kind
    {
        render_list_items(
            *ordered,
            *start,
            items,
            theme,
            buffer,
            None,
            ctx.as_deref_mut(),
            ui,
        );
        return;
    }

    let content = partial.content(buffer);
    if content.is_empty() {
        return;
    }

    match &partial.kind {
        PartialKind::CodeFence { language, .. } => {
            egui::Frame::default()
                .fill(theme.code_bg)
                .inner_margin(8.0)
                .corner_radius(4.0)
                .show(ui, |ui| {
                    let lang_str = language.map(|s| s.resolve(buffer));
                    if let Some(lang) = lang_str {
                        ui.label(RichText::new(lang).small().weak());
                    }

                    let lang = lang_str.unwrap_or("text");
                    let layout_job = highlight_sand(content, lang, ui);
                    ui.add(egui::Label::new(layout_job).wrap());
                    ui.label(RichText::new("_").weak());
                });
        }

        PartialKind::Heading { level } => {
            let size = theme.heading_sizes[(*level as usize).saturating_sub(1).min(5)];
            ui.add(egui::Label::new(RichText::new(content).size(size).strong()).wrap());
        }

        PartialKind::Table {
            headers,
            rows,
            seen_separator,
        } => {
            if *seen_separator {
                render_table(headers, rows, theme, buffer, ui);
            } else {
                ui.label(content);
            }
        }

        PartialKind::Paragraph => {
            // Parse inline elements from the partial content for proper formatting
            let inlines = parse_inline(content, partial.content_start);
            ui.horizontal_wrapped(|ui| {
                render_inlines(&inlines, theme, buffer, ctx.as_deref_mut(), ui);
            });
        }

        _ => {
            // Other partial kinds - parse inline elements too
            let inlines = parse_inline(content, partial.content_start);
            ui.horizontal_wrapped(|ui| {
                render_inlines(&inlines, theme, buffer, ctx, ui);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use md_stream::{InlineElement, Span};

    /// The editable render path with no `AppContext` — the `ctx`-less twin of
    /// [`render_markdown_with_refs_editable`]. Without a registry to resolve
    /// against, references stay plain text, but this drives the *same*
    /// single-buffer parse + [`render_md_elements`] the real function does, so a
    /// toggled checkbox's offset indexes `source` directly whether or not a
    /// reference precedes it.
    fn test_render_with_refs_editable(source: &mut String, ui: &mut Ui) -> bool {
        let mut parser = StreamParser::new();
        parser.push(source);
        parser.finalize();
        let (elements, buffer) = parser.into_parts();

        let mut edits = CheckboxEdits::default();
        render_md_elements(&elements, None, &buffer, Some(&mut edits), None, ui);
        apply_checkbox_toggles(source, &edits.toggled)
    }

    /// Helper: collect (token, text) pairs
    fn tokens<'a>(code: &'a str, lang: &str) -> Vec<(SandToken, &'a str)> {
        tokenize_code(code, lang)
    }

    /// Reassembled tokens must equal the original input (no bytes lost or duplicated)
    fn assert_roundtrip(code: &str, lang: &str) {
        let result: String = tokenize_code(code, lang)
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        assert_eq!(result, code, "roundtrip failed for lang={lang}");
    }

    // ---- Basic token classification ----

    #[test]
    fn test_rust_keyword() {
        let toks = tokens("fn main", "rust");
        assert_eq!(toks[0], (SandToken::Keyword, "fn"));
        assert_eq!(toks[1], (SandToken::Whitespace, " "));
        assert_eq!(toks[2], (SandToken::Literal, "main"));
    }

    #[test]
    fn test_rust_comment() {
        let toks = tokens("// hello", "rust");
        assert_eq!(toks, vec![(SandToken::Comment, "// hello")]);
    }

    #[test]
    fn test_rust_string() {
        let toks = tokens("\"hello world\"", "rust");
        assert_eq!(toks, vec![(SandToken::String, "\"hello world\"")]);
    }

    #[test]
    fn test_python_hash_comment() {
        let toks = tokens("# comment", "python");
        assert_eq!(toks, vec![(SandToken::Comment, "# comment")]);
    }

    #[test]
    fn test_python_keyword() {
        let toks = tokens("def foo", "py");
        assert_eq!(toks[0], (SandToken::Keyword, "def"));
    }

    #[test]
    fn test_punctuation() {
        let toks = tokens("();", "rust");
        assert_eq!(
            toks,
            vec![
                (SandToken::Punctuation, "("),
                (SandToken::Punctuation, ")"),
                (SandToken::Punctuation, ";"),
            ]
        );
    }

    #[test]
    fn test_underscore_identifier() {
        let toks = tokens("_foo_bar", "rust");
        assert_eq!(toks, vec![(SandToken::Literal, "_foo_bar")]);
    }

    // ---- Unsupported languages ----

    #[test]
    fn test_unknown_lang_plain() {
        let toks = tokens("anything goes here", "brainfuck");
        assert_eq!(toks, vec![(SandToken::Plain, "anything goes here")]);
    }

    #[test]
    fn test_text_lang_plain() {
        let toks = tokens("plain text", "text");
        assert_eq!(toks, vec![(SandToken::Plain, "plain text")]);
    }

    // ---- Edge cases for string indexing ----

    #[test]
    fn test_empty_input() {
        assert!(tokenize_code("", "rust").is_empty());
    }

    #[test]
    fn test_single_char_keyword() {
        // "if" is a keyword, "i" is not
        let toks = tokens("i", "rust");
        assert_eq!(toks, vec![(SandToken::Literal, "i")]);
    }

    #[test]
    fn test_unclosed_string() {
        // String that never closes — should consume to end of line or end of input
        let toks = tokens("\"unclosed", "rust");
        assert_eq!(toks, vec![(SandToken::String, "\"unclosed")]);
    }

    #[test]
    fn test_unclosed_string_with_newline() {
        let toks = tokens("\"unclosed\nnext", "rust");
        // Should stop the string at the newline
        assert_eq!(toks[0], (SandToken::String, "\"unclosed"));
    }

    #[test]
    fn test_empty_string() {
        let toks = tokens("\"\"", "rust");
        assert_eq!(toks, vec![(SandToken::String, "\"\"")]);
    }

    #[test]
    fn test_comment_at_end_no_newline() {
        let toks = tokens("// no newline", "rust");
        assert_eq!(toks, vec![(SandToken::Comment, "// no newline")]);
    }

    #[test]
    fn test_comment_with_newline() {
        let toks = tokens("// comment\ncode", "rust");
        assert_eq!(toks[0], (SandToken::Comment, "// comment"));
        assert_eq!(toks[1], (SandToken::Whitespace, "\n"));
        assert_eq!(toks[2], (SandToken::Literal, "code"));
    }

    #[test]
    fn test_multibyte_unicode_punctuation() {
        // Ensure multi-byte chars don't cause panics from byte indexing
        let toks = tokens("→", "rust");
        assert_eq!(toks, vec![(SandToken::Punctuation, "→")]);
    }

    #[test]
    fn test_hard_line_break_renders_on_a_new_row() {
        let buffer = "alpha  \nbeta";
        let inlines = vec![
            InlineElement::Text(Span::new(0, 5)),
            InlineElement::LineBreak,
            InlineElement::Text(Span::new(8, 12)),
        ];

        let mut harness = Harness::new_ui(move |ui| {
            let theme = MdTheme::from_visuals(ui.visuals());
            ui.horizontal_wrapped(|ui| {
                render_inlines(&inlines, &theme, buffer, None, ui);
            });
        });

        harness.run();

        let alpha = harness.get_by_label("alpha");
        let beta = harness.get_by_label("beta");
        let alpha_bounds = alpha.raw_bounds().expect("alpha bounds");
        let beta_bounds = beta.raw_bounds().expect("beta bounds");
        assert!(
            beta_bounds.y0 > alpha_bounds.y1,
            "hard line breaks should render the following text on a later row"
        );
    }

    #[test]
    fn test_mixed_unicode_and_ascii() {
        let code = "let x = «val»;";
        assert_roundtrip(code, "rust");
    }

    #[test]
    fn test_only_whitespace() {
        let toks = tokens("   \n\t", "rust");
        assert_eq!(toks, vec![(SandToken::Whitespace, "   \n\t")]);
    }

    #[test]
    fn test_only_punctuation() {
        let toks = tokens("()", "rust");
        assert_eq!(
            toks,
            vec![(SandToken::Punctuation, "("), (SandToken::Punctuation, ")"),]
        );
    }

    // ---- Roundtrip (no bytes lost) ----

    #[test]
    fn test_roundtrip_rust() {
        assert_roundtrip(
            "fn main() {\n    let x = \"hello\";\n    // done\n}",
            "rust",
        );
    }

    #[test]
    fn test_roundtrip_python() {
        assert_roundtrip("def foo():\n    # comment\n    return \"bar\"", "python");
    }

    #[test]
    fn test_roundtrip_cpp() {
        assert_roundtrip("#include <stdio.h>\nint main() { return 0; }", "cpp");
    }

    #[test]
    fn test_roundtrip_unknown() {
        assert_roundtrip("anything goes 🎉 here!", "unknown");
    }

    #[test]
    fn test_roundtrip_empty() {
        assert_roundtrip("", "rust");
    }

    #[test]
    fn test_roundtrip_bash() {
        assert_roundtrip(
            "#!/bin/bash\nif [ -f \"$1\" ]; then\n  echo \"exists\"\nfi",
            "bash",
        );
    }

    // ---- Multi-line code blocks ----

    #[test]
    fn test_multiline_rust() {
        let code = "use std::io;\n\nfn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}";
        assert_roundtrip(code, "rust");
        let toks = tokens(code, "rust");
        assert_eq!(toks[0], (SandToken::Keyword, "use"));
    }

    // ---- Language detection ----

    #[test]
    fn test_case_insensitive_language() {
        let toks = tokens("fn test", "Rust");
        assert_eq!(toks[0], (SandToken::Keyword, "fn"));

        let toks = tokens("def test", "PYTHON");
        assert_eq!(toks[0], (SandToken::Keyword, "def"));
    }

    // ---- Bash support ----

    #[test]
    fn test_bash_keywords() {
        let toks = tokens("if then fi", "bash");
        assert_eq!(toks[0], (SandToken::Keyword, "if"));
        assert_eq!(toks[2], (SandToken::Keyword, "then"));
        assert_eq!(toks[4], (SandToken::Keyword, "fi"));
    }

    #[test]
    fn test_bash_hash_comment() {
        let toks = tokens("# this is a comment", "sh");
        assert_eq!(toks, vec![(SandToken::Comment, "# this is a comment")]);
    }

    // ---- TOML ----

    #[test]
    fn test_toml_hash_comment() {
        let toks = tokens("# config", "toml");
        assert_eq!(toks, vec![(SandToken::Comment, "# config")]);
    }

    #[test]
    fn test_toml_key_value() {
        let toks = tokens("name = \"notedeck\"", "toml");
        assert_eq!(toks[0], (SandToken::Literal, "name"));
        // = is punctuation
        assert!(toks
            .iter()
            .any(|(t, s)| *t == SandToken::String && *s == "\"notedeck\""));
    }

    #[test]
    fn test_render_task_list_shows_items() {
        // End-to-end: a GFM task list parses and renders its item text without
        // panicking (guards the checkbox render path and the partial early-out).
        let md = "- [ ] todo item\n- [x] done item\n- plain item\n";
        let mut harness = Harness::new_ui(move |ui| {
            render_markdown(md, ui);
        });
        harness.run();

        // get_by_label panics if the label isn't present, so these assert it is.
        let _ = harness.get_by_label("todo item");
        let _ = harness.get_by_label("done item");
        let _ = harness.get_by_label("plain item");
    }

    #[test]
    fn test_render_ordered_list_shows_items() {
        let md = "1. alpha\n2. beta\n";
        let mut harness = Harness::new_ui(move |ui| {
            render_markdown(md, ui);
        });
        harness.run();
        let _ = harness.get_by_label("alpha");
        let _ = harness.get_by_label("beta");
    }

    #[test]
    fn test_editable_checkbox_click_checks_source() {
        use egui::accesskit::Role;
        use std::cell::RefCell;

        // Clicking an unchecked box rewrites the source `[ ]` -> `[x]`.
        let source = RefCell::new(String::from("- [ ] task\n"));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            render_markdown_editable(&mut s, ui);
        });
        harness.run();

        harness.get_by_role(Role::CheckBox).click();
        harness.run();

        assert_eq!(*source.borrow(), "- [x] task\n");
    }

    #[test]
    fn test_editable_checkbox_click_unchecks_source() {
        use egui::accesskit::Role;
        use std::cell::RefCell;

        // ...and clicking a checked box rewrites `[x]` -> `[ ]`.
        let source = RefCell::new(String::from("- [x] task\n"));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            render_markdown_editable(&mut s, ui);
        });
        harness.run();

        harness.get_by_role(Role::CheckBox).click();
        harness.run();

        assert_eq!(*source.borrow(), "- [ ] task\n");
    }

    #[test]
    fn test_editable_checkbox_click_targets_right_box() {
        use egui::accesskit::Role;
        use std::cell::RefCell;

        // With several boxes, only the clicked one flips; the byte offsets keep
        // the others (and surrounding text) untouched.
        let source = RefCell::new(String::from("- [ ] first\n- [ ] second\n- [ ] third\n"));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            render_markdown_editable(&mut s, ui);
        });
        harness.run();

        // Boxes render top-to-bottom in source order; click the second.
        harness
            .get_all_by_role(Role::CheckBox)
            .nth(1)
            .unwrap()
            .click();
        harness.run();

        assert_eq!(*source.borrow(), "- [ ] first\n- [x] second\n- [ ] third\n");
    }

    #[test]
    fn test_editable_render_no_click_leaves_source_unchanged() {
        use std::cell::RefCell;

        // Rendering without interacting must not rewrite the source.
        let source = RefCell::new(String::from("- [ ] a\n- [x] b\n"));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            assert!(!render_markdown_editable(&mut s, ui));
        });
        harness.run();
        assert_eq!(*source.borrow(), "- [ ] a\n- [x] b\n");
    }

    #[test]
    fn test_editable_with_refs_toggles_checkbox() {
        use egui::accesskit::Role;
        use std::cell::RefCell;

        // The ref-aware editable renderer must still toggle a checkbox that sits
        // in a plain (ref-free) segment of the source.
        let source = RefCell::new(String::from("- [ ] task\n"));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            test_render_with_refs_editable(&mut s, ui);
        });
        harness.run();
        harness.get_by_role(Role::CheckBox).click();
        harness.run();
        assert_eq!(*source.borrow(), "- [x] task\n");
    }

    #[test]
    fn test_editable_with_refs_maps_offset_past_a_ref() {
        use egui::accesskit::Role;
        use std::cell::RefCell;

        // A `nostr:` reference precedes the checkbox. The whole source is parsed
        // once, so the toggled byte offset indexes `source` directly — the ref
        // never splits the buffer or shifts later offsets. The bech32 here is a
        // throwaway token.
        let src = "see nostr:npub1xxx\n\n- [ ] after a ref\n";
        let source = RefCell::new(String::from(src));
        let mut harness = Harness::new_ui(|ui| {
            let mut s = source.borrow_mut();
            test_render_with_refs_editable(&mut s, ui);
        });
        harness.run();
        harness.get_by_role(Role::CheckBox).click();
        harness.run();
        assert_eq!(
            *source.borrow(),
            "see nostr:npub1xxx\n\n- [x] after a ref\n"
        );
    }

    /// A stub parser recognizing a bare `@handle` — a reference with *no* scheme
    /// prefix — so [`next_reference`] can be exercised with a second parser that
    /// owns an entirely different grammar than the built-in `nostr`.
    struct StubParser;
    impl notedeck::ReferenceParser for StubParser {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
            let at = text.find('@')?;
            let rest = &text[at + 1..];
            let len = rest
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            (len > 0).then_some(at..at + 1 + len)
        }
        fn resolve(
            &self,
            _matched: &str,
            _ctx: &notedeck::ReferenceResolveCtx,
        ) -> Option<notedeck::ResolvedRef> {
            None
        }
    }

    #[test]
    fn next_reference_matches_a_second_registered_parser() {
        let mut parsers = notedeck::ReferenceParserRegistry::default();
        parsers.register(Box::new(StubParser));

        // The built-in nostr parser still matches its `nostr:` + bech32 reference.
        let s = "see nostr:nevent1abc done";
        let m = next_reference(s, &parsers).unwrap();
        assert_eq!(m.parser, "nostr");
        assert_eq!(&s[m.range], "nostr:nevent1abc");

        // A newly registered parser is matched alongside it, with its own bare grammar.
        let s = "ping @alice please";
        let m = next_reference(s, &parsers).unwrap();
        assert_eq!(m.parser, "stub");
        assert_eq!(&s[m.range], "@alice");

        // When both appear, the leftmost wins regardless of registration order.
        let s = "hi @bob and nostr:note1two";
        let m = next_reference(s, &parsers).unwrap();
        assert_eq!(m.parser, "stub");
        assert_eq!(&s[m.range], "@bob");

        // Text with no recognized reference yields nothing.
        assert!(next_reference("just prose, no refs", &parsers).is_none());
    }

    #[test]
    fn whole_reference_gates_on_the_full_code_span() {
        let mut parsers = notedeck::ReferenceParserRegistry::default();
        parsers.register(Box::new(StubParser));

        // A code span that *is* the reference (bare, or padded with the
        // whitespace `trim` drops) draws as a chip.
        let m = whole_reference("@alice", &parsers).unwrap();
        assert_eq!(m.parser, "stub");
        assert!(whole_reference("  @alice  ", &parsers).is_some());

        // A reference embedded in a longer code snippet stays literal code.
        assert!(whole_reference("ping @alice now", &parsers).is_none());
        assert!(whole_reference("@alice.handle", &parsers).is_none());
        // Ordinary code with no reference is untouched.
        assert!(whole_reference("let x = 1;", &parsers).is_none());
    }
}
