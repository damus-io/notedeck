use egui::{Label, RichText, Sense};
use nostrdb::{NoteReply, Transaction};

use super::NoteOptions;
use crate::{Mention, jobs::JobsCache, note::NoteView};
use notedeck::{NoteAction, NoteContext, tr};

// Rich text segment types for internationalized rendering
#[derive(Debug, Clone)]
pub enum TextSegment<'a> {
    Plain(String),
    UserMention(Option<&'a [u8; 32]>),       // pubkey
    ThreadUserMention(Option<&'a [u8; 32]>), // pubkey
    NoteLink(Option<&'a [u8; 32]>),
    ThreadLink(Option<&'a [u8; 32]>),
}

// Helper function to parse i18n template strings with placeholders
fn parse_i18n_template(template: &str) -> Vec<TextSegment<'_>> {
    let mut segments = Vec::new();
    let mut current_text = String::new();
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            // Save any accumulated plain text
            if !current_text.is_empty() {
                segments.push(TextSegment::Plain(current_text.clone()));
                current_text.clear();
            }

            // Parse placeholder
            let mut placeholder = String::new();
            for ch in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                placeholder.push(ch);
            }

            // Handle different placeholder types
            match placeholder.as_str() {
                // Placeholder values will be filled later.
                "user" => segments.push(TextSegment::UserMention(None)),
                "thread_user" => segments.push(TextSegment::ThreadUserMention(None)),
                "note" => segments.push(TextSegment::NoteLink(None)),
                "thread" => segments.push(TextSegment::ThreadLink(None)),
                _ => {
                    // Unknown placeholder, treat as plain text
                    current_text.push_str(&format!("{{{placeholder}}}"));
                }
            }
        } else {
            current_text.push(ch);
        }
    }

    // Add any remaining plain text
    if !current_text.is_empty() {
        segments.push(TextSegment::Plain(current_text));
    }

    segments
}

// Helper function to fill in the actual data for placeholders
fn fill_template_data<'a>(
    segments: &mut [TextSegment<'a>],
    reply_pubkey: &'a [u8; 32],
    reply_note_id: &'a [u8; 32],
    root_pubkey: Option<&'a [u8; 32]>,
    root_note_id: Option<&'a [u8; 32]>,
) {
    for segment in segments {
        match segment {
            TextSegment::UserMention(pubkey) => {
                if pubkey.is_none() {
                    *pubkey = Some(reply_pubkey);
                }
            }
            TextSegment::ThreadUserMention(pubkey) => {
                if pubkey.is_none() {
                    *pubkey = Some(root_pubkey.unwrap_or(reply_pubkey));
                }
            }
            TextSegment::NoteLink(note_id) => {
                if note_id.is_none() {
                    *note_id = Some(reply_note_id);
                }
            }
            TextSegment::ThreadLink(note_id) => {
                if note_id.is_none() {
                    *note_id = Some(root_note_id.unwrap_or(reply_note_id));
                }
            }
            TextSegment::Plain(_) => {}
        }
    }
}

// Main rendering function for text segments
#[allow(clippy::too_many_arguments)]
fn render_text_segments(
    ui: &mut egui::Ui,
    segments: &[TextSegment<'_>],
    txn: &Transaction,
    note_context: &mut NoteContext,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
    size: f32,
    selectable: bool,
) -> Option<NoteAction> {
    let mut note_action: Option<NoteAction> = None;
    let visuals = ui.visuals();
    let color = visuals.noninteractive().fg_stroke.color;
    let link_color = visuals.hyperlink_color;

    for segment in segments {
        match segment {
            TextSegment::Plain(text) => {
                ui.add(
                    Label::new(RichText::new(text).size(size).color(color)).selectable(selectable),
                );
            }
            TextSegment::UserMention(pubkey) | TextSegment::ThreadUserMention(pubkey) => {
                let action = Mention::new(
                    note_context.ndb,
                    note_context.img_cache,
                    txn,
                    pubkey.expect("expected pubkey"),
                )
                .size(size)
                .selectable(selectable)
                .show(ui);

                if action.is_some() {
                    note_action = action;
                }
            }
            TextSegment::NoteLink(note_id) => {
                if let Ok(note) = note_context
                    .ndb
                    .get_note_by_id(txn, note_id.expect("expected text segment note_id"))
                {
                    let r = ui.add(
                        Label::new(
                            RichText::new(tr!(
                                note_context.i18n,
                                "note",
                                "Link text for note references"
                            ))
                            .size(size)
                            .color(link_color),
                        )
                        .sense(Sense::click())
                        .selectable(selectable),
                    );

                    if r.clicked() {
                        // TODO: jump to note
                    }

                    if r.hovered() {
                        r.on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(400.0);
                            NoteView::new(note_context, &note, note_options, jobs)
                                .actionbar(false)
                                .wide(true)
                                .show(ui);
                        });
                    }
                }
            }
            TextSegment::ThreadLink(note_id) => {
                if let Ok(note) = note_context
                    .ndb
                    .get_note_by_id(txn, note_id.expect("expected text segment threadlink"))
                {
                    let r = ui.add(
                        Label::new(
                            RichText::new(tr!(
                                note_context.i18n,
                                "thread",
                                "Link text for thread references"
                            ))
                            .size(size)
                            .color(link_color),
                        )
                        .sense(Sense::click())
                        .selectable(selectable),
                    );

                    if r.clicked() {
                        // TODO: jump to note
                    }

                    if r.hovered() {
                        r.on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(400.0);
                            NoteView::new(note_context, &note, note_options, jobs)
                                .actionbar(false)
                                .wide(true)
                                .show(ui);
                        });
                    }
                }
            }
        }
    }

    note_action
}

