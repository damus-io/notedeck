use std::cell::OnceCell;

use crate::{
    blur::imeta_blurhashes,
    jobs::JobsCache,
    note::{NoteAction, NoteOptions, NoteResponse, NoteView},
    secondary_label,
};

use egui::{vec2, Color32, Hyperlink, Label, RichText};
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use tracing::warn;

use notedeck::{IsFollowing, NoteCache, NoteContext, NotedeckTextStyle};

use super::media::{find_renderable_media, image_carousel, RenderableMedia};

pub struct NoteContents<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    options: NoteOptions,
    pub action: Option<NoteAction>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> NoteContents<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        txn: &'a Transaction,
        note: &'a Note,
        options: NoteOptions,
        jobs: &'a mut JobsCache,
    ) -> Self {
        NoteContents {
            note_context,
            txn,
            note,
            options,
            action: None,
            jobs,
        }
    }
}

impl egui::Widget for &mut NoteContents<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

        if self.options.contains(NoteOptions::ShowNoteClientTop) {
            render_client(ui, self.note_context.note_cache, self.note);
        }
        let result = render_note_contents(
            ui,
            self.note_context,
            self.txn,
            self.note,
            self.options,
            self.jobs,
        );
        if self.options.contains(NoteOptions::ShowNoteClientBottom) {
            render_client(ui, self.note_context.note_cache, self.note);
        }
        self.action = result.action;
        result.response
    }
}

