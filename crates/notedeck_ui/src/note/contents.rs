use super::media::image_carousel;
use crate::{
    note::{NoteAction, NoteOptions, NoteResponse, NoteView},
    secondary_label,
};
use egui::{Color32, Hyperlink, Label, RichText};
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use notedeck::Localization;
use notedeck::{time_format, update_imeta_blurhashes, NoteCache, NoteContext, NotedeckTextStyle};
use notedeck::{JobsCache, RenderableMedia};
use tracing::warn;

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
        let result = render_note_contents(
            ui,
            self.note_context,
            self.txn,
            self.note,
            self.options,
            self.jobs,
        );
        self.action = result.action;
        result.response
    }
}

fn render_client_name(ui: &mut egui::Ui, note_cache: &mut NoteCache, note: &Note, before: bool) {
    let cached_note = note_cache.cached_note_or_insert_mut(note.key().unwrap(), note);

    let Some(client) = cached_note.client.as_ref() else {
        return;
    };

    if client.is_empty() {
        return;
    }

    if before {
        secondary_label(ui, "⋅");
    }

    secondary_label(ui, format!("via {client}"));
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

/// Render note contents and surrounding info (client name, full date timestamp)
fn render_note_contents(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    note: &Note,
    options: NoteOptions,
    jobs: &mut JobsCache,
) -> NoteResponse {
    let response = render_undecorated_note_contents(ui, note_context, txn, note, options, jobs);

    ui.horizontal_wrapped(|ui| {
        note_bottom_metadata_ui(
            ui,
            note_context.i18n,
            note_context.note_cache,
            note,
            options,
        );
    });

    response
}

/// Client name, full timestamp, etc
fn note_bottom_metadata_ui(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    note_cache: &mut NoteCache,
    note: &Note,
    options: NoteOptions,
) {
    let show_full_date = options.contains(NoteOptions::FullCreatedDate);

    if show_full_date {
        secondary_label(ui, time_format(i18n, note.created_at()));
    }

    if options.contains(NoteOptions::ClientName) {
        render_client_name(ui, note_cache, note, show_full_date);
    }
}

#[allow(clippy::too_many_arguments)]
#[profiling::function]
fn render_undecorated_note_contents<'a>(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    note: &'a Note,
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

    let response = ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;

        let blocks = if let Ok(blocks) = note_context.ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("missing note content blocks? '{}'", note.content());
            ui.weak(note.content());
            return;
        };

        for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => match block.as_mention().unwrap() {
                    Mention::Profile(profile) => {
                        profiling::scope!("profile-block");
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
                        profiling::scope!("pubkey-block");
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
                    profiling::scope!("hashtag-block");
                    if block.as_str().trim().is_empty() {
                        continue;
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
                    profiling::scope!("url-block");
                    let mut found_supported = || -> bool {
                        let url = block.as_str();

                        if !note_context.img_cache.metadata.contains_key(url) {
                            update_imeta_blurhashes(note, &mut note_context.img_cache.metadata);
                        }

                        let Some(media) = note_context.img_cache.get_renderable_media(url) else {
                            return false;
                        };

                        supported_medias.push(media);
                        true
                    };

                    if hide_media || !found_supported() {
                        if block.as_str().trim().is_empty() {
                            continue;
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
                    profiling::scope!("text-block");
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
                    if block_str.trim().is_empty() {
                        continue;
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
                        let mut richtext = RichText::new(block_str)
                            .text_style(NotedeckTextStyle::NoteBody.text_style());

                        if options.contains(NoteOptions::NotificationPreview) {
                            richtext = richtext.color(egui::Color32::from_rgb(0x87, 0x87, 0x8D));
                        }

                        ui.add(Label::new(richtext).wrap().selectable(selectable));
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
                    scroll_offset: 0.0,
                },
                other => other,
            })
    });

    let mut media_action = None;
    if !supported_medias.is_empty() && !options.contains(NoteOptions::Textmode) {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));

        media_action = image_carousel(
            ui,
            note_context.img_cache,
            note_context.job_pool,
            jobs,
            &supported_medias,
            carousel_id,
            note_context.i18n,
            options,
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
