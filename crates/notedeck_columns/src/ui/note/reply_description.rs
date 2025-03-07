use crate::{
    actionbar::NoteAction,
    ui::{self},
};
use egui::{Label, RichText, Sense};
use nostrdb::{Note, NoteReply, Transaction};

use super::contents::NoteContext;

#[must_use = "Please handle the resulting note action"]
pub fn reply_desc(
    ui: &mut egui::Ui,
    txn: &Transaction,
    note_reply: &NoteReply,
    note_context: &mut NoteContext,
) -> Option<NoteAction> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut note_action: Option<NoteAction> = None;
    let size = 10.0;
    let selectable = false;
    let visuals = ui.visuals();
    let color = visuals.noninteractive().fg_stroke.color;
    let link_color = visuals.hyperlink_color;

    // note link renderer helper
    let note_link =
        |ui: &mut egui::Ui, note_context: &mut NoteContext, text: &str, note: &Note<'_>| {
            let r = ui.add(
                Label::new(RichText::new(text).size(size).color(link_color))
                    .sense(Sense::click())
                    .selectable(selectable),
            );

            if r.clicked() {
                // TODO: jump to note
            }

            if r.hovered() {
                r.on_hover_ui_at_pointer(|ui| {
                    ui.set_max_width(400.0);
                    ui::NoteView::new(note_context, note)
                        .actionbar(false)
                        .wide(true)
                        .show(ui);
                });
            }
        };

    ui.add(Label::new(RichText::new("replying to").size(size).color(color)).selectable(selectable));

    let reply = note_reply.reply()?;

    let reply_note = if let Ok(reply_note) = note_context.ndb.get_note_by_id(txn, reply.id) {
        reply_note
    } else {
        ui.add(Label::new(RichText::new("a note").size(size).color(color)).selectable(selectable));
        return None;
    };

    if note_reply.is_reply_to_root() {
        // We're replying to the root, let's show this
        let action = ui::Mention::new(
            note_context.ndb,
            note_context.img_cache,
            txn,
            reply_note.pubkey(),
        )
        .size(size)
        .selectable(selectable)
        .show(ui)
        .inner;

        if action.is_some() {
            note_action = action;
        }

        ui.add(Label::new(RichText::new("'s").size(size).color(color)).selectable(selectable));

        note_link(ui, note_context, "thread", &reply_note);
    } else if let Some(root) = note_reply.root() {
        // replying to another post in a thread, not the root

        if let Ok(root_note) = note_context.ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // simply "replying to bob's note" when replying to bob in his thread
                let action = ui::Mention::new(
                    note_context.ndb,
                    note_context.img_cache,
                    txn,
                    reply_note.pubkey(),
                )
                .size(size)
                .selectable(selectable)
                .show(ui)
                .inner;

                if action.is_some() {
                    note_action = action;
                }

                ui.add(
                    Label::new(RichText::new("'s").size(size).color(color)).selectable(selectable),
                );

                note_link(ui, note_context, "note", &reply_note);
            } else {
                // replying to bob in alice's thread

                let action = ui::Mention::new(
                    note_context.ndb,
                    note_context.img_cache,
                    txn,
                    reply_note.pubkey(),
                )
                .size(size)
                .selectable(selectable)
                .show(ui)
                .inner;

                if action.is_some() {
                    note_action = action;
                }

                ui.add(
                    Label::new(RichText::new("'s").size(size).color(color)).selectable(selectable),
                );

                note_link(ui, note_context, "note", &reply_note);

                ui.add(
                    Label::new(RichText::new("in").size(size).color(color)).selectable(selectable),
                );

                let action = ui::Mention::new(
                    note_context.ndb,
                    note_context.img_cache,
                    txn,
                    root_note.pubkey(),
                )
                .size(size)
                .selectable(selectable)
                .show(ui)
                .inner;

                if action.is_some() {
                    note_action = action;
                }

                ui.add(
                    Label::new(RichText::new("'s").size(size).color(color)).selectable(selectable),
                );

                note_link(ui, note_context, "thread", &root_note);
            }
        } else {
            let action = ui::Mention::new(
                note_context.ndb,
                note_context.img_cache,
                txn,
                reply_note.pubkey(),
            )
            .size(size)
            .selectable(selectable)
            .show(ui)
            .inner;

            if action.is_some() {
                note_action = action;
            }

            ui.add(
                Label::new(RichText::new("in someone's thread").size(size).color(color))
                    .selectable(selectable),
            );
        }
    }

    note_action
}
