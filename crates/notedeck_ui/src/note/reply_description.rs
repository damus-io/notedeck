use egui::{Label, RichText, Sense};
use nostrdb::{NoteReply, Transaction};

use super::NoteOptions;
use crate::{jobs::JobsCache, note::NoteView, Mention};
use notedeck::{tr, NoteAction, NoteContext};

// Rich text segment types for internationalized rendering
#[derive(Debug, Clone)]
pub enum TextSegment {
    Plain(String),
    UserMention([u8; 32]),       // pubkey
    ThreadUserMention([u8; 32]), // pubkey
    NoteLink([u8; 32]),
    ThreadLink([u8; 32]),
}

// Helper function to parse i18n template strings with placeholders
fn parse_i18n_template(template: &str) -> Vec<TextSegment> {
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
                "user" => segments.push(TextSegment::UserMention([0; 32])),
                "thread_user" => segments.push(TextSegment::ThreadUserMention([0; 32])),
                "note" => segments.push(TextSegment::NoteLink([0; 32])),
                "thread" => segments.push(TextSegment::ThreadLink([0; 32])),
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
fn fill_template_data(
    mut segments: Vec<TextSegment>,
    reply_pubkey: &[u8; 32],
    reply_note_id: &[u8; 32],
    root_pubkey: Option<&[u8; 32]>,
    root_note_id: Option<&[u8; 32]>,
) -> Vec<TextSegment> {
    for segment in &mut segments {
        match segment {
            TextSegment::UserMention(pubkey) if *pubkey == [0; 32] => {
                *pubkey = *reply_pubkey;
            }
            TextSegment::ThreadUserMention(pubkey) if *pubkey == [0; 32] => {
                *pubkey = *root_pubkey.unwrap_or(reply_pubkey);
            }
            TextSegment::NoteLink(note_id) if *note_id == [0; 32] => {
                *note_id = *reply_note_id;
            }
            TextSegment::ThreadLink(note_id) if *note_id == [0; 32] => {
                *note_id = *root_note_id.unwrap_or(reply_note_id);
            }
            _ => {}
        }
    }

    segments
}

// Main rendering function for text segments
#[allow(clippy::too_many_arguments)]
fn render_text_segments(
    ui: &mut egui::Ui,
    segments: &[TextSegment],
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
                let action = Mention::new(note_context.ndb, note_context.img_cache, txn, pubkey)
                    .size(size)
                    .selectable(selectable)
                    .show(ui);

                if action.is_some() {
                    note_action = action;
                }
            }
            TextSegment::NoteLink(note_id) => {
                if let Ok(note) = note_context.ndb.get_note_by_id(txn, note_id) {
                    let r = ui.add(
                        Label::new(
                            RichText::new(tr!("note", "Link text for note references"))
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
                if let Ok(note) = note_context.ndb.get_note_by_id(txn, note_id) {
                    let r = ui.add(
                        Label::new(
                            RichText::new(tr!("thread", "Link text for thread references"))
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

    let segments = if note_reply.is_reply_to_root() {
        // Template: "replying to {user}'s {thread}"
        let template = tr!(
            "replying to {user}'s {thread}",
            "Template for replying to root thread",
            user = "{user}",
            thread = "{thread}"
        );
        let segments = parse_i18n_template(&template);
        fill_template_data(
            segments,
            reply_note.pubkey(),
            reply.id,
            None,
            Some(reply.id),
        )
    } else if let Some(root) = note_reply.root() {
        if let Ok(root_note) = note_context.ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // Template: "replying to {user}'s {note}"
                let template = tr!(
                    "replying to {user}'s {note}",
                    "Template for replying to user's note",
                    user = "{user}",
                    note = "{note}"
                );
                let segments = parse_i18n_template(&template);
                fill_template_data(segments, reply_note.pubkey(), reply.id, None, None)
            } else {
                // Template: "replying to {reply_user}'s {note} in {thread_user}'s {thread}"
                // This would need more sophisticated placeholder handling
                let template = tr!(
                    "replying to {user}'s {note} in {thread_user}'s {thread}",
                    "Template for replying to note in different user's thread",
                    user = "{user}",
                    note = "{note}",
                    thread_user = "{thread_user}",
                    thread = "{thread}"
                );
                let segments = parse_i18n_template(&template);
                fill_template_data(
                    segments,
                    reply_note.pubkey(),
                    reply.id,
                    Some(root_note.pubkey()),
                    Some(root.id),
                )
            }
        } else {
            // Template: "replying to {user} in someone's thread"
            let template = tr!(
                "replying to {user} in someone's thread",
                "Template for replying to user in unknown thread",
                user = "{user}"
            );
            let segments = parse_i18n_template(&template);
            fill_template_data(segments, reply_note.pubkey(), reply.id, None, None)
        }
    } else {
        // Fallback
        let template = tr!(
            "replying to {user}",
            "Fallback template for replying to user",
            user = "{user}"
        );
        let segments = parse_i18n_template(&template);
        fill_template_data(segments, reply_note.pubkey(), reply.id, None, None)
    };

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
