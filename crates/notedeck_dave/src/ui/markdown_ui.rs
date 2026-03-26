//! Markdown rendering for assistant messages using egui.

use std::borrow::Cow;

use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, RichText, TextFormat, Ui};
use md_stream::{
    parse_inline, CodeBlock, InlineElement, InlineStyle, ListItem, MdElement, Partial, PartialKind,
    Span,
};

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

/// Render all parsed markdown elements plus any partial state.
pub fn render_assistant_message(
    elements: &[MdElement],
    partial: Option<&Partial>,
    buffer: &str,
    ui: &mut Ui,
) {
    let theme = MdTheme::from_visuals(ui.visuals());

    ui.vertical(|ui| {
        for element in elements {
            render_element(element, &theme, buffer, ui);
        }

        // Render partial (speculative) content for immediate feedback
        if let Some(partial) = partial {
            render_partial(partial, &theme, buffer, ui);
        }
    });
}

fn render_element(element: &MdElement, theme: &MdTheme, buffer: &str, ui: &mut Ui) {
    match element {
        MdElement::Heading { level, content } => {
            let size = theme.heading_sizes[(*level as usize).saturating_sub(1).min(5)];
            ui.add(
                egui::Label::new(RichText::new(content.resolve(buffer)).size(size).strong()).wrap(),
            );
            ui.add_space(4.0);
        }

        MdElement::Paragraph(inlines) => {
            render_wrapped_inlines(inlines, theme, buffer, ui);
            ui.add_space(8.0);
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
                .stroke(egui::Stroke::new(2.0, theme.blockquote_border))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    for elem in nested {
                        render_element(elem, theme, buffer, ui);
                    }
                });
            ui.add_space(8.0);
        }

        MdElement::UnorderedList(items) => {
            for item in items {
                render_list_item(item, "\u{2022}", theme, buffer, ui);
            }
            ui.add_space(8.0);
        }

        MdElement::OrderedList { start, items } => {
            for (i, item) in items.iter().enumerate() {
                let marker = format!("{}.", start + i as u32);
                render_list_item(item, &marker, theme, buffer, ui);
            }
            ui.add_space(8.0);
        }

        MdElement::Table { headers, rows } => {
            render_table(headers, rows, theme, buffer, ui);
        }

        MdElement::ThematicBreak => {
            ui.separator();
            ui.add_space(8.0);
        }

        MdElement::Text(span) => {
            ui.label(span.resolve(buffer));
        }
    }
}

/// A link embedded in a LayoutJob, tracked by byte range so we can detect
/// clicks after the label is rendered.
struct InlineLink {
    /// Byte range in the LayoutJob text.
    byte_range: std::ops::Range<usize>,
    /// URL to open on click.
    url: String,
}

/// Flush a LayoutJob as a wrapped label if it has any content.
/// When `links` is non-empty, the label is made clickable and any click on a
/// link byte range opens the corresponding URL.
fn flush_job(job: &mut LayoutJob, links: &mut Vec<InlineLink>, ui: &mut Ui) {
    if job.text.is_empty() {
        return;
    }

    // Keep a small headroom to avoid subpixel right-edge spill at narrow widths.
    job.wrap.max_width = (ui.available_width() - 1.0).max(0.0);

    if links.is_empty() {
        ui.add(egui::Label::new(std::mem::take(job)).wrap());
    } else {
        // Layout the job manually so we get the galley for hit-testing links.
        let galley = ui.fonts(|f| f.layout_job(std::mem::take(job)));
        let desired_size = egui::vec2(ui.available_width().min(galley.size().x), galley.size().y);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
        ui.painter()
            .galley(rect.min, galley.clone(), ui.visuals().text_color());

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let local_pos = pos - rect.min;
                let offset = galley.cursor_from_pos(local_pos).pcursor.offset;
                for link in links.iter() {
                    if link.byte_range.contains(&offset) {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&link.url));
                        break;
                    }
                }
            }
        }

        if response.hovered() {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                let local_pos = pos - rect.min;
                let offset = galley.cursor_from_pos(local_pos).pcursor.offset;
                if links.iter().any(|l| l.byte_range.contains(&offset)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }

        links.clear();
    }
}

