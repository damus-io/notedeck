use crate::{actionbar::NoteAction, gif::GifStateMap, ui};
use egui::{Label, RichText, Sense};
use nostrdb::{Ndb, Note, NoteReply, Transaction};
use notedeck::{MediaCache, NoteCache, UrlMimes};

#[must_use = "Please handle the resulting note action"]
pub fn reply_desc(
    ui: &mut egui::Ui,
    txn: &Transaction,
    note_reply: &NoteReply,
    ndb: &Ndb,
    img_cache: &mut MediaCache,
    urls: &mut UrlMimes,
    note_cache: &mut NoteCache,
    gifs: &mut GifStateMap,
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
    let note_link = |ui: &mut egui::Ui,
                     note_cache: &mut NoteCache,
                     img_cache: &mut MediaCache,
                     urls: &mut UrlMimes,
                     gifs: &mut GifStateMap,
                     text: &str,
                     note: &Note<'_>| {
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
                ui::NoteView::new(ndb, note_cache, img_cache, urls, gifs, note)
                    .actionbar(false)
                    .wide(true)
                    .show(ui);
            });
        }
    };

    ui.add(Label::new(RichText::new("replying to").size(size).color(color)).selectable(selectable));

    let reply = note_reply.reply()?;

    let reply_note = if let Ok(reply_note) = ndb.get_note_by_id(txn, reply.id) {
        reply_note
    } else {
        ui.add(Label::new(RichText::new("a note").size(size).color(color)).selectable(selectable));
        return None;
    };

    if note_reply.is_reply_to_root() {
        // We're replying to the root, let's show this
        let action = ui::Mention::new(ndb, img_cache, gifs, txn, reply_note.pubkey())
            .size(size)
            .selectable(selectable)
            .show(ui)
            .inner;

        if action.is_some() {
            note_action = action;
        }

        ui.add(Label::new(RichText::new("'s").size(size).color(color)).selectable(selectable));

        note_link(ui, note_cache, img_cache, urls, gifs, "thread", &reply_note);
    } else if let Some(root) = note_reply.root() {
        // replying to another post in a thread, not the root

        if let Ok(root_note) = ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // simply "replying to bob's note" when replying to bob in his thread
                let action = ui::Mention::new(ndb, img_cache, gifs, txn, reply_note.pubkey())
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

                note_link(ui, note_cache, img_cache, urls, gifs, "note", &reply_note);
            } else {
                // replying to bob in alice's thread

                let action = ui::Mention::new(ndb, img_cache, gifs, txn, reply_note.pubkey())
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

                note_link(ui, note_cache, img_cache, urls, gifs, "note", &reply_note);

                ui.add(
                    Label::new(RichText::new("in").size(size).color(color)).selectable(selectable),
                );

                let action = ui::Mention::new(ndb, img_cache, gifs, txn, root_note.pubkey())
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

                note_link(ui, note_cache, img_cache, urls, gifs, "thread", &root_note);
            }
        } else {
            let action = ui::Mention::new(ndb, img_cache, gifs, txn, reply_note.pubkey())
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
