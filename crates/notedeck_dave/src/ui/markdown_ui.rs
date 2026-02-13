//! Markdown rendering for assistant messages using egui.

use egui::{Color32, RichText, Ui};
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
        Self {
            heading_sizes: [24.0, 20.0, 18.0, 16.0, 14.0, 12.0],
            code_bg: visuals.extreme_bg_color,
            code_text: visuals.text_color(),
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

        MdElement::ThematicBreak => {
            ui.separator();
            ui.add_space(8.0);
        }

        MdElement::Text(text) => {
            ui.label(text);
        }
    }
}

fn render_inlines(inlines: &[InlineElement], theme: &MdTheme, ui: &mut Ui) {
    for inline in inlines {
        match inline {
            InlineElement::Text(text) => {
                ui.label(text);
            }

            InlineElement::Styled { style, content } => {
                let rt = match style {
                    InlineStyle::Bold => RichText::new(content).strong(),
                    InlineStyle::Italic => RichText::new(content).italics(),
                    InlineStyle::BoldItalic => RichText::new(content).strong().italics(),
                    InlineStyle::Strikethrough => RichText::new(content).strikethrough(),
                };
                ui.label(rt);
            }

            InlineElement::Code(code) => {
                ui.label(
                    RichText::new(code)
                        .monospace()
                        .background_color(theme.code_bg),
                );
            }

            InlineElement::Link { text, url } => {
                ui.hyperlink_to(RichText::new(text).color(theme.link_color), url);
            }

            InlineElement::Image { alt, url } => {
                // Render as link for now; full image support can be added later
                ui.hyperlink_to(format!("[Image: {}]", alt), url);
            }

            InlineElement::LineBreak => {
                ui.end_row();
            }
        }
    }
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