/// Insert invisible wrap opportunities after code-like separators without
/// changing how normal prose wraps.
/// Returns `true` for characters that act as word separators in code-like
/// tokens (paths, identifiers, URLs).
fn is_wrap_separator(ch: char) -> bool {
    matches!(ch, '_' | '.' | '-' | '/' | '\\' | ':')
}

fn soft_wrap_text(text: &str) -> Cow<'_, str> {
    const SOFT_WRAP_MIN_TOKEN_CHARS: usize = 24;

    if text.len() < SOFT_WRAP_MIN_TOKEN_CHARS {
        return Cow::Borrowed(text);
    }

    let mut wrapped = String::with_capacity(text.len());
    let mut token = String::new();
    let mut changed = false;

    let flush_token = |token: &mut String, wrapped: &mut String, changed: &mut bool| {
        if token.is_empty() {
            return;
        }

        let token_len = token.chars().count();
        let has_separator = token.chars().any(is_wrap_separator);

        if token_len >= SOFT_WRAP_MIN_TOKEN_CHARS && has_separator {
            for ch in token.chars() {
                wrapped.push(ch);
                if is_wrap_separator(ch) {
                    wrapped.push('\u{200B}');
                    *changed = true;
                }
            }
        } else {
            wrapped.push_str(token);
        }

        token.clear();
    };

    for ch in text.chars() {
        if ch.is_whitespace() {
            flush_token(&mut token, &mut wrapped, &mut changed);
            wrapped.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush_token(&mut token, &mut wrapped, &mut changed);

    if changed {
        Cow::Owned(wrapped)
    } else {
        Cow::Borrowed(text)
    }
}

/// Append text to a layout job while preserving normal word wrapping and
/// enabling breaks inside long code-like identifiers and paths.
fn append_wrapped_text(job: &mut LayoutJob, text: &str, format: TextFormat) {
    let wrapped = soft_wrap_text(text);
    job.append(&wrapped, 0.0, format);
}

/// Render inline elements inside a width-constrained wrapper that prevents
/// `horizontal_wrapped` from expanding past the parent boundary.
fn render_wrapped_inlines(inlines: &[InlineElement], theme: &MdTheme, buffer: &str, ui: &mut Ui) {
    let max_width = (ui.available_width() - 1.0).max(0.0);
    ui.allocate_ui_with_layout(
        egui::vec2(max_width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_max_width(max_width);
            ui.horizontal_wrapped(|ui| {
                render_inlines(inlines, theme, buffer, ui);
            });
        },
    );
}

fn render_inlines(inlines: &[InlineElement], theme: &MdTheme, buffer: &str, ui: &mut Ui) {
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
        strikethrough: egui::Stroke::new(1.0, text_color),
        ..Default::default()
    };

    // Use the named "bold" font family (OnestBold) if available, otherwise
    // fall back to Proportional. Keeping bold text inside the LayoutJob
    // instead of flushing it as a separate ui.label() prevents
    // horizontal_wrapped layout expansion that shifts content off-screen.
    let bold_font = if ui.fonts(|f| f.families().contains(&FontFamily::Name("bold".into()))) {
        FontFamily::Name("bold".into())
    } else {
        FontFamily::Proportional
    };

    let bold_fmt = TextFormat {
        font_id: FontId::new(font_size, bold_font.clone()),
        color: text_color,
        ..Default::default()
    };

    let bold_italic_fmt = TextFormat {
        font_id: FontId::new(font_size, bold_font),
        color: text_color,
        italics: true,
        ..Default::default()
    };

    let link_fmt = TextFormat {
        font_id: FontId::new(font_size, FontFamily::Proportional),
        color: theme.link_color,
        underline: egui::Stroke::new(1.0, theme.link_color),
        ..Default::default()
    };

    let mut job = LayoutJob::default();
    let mut links: Vec<InlineLink> = Vec::new();

    for inline in inlines {
        match inline {
            InlineElement::Text(span) => {
                append_wrapped_text(&mut job, span.resolve(buffer), text_fmt.clone());
            }

            InlineElement::Code(span) => {
                append_wrapped_text(&mut job, span.resolve(buffer), code_fmt.clone());
            }

            InlineElement::Styled { style, content } => {
                let text = content.resolve(buffer);
                match style {
                    InlineStyle::Italic => {
                        append_wrapped_text(&mut job, text, italic_fmt.clone());
                    }
                    InlineStyle::Strikethrough => {
                        append_wrapped_text(&mut job, text, strikethrough_fmt.clone());
                    }
                    InlineStyle::Bold => {
                        append_wrapped_text(&mut job, text, bold_fmt.clone());
                    }
                    InlineStyle::BoldItalic => {
                        append_wrapped_text(&mut job, text, bold_italic_fmt.clone());
                    }
                }
            }

            InlineElement::Link { text, url } => {
                let link_text = text.resolve(buffer);
                let start = job.text.len();
                append_wrapped_text(&mut job, link_text, link_fmt.clone());
                let end = job.text.len();
                links.push(InlineLink {
                    byte_range: start..end,
                    url: url.resolve(buffer).to_owned(),
                });
            }

            InlineElement::Image { alt, url } => {
                let alt_text = format!("[Image: {}]", alt.resolve(buffer));
                let start = job.text.len();
                append_wrapped_text(&mut job, &alt_text, link_fmt.clone());
                let end = job.text.len();
                links.push(InlineLink {
                    byte_range: start..end,
                    url: url.resolve(buffer).to_owned(),
                });
            }

            InlineElement::LineBreak => {
                flush_job(&mut job, &mut links, ui);
                ui.end_row();
            }
        }
    }

    flush_job(&mut job, &mut links, ui);
}