#[profiling::function]
fn render_client(ui: &mut egui::Ui, note_cache: &mut NoteCache, note: &Note) {
    let cached_note = note_cache.cached_note_or_insert_mut(note.key().unwrap(), note);

    match cached_note.client.as_deref() {
        Some(client) if !client.is_empty() => {
            ui.horizontal(|ui| {
                secondary_label(ui, format!("via {client}"));
            });
        }
        _ => return,
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
#[allow(clippy::too_many_arguments)]
#[profiling::function]
pub fn render_note_preview(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    id: &[u8; 32],
    parent: NoteKey,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
) -> NoteResponse {
    let note = if let Ok(note) = note_context.ndb.get_note_by_id(txn, id) {
        // TODO: support other preview kinds
        if note.kind() == 1 {
            note
        } else {
            return NoteResponse::new(ui.colored_label(
                Color32::RED,
                format!("TODO: can't preview kind {}", note.kind()),
            ));
        }
    } else {
        note_context
            .unknown_ids
            .add_note_id_if_missing(note_context.ndb, txn, id);

        return NoteResponse::new(ui.colored_label(Color32::RED, "TODO: COULD NOT LOAD"));
        /*
        return ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.colored_label(link_color, "@");
                ui.colored_label(link_color, &id_str[4..16]);
            })
            .response;
            */
    };

    NoteView::new(note_context, &note, note_options, jobs)
        .preview_style()
        .parent(parent)
        .show(ui)
}

#[allow(clippy::too_many_arguments)]
#[profiling::function]
pub fn render_note_contents(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    note: &Note,
    options: NoteOptions,
    jobs: &mut JobsCache,
) -> NoteResponse {
    let note_key = note.key().expect("todo: implement non-db notes");
    let selectable = options.contains(NoteOptions::SelectableText);
    let mut note_action: Option<NoteAction> = None;
    let mut inline_note: Option<(&[u8; 32], &str)> = None;
    let hide_media = options.contains(NoteOptions::HideMedia);
    let link_color = ui.visuals().hyperlink_color;

    // The current length of the rendered blocks. Used in trucation logic
    let mut current_len: usize = 0;
    let truncate_len = 280;

    if !options.contains(NoteOptions::IsPreview) {
        // need this for the rect to take the full width of the column
        let _ = ui.allocate_at_least(egui::vec2(ui.available_width(), 0.0), egui::Sense::click());
    }

    let mut supported_medias: Vec<RenderableMedia> = vec![];
    let blurhashes = OnceCell::new();

    let response = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = note_context.ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("missing note content blocks? '{}'", note.content());
            ui.weak(note.content());
            return;
        };

        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

        'block_loop: for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => match block.as_mention().unwrap() {
                    Mention::Profile(profile) => {
                        let act = crate::Mention::new(
                            note_context.ndb,
                            note_context.img_cache,
                            txn,
                            profile.pubkey(),
                        )
                        .show(ui);

                        if act.is_some() {
                            note_action = act;
                        }
                    }

                    Mention::Pubkey(npub) => {
                        let act = crate::Mention::new(
                            note_context.ndb,
                            note_context.img_cache,
                            txn,
                            npub.pubkey(),
                        )
                        .show(ui);

                        if act.is_some() {
                            note_action = act;
                        }
                    }

                    Mention::Note(note) if options.contains(NoteOptions::HasNotePreviews) => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    Mention::Event(note) if options.contains(NoteOptions::HasNotePreviews) => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    _ => {
                        ui.colored_label(
                            link_color,
                            RichText::new(format!("@{}", &block.as_str()[..16]))
                                .text_style(NotedeckTextStyle::NoteBody.text_style()),
                        );
                    }
                },

                BlockType::Hashtag => {
                    if block.as_str().trim().len() == 0 {
                        continue 'block_loop;
                    }
                    let resp = ui
                        .colored_label(
                            link_color,
                            RichText::new(format!("#{}", block.as_str()))
                                .text_style(NotedeckTextStyle::NoteBody.text_style()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);

                    if resp.clicked() {
                        note_action = Some(NoteAction::Hashtag(block.as_str().to_string()));
                    }
                }

                BlockType::Url => {
                    let mut found_supported = || -> bool {
                        let url = block.as_str();

                        let blurs = blurhashes.get_or_init(|| imeta_blurhashes(note));

                        let Some(media_type) =
                            find_renderable_media(&mut note_context.img_cache.urls, blurs, url)
                        else {
                            return false;
                        };

                        supported_medias.push(media_type);
                        true
                    };

                    if hide_media || !found_supported() {
                        if block.as_str().trim().len() == 0 {
                            continue 'block_loop;
                        }
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str())
                                .color(link_color)
                                .text_style(NotedeckTextStyle::NoteBody.text_style()),
                            block.as_str(),
                        ));
                    }
                }

                BlockType::Text => {
                    // truncate logic
                    let mut truncate = false;
                    let block_str = if options.contains(NoteOptions::Truncate)
                        && (current_len + block.as_str().len() > truncate_len)
                    {
                        truncate = true;
                        // The current block goes over the truncate length,
                        // we'll need to truncate this block
                        let block_str = block.as_str();
                        let closest = notedeck::abbrev::floor_char_boundary(
                            block_str,
                            truncate_len - current_len,
                        );
                        &(block_str[..closest].to_string() + "…")
                    } else {
                        let block_str = block.as_str();
                        current_len += block_str.len();
                        block_str
                    };
                    if block_str.trim().len() == 0 {
                        continue 'block_loop;
                    }
                    if options.contains(NoteOptions::ScrambleText) {
                        ui.add(
                            Label::new(
                                RichText::new(rot13(block_str))
                                    .text_style(NotedeckTextStyle::NoteBody.text_style()),
                            )
                            .wrap()
                            .selectable(selectable),
                        );
                    } else {
                        ui.add(
                            Label::new(
                                RichText::new(block_str)
                                    .text_style(NotedeckTextStyle::NoteBody.text_style()),
                            )
                            .wrap()
                            .selectable(selectable),
                        );
                    }
                    // don't render any more blocks
                    if truncate {
                        break;
                    }
                }

                _ => {
                    ui.colored_label(link_color, block.as_str());
                }
            }
        }
    });

    let preview_note_action = inline_note.and_then(|(id, _)| {
        render_note_preview(ui, note_context, txn, id, note_key, options, jobs)
            .action
            .map(|a| match a {
                NoteAction::Note { note_id, .. } => NoteAction::Note {
                    note_id,
                    preview: true,
                },
                other => other,
            })
    });

    let mut media_action = None;
    if !supported_medias.is_empty() && !options.contains(NoteOptions::Textmode) {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));

        let is_self = note.pubkey()
            == note_context
                .accounts
                .get_selected_account()
                .key
                .pubkey
                .bytes();
        let trusted_media = is_self
            || note_context
                .accounts
                .get_selected_account()
                .is_following(note.pubkey())
                == IsFollowing::Yes;

        media_action = image_carousel(
            ui,
            note_context.img_cache,
            note_context.job_pool,
            jobs,
            &supported_medias,
            carousel_id,
            trusted_media,
            note_context.i18n,
        );
        ui.add_space(2.0);
    }

    let note_action = preview_note_action
        .or(note_action)
        .or(media_action.map(NoteAction::Media));

    NoteResponse::new(response.response).with_action(note_action)
}

fn rot13(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                // Rotate lowercase letters
                (((c as u8 - b'a' + 13) % 26) + b'a') as char
            } else if c.is_ascii_uppercase() {
                // Rotate uppercase letters
                (((c as u8 - b'A' + 13) % 26) + b'A') as char
            } else {
                // Leave other characters unchanged
                c
            }
        })
        .collect()
}
