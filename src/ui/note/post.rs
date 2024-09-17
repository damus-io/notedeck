use crate::draft::{Draft, DraftSource};
use crate::imgcache::ImageCache;
use crate::notecache::NoteCache;
use crate::post::NewPost;
use crate::ui;
use crate::ui::{Preview, PreviewConfig, View};
use egui::widgets::text_edit::TextEdit;
use egui::{Frame, Layout};
use enostr::{FilledKeypair, FullKeypair};
use nostrdb::{Config, Ndb, Transaction};

use super::contents::render_note_preview;

pub struct PostView<'a> {
    ndb: &'a Ndb,
    draft: &'a mut Draft,
    draft_source: DraftSource<'a>,
    img_cache: &'a mut ImageCache,
    note_cache: &'a mut NoteCache,
    poster: FilledKeypair<'a>,
    id_source: Option<egui::Id>,
}

pub enum PostAction {
    Post(NewPost),
}

pub struct PostResponse {
    pub action: Option<PostAction>,
    pub edit_response: egui::Response,
}

impl<'a> PostView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        draft: &'a mut Draft,
        draft_source: DraftSource<'a>,
        img_cache: &'a mut ImageCache,
        note_cache: &'a mut NoteCache,
        poster: FilledKeypair<'a>,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        PostView {
            ndb,
            draft,
            img_cache,
            note_cache,
            poster,
            id_source,
            draft_source,
        }
    }

    pub fn id_source(mut self, id_source: impl std::hash::Hash) -> Self {
        self.id_source = Some(egui::Id::new(id_source));
        self
    }

    fn editbox(&mut self, txn: &nostrdb::Transaction, ui: &mut egui::Ui) -> egui::Response {
        ui.spacing_mut().item_spacing.x = 12.0;

        let pfp_size = 24.0;

        // TODO: refactor pfp control to do all of this for us
        let poster_pfp = self
            .ndb
            .get_profile_by_pubkey(txn, self.poster.pubkey.bytes())
            .as_ref()
            .ok()
            .and_then(|p| Some(ui::ProfilePic::from_profile(self.img_cache, p)?.size(pfp_size)));

        if let Some(pfp) = poster_pfp {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(self.img_cache, ui::ProfilePic::no_pfp_url()).size(pfp_size),
            );
        }

        let response = ui.add_sized(
            ui.available_size(),
            TextEdit::multiline(&mut self.draft.buffer)
                .hint_text(egui::RichText::new("Write a banger note here...").weak())
                .frame(false),
        );

        let focused = response.has_focus();

        ui.ctx().data_mut(|d| d.insert_temp(self.id(), focused));

        response
    }

    fn focused(&self, ui: &egui::Ui) -> bool {
        ui.ctx()
            .data(|d| d.get_temp::<bool>(self.id()).unwrap_or(false))
    }

    fn id(&self) -> egui::Id {
        self.id_source.unwrap_or_else(|| egui::Id::new("post"))
    }

    pub fn outer_margin() -> f32 {
        16.0
    }

    pub fn inner_margin() -> f32 {
        12.0
    }

    pub fn ui(&mut self, txn: &nostrdb::Transaction, ui: &mut egui::Ui) -> PostResponse {
        let focused = self.focused(ui);
        let stroke = if focused {
            ui.visuals().selection.stroke
        } else {
            //ui.visuals().selection.stroke
            ui.visuals().noninteractive().bg_stroke
        };

        let mut frame = egui::Frame::default()
            .inner_margin(egui::Margin::same(PostView::inner_margin()))
            .outer_margin(egui::Margin::same(PostView::outer_margin()))
            .fill(ui.visuals().extreme_bg_color)
            .stroke(stroke)
            .rounding(12.0);

        if focused {
            frame = frame.shadow(egui::epaint::Shadow {
                offset: egui::vec2(0.0, 0.0),
                blur: 8.0,
                spread: 0.0,
                color: stroke.color,
            });
        }

        frame
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    let edit_response = ui.horizontal(|ui| self.editbox(txn, ui)).inner;

                    let action = ui
                        .horizontal(|ui| {
                            if let DraftSource::Quote(id) = self.draft_source {
                                let avail_size = ui.available_size_before_wrap();
                                ui.with_layout(Layout::left_to_right(egui::Align::TOP), |ui| {
                                    Frame::none().show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.set_max_width(avail_size.x * 0.8);
                                            render_note_preview(
                                                ui,
                                                self.ndb,
                                                self.note_cache,
                                                self.img_cache,
                                                txn,
                                                id,
                                                "",
                                            );
                                        });
                                    });
                                });
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::BOTTOM), |ui| {
                                if ui
                                    .add_sized([91.0, 32.0], egui::Button::new("Post now"))
                                    .clicked()
                                {
                                    Some(PostAction::Post(NewPost::new(
                                        self.draft.buffer.clone(),
                                        self.poster.to_full(),
                                    )))
                                } else {
                                    None
                                }
                            })
                            .inner
                        })
                        .inner;

                    PostResponse {
                        action,
                        edit_response,
                    }
                })
                .inner
            })
            .inner
    }
}

mod preview {
    use super::*;

    pub struct PostPreview {
        ndb: Ndb,
        img_cache: ImageCache,
        note_cache: NoteCache,
        draft: Draft,
        poster: FullKeypair,
    }

    impl PostPreview {
        fn new() -> Self {
            let ndb = Ndb::new(".", &Config::new()).expect("ndb");

            PostPreview {
                ndb,
                img_cache: ImageCache::new(".".into()),
                note_cache: NoteCache {
                    cache: Default::default(),
                },
                draft: Draft::new(),
                poster: FullKeypair::generate(),
            }
        }
    }

    impl View for PostPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let txn = Transaction::new(&self.ndb).expect("txn");
            PostView::new(
                &self.ndb,
                &mut self.draft,
                DraftSource::Compose,
                &mut self.img_cache,
                &mut self.note_cache,
                self.poster.to_filled(),
            )
            .ui(&txn, ui);
        }
    }

    impl<'a> Preview for PostView<'a> {
        type Prev = PostPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            PostPreview::new()
        }
    }
}