/// Sand-themed syntax highlighting colors (warm, Claude-Code-esque palette)
pub(crate) struct SandCodeTheme {
    comment: Color32,
    keyword: Color32,
    literal: Color32,
    string: Color32,
    punctuation: Color32,
    plain: Color32,
}

impl SandCodeTheme {
    pub(crate) fn from_visuals(visuals: &egui::Visuals) -> Self {
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

    pub(crate) fn format(&self, token: SandToken, font_id: &FontId) -> TextFormat {
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
pub(crate) enum SandToken {
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
pub(crate) fn tokenize_code<'a>(code: &'a str, language: &str) -> Vec<(SandToken, &'a str)> {
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
        append_wrapped_text(&mut job, text, theme.format(token, &font_id));
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
            let mut layout_job = highlight_sand(content, lang, ui);
            layout_job.wrap.max_width = (ui.available_width() - 1.0).max(0.0);
            ui.add(egui::Label::new(layout_job).wrap());
        });
    ui.add_space(8.0);
}

fn render_list_item(item: &ListItem, marker: &str, theme: &MdTheme, buffer: &str, ui: &mut Ui) {
    let row_width = ui.available_width();
    ui.horizontal_top(|ui| {
        let marker_response = ui.label(RichText::new(marker).weak());
        let content_width =
            (row_width - marker_response.rect.width() - ui.spacing().item_spacing.x).max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(content_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(content_width);
                ui.horizontal_wrapped(|ui| {
                    render_inlines(&item.content, theme, buffer, ui);
                });
                // Render nested list if present
                if let Some(nested) = &item.nested {
                    ui.indent("nested", |ui| {
                        render_element(nested, theme, buffer, ui);
                    });
                }
            },
        );
    });
}

fn render_table(headers: &[Span], rows: &[Vec<Span>], theme: &MdTheme, buffer: &str, ui: &mut Ui) {
    use egui_extras::{Column, TableBuilder};

    let num_cols = headers.len();
    if num_cols == 0 {
        return;
    }

    let cell_padding = egui::Margin::symmetric(8, 4);

    // Use first header's byte offset as id_salt so multiple tables don't clash
    let salt = headers.first().map_or(0, |h| h.start);

    // Constrain table columns to fit within available width — tables must never
    // overflow their container. Account for inter-column spacing that TableBuilder
    // adds between columns (item_spacing.x per gap).
    let table_width = ui.available_width();
    let spacing = ui.spacing().item_spacing.x;
    let total_spacing = spacing * (num_cols - 1) as f32;
    let col_width = ((table_width - total_spacing) / num_cols as f32).max(20.0);

    let mut builder = TableBuilder::new(ui)
        .id_salt(salt)
        .vscroll(false)
        .auto_shrink([false, false]);
    for _ in 0..num_cols {
        builder = builder.column(Column::initial(col_width).clip(true).resizable(true));
    }

    let header_bg = theme.code_bg;

    builder
        .header(28.0, |mut header| {
            for h in headers {
                header.col(|ui| {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, header_bg);
                    egui::Frame::NONE.inner_margin(cell_padding).show(ui, |ui| {
                        ui.strong(h.resolve(buffer));
                    });
                });
            }
        })
        .body(|mut body| {
            for row in rows {
                body.row(28.0, |mut table_row| {
                    for i in 0..num_cols {
                        table_row.col(|ui| {
                            egui::Frame::NONE.inner_margin(cell_padding).show(ui, |ui| {
                                if let Some(cell) = row.get(i) {
                                    ui.label(cell.resolve(buffer));
                                }
                            });
                        });
                    }
                });
            }
        });
    ui.add_space(8.0);
}