#[must_use = "Please handle the resulting note action"]
#[profiling::function]
pub fn reply_desc(
    ui: &mut egui::Ui,
    txn: &Transaction,
    note_reply: &NoteReply,
    note_context: &mut NoteContext,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
) -> Option<NoteAction> {
    let size = 10.0;
    let selectable = false;

    let reply = note_reply.reply()?;

    let reply_note = if let Ok(reply_note) = note_context.ndb.get_note_by_id(txn, reply.id) {
        reply_note
    } else {
        // Handle case where reply note is not found
        let template = tr!(
            note_context.i18n,
            "replying to a note",
            "Fallback text when reply note is not found"
        );
        let segments = parse_i18n_template(&template);
        return render_text_segments(
            ui,
            &segments,
            txn,
            note_context,
            note_options,
            jobs,
            size,
            selectable,
        );
    };

    if note_reply.is_reply_to_root() {
        // Template: "replying to {user}'s {thread}"
        let template = tr!(
            note_context.i18n,
            "replying to {user}'s {thread}",
            "Template for replying to root thread",
            user = "{user}",
            thread = "{thread}"
        );
        let mut segments = parse_i18n_template(&template);
        fill_template_data(
            &mut segments,
            reply_note.pubkey(),
            reply.id,
            None,
            Some(reply.id),
        );
        render_text_segments(
            ui,
            &segments,
            txn,
            note_context,
            note_options,
            jobs,
            size,
            selectable,
        )
    } else if let Some(root) = note_reply.root() {
        if let Ok(root_note) = note_context.ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // Template: "replying to {user}'s {note}"
                let template = tr!(
                    note_context.i18n,
                    "replying to {user}'s {note}",
                    "Template for replying to user's note",
                    user = "{user}",
                    note = "{note}"
                );
                let mut segments = parse_i18n_template(&template);
                fill_template_data(&mut segments, reply_note.pubkey(), reply.id, None, None);
                render_text_segments(
                    ui,
                    &segments,
                    txn,
                    note_context,
                    note_options,
                    jobs,
                    size,
                    selectable,
                )
            } else {
                // Template: "replying to {reply_user}'s {note} in {thread_user}'s {thread}"
                // This would need more sophisticated placeholder handling
                let template = tr!(
                    note_context.i18n,
                    "replying to {user}'s {note} in {thread_user}'s {thread}",
                    "Template for replying to note in different user's thread",
                    user = "{user}",
                    note = "{note}",
                    thread_user = "{thread_user}",
                    thread = "{thread}"
                );
                let mut segments = parse_i18n_template(&template);
                fill_template_data(
                    &mut segments,
                    reply_note.pubkey(),
                    reply.id,
                    Some(root_note.pubkey()),
                    Some(root.id),
                );
                render_text_segments(
                    ui,
                    &segments,
                    txn,
                    note_context,
                    note_options,
                    jobs,
                    size,
                    selectable,
                )
            }
        } else {
            // Template: "replying to {user} in someone's thread"
            let template = tr!(
                note_context.i18n,
                "replying to {user} in someone's thread",
                "Template for replying to user in unknown thread",
                user = "{user}"
            );
            let mut segments = parse_i18n_template(&template);
            fill_template_data(&mut segments, reply_note.pubkey(), reply.id, None, None);
            render_text_segments(
                ui,
                &segments,
                txn,
                note_context,
                note_options,
                jobs,
                size,
                selectable,
            )
        }
    } else {
        // Fallback
        let template = tr!(
            note_context.i18n,
            "replying to {user}",
            "Fallback template for replying to user",
            user = "{user}"
        );
        let mut segments = parse_i18n_template(&template);
        fill_template_data(&mut segments, reply_note.pubkey(), reply.id, None, None);
        render_text_segments(
            ui,
            &segments,
            txn,
            note_context,
            note_options,
            jobs,
            size,
            selectable,
        )
    }
}
