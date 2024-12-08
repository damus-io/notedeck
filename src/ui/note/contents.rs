use crate::actionbar::NoteAction;
use crate::images::ImageType;
use crate::imgcache::ImageCache;
use crate::notecache::NoteCache;
use crate::ui::note::{NoteOptions, NoteResponse};
use crate::ui::ProfilePic;
use crate::{colors, ui};
use egui::{Color32, Hyperlink, Image, RichText};
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use tracing::warn;

pub struct NoteContents<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
    note_cache: &'a mut NoteCache,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    note_key: NoteKey,
    options: NoteOptions,
    action: Option<NoteAction>,
}

impl<'a> NoteContents<'a> {
    pub fn new(
        ndb: &'a Ndb,
        img_cache: &'a mut ImageCache,
        note_cache: &'a mut NoteCache,
        txn: &'a Transaction,
        note: &'a Note,
        note_key: NoteKey,
        options: ui::note::NoteOptions,
    ) -> Self {
        NoteContents {
            ndb,
            img_cache,
            note_cache,
            txn,
            note,
            note_key,
            options,
            action: None,
        }
    }

    pub fn action(&self) -> &Option<NoteAction> {
        &self.action
    }
}

impl egui::Widget for &mut NoteContents<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let result = render_note_contents(
            ui,
            self.ndb,
            self.img_cache,
            self.note_cache,
            self.txn,
            self.note,
            self.note_key,
            self.options,
        );
        self.action = result.action;
        result.response
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
pub fn render_note_preview(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    img_cache: &mut ImageCache,
    txn: &Transaction,
    id: &[u8; 32],
    parent: NoteKey,
) -> NoteResponse {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let note = if let Ok(note) = ndb.get_note_by_id(txn, id) {
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
        return NoteResponse::new(ui.colored_label(Color32::RED, "TODO: COULD NOT LOAD"));
        /*
        return ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.colored_label(colors::PURPLE, "@");
                ui.colored_label(colors::PURPLE, &id_str[4..16]);
            })
            .response;
            */
    };

    egui::Frame::none()
        .fill(ui.visuals().noninteractive().weak_bg_fill)
        .inner_margin(egui::Margin::same(8.0))
        .outer_margin(egui::Margin::symmetric(0.0, 8.0))
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui::NoteView::new(ndb, note_cache, img_cache, &note)
                .actionbar(false)
                .small_pfp(true)
                .wide(true)
                .note_previews(false)
                .options_button(true)
                .parent(parent)
                .show(ui)
        })
        .inner
}

fn is_image_link(url: &str) -> bool {
    url.ends_with("png") || url.ends_with("jpg") || url.ends_with("jpeg")
}

#[allow(clippy::too_many_arguments)]
fn render_note_contents(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    txn: &Transaction,
    note: &Note,
    note_key: NoteKey,
    options: NoteOptions,
) -> NoteResponse {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let selectable = options.has_selectable_text();
    let mut images: Vec<String> = vec![];
    let mut inline_note: Option<(&[u8; 32], &str)> = None;
    let hide_media = options.has_hide_media();

    let response = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("missing note content blocks? '{}'", note.content());
            ui.weak(note.content());
            return;
        };

        ui.spacing_mut().item_spacing.x = 0.0;

        for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => match block.as_mention().unwrap() {
                    Mention::Profile(profile) => {
                        ui.add(ui::Mention::new(ndb, img_cache, txn, profile.pubkey()));
                    }

                    Mention::Pubkey(npub) => {
                        ui.add(ui::Mention::new(ndb, img_cache, txn, npub.pubkey()));
                    }

                    Mention::Note(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    Mention::Event(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    _ => {
                        ui.colored_label(colors::PURPLE, format!("@{}", &block.as_str()[4..16]));
                    }
                },

                BlockType::Hashtag => {
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("hashtag contents");
                    ui.colored_label(colors::PURPLE, format!("#{}", block.as_str()));
                }

                BlockType::Url => {
                    let lower_url = block.as_str().to_lowercase();
                    if !hide_media && is_image_link(&lower_url) {
                        images.push(block.as_str().to_string());
                    } else {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("url contents");
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str()).color(colors::PURPLE),
                            block.as_str(),
                        ));
                    }
                }

                BlockType::Text => {
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("text contents");
                    ui.add(egui::Label::new(block.as_str()).selectable(selectable));
                }

                _ => {
                    ui.colored_label(colors::PURPLE, block.as_str());
                }
            }
        }
    });

    let note_action = if let Some((id, _block_str)) = inline_note {
        render_note_preview(ui, ndb, note_cache, img_cache, txn, id, note_key).action
    } else {
        None
    };

    if !images.is_empty() && !options.has_textmode() {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));
        image_carousel(ui, img_cache, images, carousel_id);
        ui.add_space(2.0);
    }

    NoteResponse::new(response.response).with_action(note_action)
}

fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut ImageCache,
    images: Vec<String>,
    carousel_id: egui::Id,
) {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_size().x;
    let spinsz = if height > width { width } else { height };

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for image in images {
                        // If the cache is empty, initiate the fetch
                        let m_cached_promise = img_cache.map().get(&image);
                        if m_cached_promise.is_none() {
                            let res = crate::images::fetch_img(
                                img_cache,
                                ui.ctx(),
                                &image,
                                ImageType::Content(width.round() as u32, height.round() as u32),
                            );
                            img_cache.map_mut().insert(image.to_owned(), res);
                        }

                        // What is the state of the fetch?
                        match img_cache.map()[&image].ready() {
                            // Still waiting
                            None => {
                                ui.allocate_space(egui::vec2(spinsz, spinsz));
                                //ui.add(egui::Spinner::new().size(spinsz));
                            }
                            // Failed to fetch image!
                            Some(Err(_err)) => {
                                // FIXME - use content-specific error instead
                                let no_pfp = crate::images::fetch_img(
                                    img_cache,
                                    ui.ctx(),
                                    ProfilePic::no_pfp_url(),
                                    ImageType::Profile(128),
                                );
                                img_cache.map_mut().insert(image.to_owned(), no_pfp);
                                // spin until next pass
                                ui.allocate_space(egui::vec2(spinsz, spinsz));
                                //ui.add(egui::Spinner::new().size(spinsz));
                            }
                            // Use the previously resolved image
                            Some(Ok(img)) => {
                                let img_resp = ui.add(
                                    Image::new(img)
                                        .max_height(height)
                                        .rounding(5.0)
                                        .fit_to_original_size(1.0),
                                );
                                img_resp.context_menu(|ui| {
                                    if ui.button("Copy Link").clicked() {
                                        ui.ctx().copy_text(image);
                                        ui.close_menu();
                                    }
                                });
                            }
                        }
                    }
                })
                .response
            })
            .inner
    });
}