fn render_partial(partial: &Partial, theme: &MdTheme, buffer: &str, ui: &mut Ui) {
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
                    let mut layout_job = highlight_sand(content, lang, ui);
                    layout_job.wrap.max_width = (ui.available_width() - 1.0).max(0.0);
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

        _ => {
            let inlines = parse_inline(content, partial.content_start);
            render_wrapped_inlines(&inlines, theme, buffer, ui);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::ParsedMarkdown;
    use egui_kittest::{kittest::Queryable, Harness};
    use md_stream::{InlineElement, Span};

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
                render_inlines(&inlines, &theme, buffer, ui);
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
    fn inline_link_stays_on_same_row_as_surrounding_text_when_width_allows() {
        // Links are now embedded in a single LayoutJob galley alongside their
        // surrounding text, so they inherently stay on the same row.  Verify
        // the rendered content fits within a single line height by checking the
        // container node bounds.
        let markdown = "Renderer-level in [markdown_ui.rs](https://example.com): keep this inline.";
        let parsed = ParsedMarkdown::parse(markdown);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 200.0))
            .build_ui(move |ui| {
                ui.allocate_ui(egui::vec2(800.0, 200.0), |ui| {
                    render_assistant_message(&parsed.elements, None, &parsed.source, ui);
                });
            });

        harness.run();

        // The manually-painted galley doesn't expose per-text accessibility
        // nodes, but the container node's bounds reflect the laid-out size.
        // At 800px the whole sentence fits on one row (~20px high).
        // The full-app E2E test `full_app_inline_link_stays_on_same_row_when_width_allows`
        // provides deeper coverage of this behavior.
        let container = harness.get_by(|node| {
            node.raw_bounds()
                .is_some_and(|b| b.width() > 100.0 && b.height() > 0.0)
        });
        let bounds = container.raw_bounds().expect("container bounds");
        assert!(
            bounds.height() < 40.0,
            "paragraph with inline link should render on a single row at 800px, got height {}",
            bounds.height()
        );
    }

    #[test]
    fn soft_wrap_text_preserves_normal_words() {
        assert_eq!(soft_wrap_text("plain words only"), "plain words only");
    }

    #[test]
    fn soft_wrap_text_adds_breaks_to_code_like_tokens() {
        assert_eq!(
            soft_wrap_text("restart_should_prefetch_newer_known_participant_relay_list_e2e"),
            "restart_\u{200b}should_\u{200b}prefetch_\u{200b}newer_\u{200b}known_\u{200b}participant_\u{200b}relay_\u{200b}list_\u{200b}e2e"
        );
        assert_eq!(
            soft_wrap_text("/Users/user/Documents/test/test_folder"),
            "/\u{200b}Users/\u{200b}user/\u{200b}Documents/\u{200b}test/\u{200b}test_\u{200b}folder"
        );
    }

    #[test]
    fn soft_wrap_text_keeps_short_code_like_tokens_unchanged() {
        assert_eq!(soft_wrap_text("messages_e2e.rs"), "messages_e2e.rs");
        assert_eq!(soft_wrap_text("markdown_ui.rs"), "markdown_ui.rs");
        assert_eq!(soft_wrap_text("relay-A/path"), "relay-A/path");
    }

    fn markdown_overflows(md: &str, width: f32) -> Vec<String> {
        let parsed = ParsedMarkdown::parse(md);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(width, 800.0))
            .build_ui(move |ui| {
                ui.allocate_ui(egui::vec2(width, 800.0), |ui| {
                    render_assistant_message(&parsed.elements, None, &parsed.source, ui);
                });
            });

        harness.run();

        // Collect bounds once, then derive min_x and check overflows.
        let all_bounds: Vec<_> = harness
            .query_all(egui_kittest::kittest::by())
            .filter_map(|node| {
                let bounds = node.raw_bounds()?;
                let label = node.label().map(|s| s.to_owned());
                Some((bounds, label))
            })
            .collect();

        let min_x = all_bounds
            .iter()
            .map(|(b, _)| b.x0)
            .fold(f64::INFINITY, f64::min);
        let min_x = if min_x.is_finite() { min_x } else { 0.0 };

        all_bounds
            .iter()
            .filter_map(|(bounds, label)| {
                if bounds.x0 < min_x - 4.0 || bounds.x1 > min_x + f64::from(width + 4.0) {
                    Some(format!(
                        "label={label:?} bounds=({:.1}, {:.1})-({:.1}, {:.1})",
                        bounds.x0, bounds.y0, bounds.x1, bounds.y1
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn plain_paragraph_stays_within_renderer_width_at_198px() {
        let overflows = markdown_overflows(
            "This is a plain paragraph without lists or code blocks. It should wrap inside the \
             allocated markdown renderer width even on a narrow viewport.",
            198.0,
        );
        assert!(
            overflows.is_empty(),
            "plain paragraph overflowed: {overflows:?}"
        );
    }

    #[test]
    fn emphasized_paragraph_followed_by_code_block_stays_within_renderer_width_at_198px() {
        let overflows = markdown_overflows(
            "Use **strong emphasis** before the example so the preceding paragraph exercises the \
             inline formatting path, then render the code block below without shifting the chat \
             layout.\n\n\
             ```rust\n\
             let builder = egui::Frame::default()\n\
                 .inner_margin(8.0)\n\
                 .corner_radius(4.0)\n\
                 .fill(ui.visuals().panel_fill);\n\
             ```\n\n\
             After the block, render one more **bold marker** to ensure normal inline content \
             still lines up with the rest of the conversation.",
            198.0,
        );
        assert!(
            overflows.is_empty(),
            "code block markdown overflowed at 198px: {overflows:?}"
        );
    }

    #[test]
    fn exact_repro_paragraph_stays_within_renderer_width_across_narrow_sizes() {
        let paragraph = "The Linux issue is fixed in messages_e2e.rs. \
             restart_should_prefetch_newer_known_participant_relay_list_e2e now waits until \
             sender_current has actually ingested the recipient’s relay-A DM relay list before \
             the first send, instead of relying on the fixed warmup alone.";

        for width in [240.0, 220.0, 210.0, 205.0, 200.0, 198.0] {
            let overflows = markdown_overflows(paragraph, width);
            assert!(
                overflows.is_empty(),
                "exact repro paragraph overflowed at {width}px: {overflows:?}"
            );
        }
    }

    #[test]
    fn ordered_list_repro_paragraph_stays_within_renderer_width_across_narrow_sizes() {
        let paragraph =
            "1. Medium: the restart E2E tests are not fully correct as written; at least one is \
             flaky because it waits on relay-side giftwrap counts without continuing to step the \
             sender that flushes outbound publishes. The helper at messages_e2e.rs only polls the \
             relay DB. In messages_e2e.rs, messages_e2e.rs, messages_e2e.rs, and \
             messages_e2e.rs, the test stops stepping sender_device before asserting the relay \
             count increase. I reproduced that flake directly: \
             same_account_device_restart_catches_up_from_existing_db_e2e failed once with \
             \"expected relay giftwrap count at least 2, actual 1\", then passed immediately on \
             rerun. The same pattern exists in the many-conversation restart variants too.";

        for width in [
            300.0, 280.0, 260.0, 240.0, 220.0, 210.0, 200.0, 198.0, 190.0, 180.0,
        ] {
            let overflows = markdown_overflows(paragraph, width);
            assert!(
                overflows.is_empty(),
                "ordered-list repro paragraph overflowed at {width}px: {overflows:?}"
            );
        }
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
}
