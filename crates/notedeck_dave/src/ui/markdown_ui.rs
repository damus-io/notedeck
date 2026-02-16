//! Markdown rendering for assistant messages using egui.

use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, RichText, TextFormat, Ui};
use md_stream::{
    parse_inline, CodeBlock, InlineElement, InlineStyle, ListItem, MdElement, Partial, PartialKind,
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
            link_color: Color32::from_rgb(100, 149, 237), // Cornflower blue
            blockquote_border: visuals.widgets.noninteractive.bg_stroke.color,
            blockquote_bg: visuals.faint_bg_color,
        }
    }
}

/// Render all parsed markdown elements plus any partial state.
pub fn render_assistant_message(elements: &[MdElement], partial: Option<&Partial>, ui: &mut Ui) {
    let theme = MdTheme::from_visuals(ui.visuals());

    ui.vertical(|ui| {
        for element in elements {
            render_element(element, &theme, ui);
        }

        // Render partial (speculative) content for immediate feedback
        if let Some(partial) = partial {
            render_partial(partial, &theme, ui);
        }
    });
}

fn render_element(element: &MdElement, theme: &MdTheme, ui: &mut Ui) {
    match element {
        MdElement::Heading { level, content } => {
            let size = theme.heading_sizes[(*level as usize).saturating_sub(1).min(5)];
            ui.add(egui::Label::new(RichText::new(content).size(size).strong()).wrap());
            ui.add_space(4.0);
        }

        MdElement::Paragraph(inlines) => {
            ui.horizontal_wrapped(|ui| {
                render_inlines(inlines, theme, ui);
            });
            ui.add_space(8.0);
        }

        MdElement::CodeBlock(CodeBlock { language, content }) => {
            render_code_block(language.as_deref(), content, theme, ui);
        }

        MdElement::BlockQuote(nested) => {
            egui::Frame::default()
                .fill(theme.blockquote_bg)
                .stroke(egui::Stroke::new(2.0, theme.blockquote_border))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    for elem in nested {
                        render_element(elem, theme, ui);
                    }
                });
            ui.add_space(8.0);
        }

        MdElement::UnorderedList(items) => {
            for item in items {
                render_list_item(item, "\u{2022}", theme, ui);
            }
            ui.add_space(8.0);
        }

        MdElement::OrderedList { start, items } => {
            for (i, item) in items.iter().enumerate() {
                let marker = format!("{}.", start + i as u32);
                render_list_item(item, &marker, theme, ui);
            }
            ui.add_space(8.0);
        }

        MdElement::Table { headers, rows } => {
            render_table(headers, rows, theme, ui);
        }

        MdElement::ThematicBreak => {
            ui.separator();
            ui.add_space(8.0);
        }

        MdElement::Text(text) => {
            ui.label(text);
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

fn render_inlines(inlines: &[InlineElement], theme: &MdTheme, ui: &mut Ui) {
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

    let mut job = LayoutJob::default();

    for inline in inlines {
        match inline {
            InlineElement::Text(text) => {
                job.append(text, 0.0, text_fmt.clone());
            }

            InlineElement::Code(code) => {
                job.append(code, 0.0, code_fmt.clone());
            }

            InlineElement::Styled { style, content } => match style {
                InlineStyle::Italic => {
                    job.append(content, 0.0, italic_fmt.clone());
                }
                InlineStyle::Strikethrough => {
                    job.append(content, 0.0, strikethrough_fmt.clone());
                }
                InlineStyle::Bold | InlineStyle::BoldItalic => {
                    // TextFormat has no bold/weight — flush and render as separate label
                    flush_job(&mut job, ui);
                    let rt = if matches!(style, InlineStyle::BoldItalic) {
                        RichText::new(content).strong().italics()
                    } else {
                        RichText::new(content).strong()
                    };
                    ui.label(rt);
                }
            },

            InlineElement::Link { text, url } => {
                flush_job(&mut job, ui);
                ui.hyperlink_to(RichText::new(text).color(theme.link_color), url);
            }

            InlineElement::Image { alt, url } => {
                flush_job(&mut job, ui);
                ui.hyperlink_to(format!("[Image: {}]", alt), url);
            }

            InlineElement::LineBreak => {
                job.append("\n", 0.0, text_fmt.clone());
            }
        }
    }

    flush_job(&mut job, ui);
}

fn render_code_block(language: Option<&str>, content: &str, theme: &MdTheme, ui: &mut Ui) {
    egui::Frame::default()
        .fill(theme.code_bg)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            // Language label if present
            if let Some(lang) = language {
                ui.label(RichText::new(lang).small().weak());
            }

            // Code content
            ui.add(
                egui::Label::new(RichText::new(content).monospace().color(theme.code_text)).wrap(),
            );
        });
    ui.add_space(8.0);
}

fn render_list_item(item: &ListItem, marker: &str, theme: &MdTheme, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(marker).weak());
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                render_inlines(&item.content, theme, ui);
            });
            // Render nested list if present
            if let Some(nested) = &item.nested {
                ui.indent("nested", |ui| {
                    render_element(nested, theme, ui);
                });
            }
        });
    });
}

fn render_table(headers: &[String], rows: &[Vec<String>], theme: &MdTheme, ui: &mut Ui) {
    use egui_extras::{Column, TableBuilder};

    let num_cols = headers.len();
    if num_cols == 0 {
        return;
    }

    let cell_padding = egui::Margin::symmetric(8, 4);

    let mut builder = TableBuilder::new(ui).vscroll(false);
    for _ in 0..num_cols {
        builder = builder.column(Column::auto().resizable(true));
    }

    let header_bg = theme.code_bg;

    builder
        .header(28.0, |mut header| {
            for h in headers {
                header.col(|ui| {
                    ui.painter()
                        .rect_filled(ui.max_rect(), 0.0, header_bg);
                    egui::Frame::NONE.inner_margin(cell_padding).show(ui, |ui| {
                        ui.strong(h);
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
                                    ui.label(cell);
                                }
                            });
                        });
                    }
                });
            }
        });
    ui.add_space(8.0);
}

fn render_partial(partial: &Partial, theme: &MdTheme, ui: &mut Ui) {
    let content = &partial.content;
    if content.is_empty() {
        return;
    }

    match &partial.kind {
        PartialKind::CodeFence { language, .. } => {
            // Show incomplete code block
            egui::Frame::default()
                .fill(theme.code_bg)
                .inner_margin(8.0)
                .corner_radius(4.0)
                .show(ui, |ui| {
                    if let Some(lang) = language {
                        ui.label(RichText::new(lang).small().weak());
                    }
                    ui.add(
                        egui::Label::new(RichText::new(content).monospace().color(theme.code_text))
                            .wrap(),
                    );
                    // Blinking cursor indicator would require animation; just show underscore
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
                render_table(headers, rows, theme, ui);
            } else {
                ui.label(content);
            }
        }

        PartialKind::Paragraph => {
            // Parse inline elements from the partial content for proper formatting
            let inlines = parse_inline(content);
            ui.horizontal_wrapped(|ui| {
                render_inlines(&inlines, theme, ui);
            });
        }

        _ => {
            // Other partial kinds - parse inline elements too
            let inlines = parse_inline(content);
            ui.horizontal_wrapped(|ui| {
                render_inlines(&inlines, theme, ui);
            });
        }
    }
}
