use crate::app::Damus;
use crate::draft::{Draft, DraftSource};
use crate::ui;
use crate::ui::{Preview, PreviewConfig, View};
use egui::widgets::text_edit::TextEdit;
use nostrdb::Transaction;

pub struct PostView<'app, 'd> {
    app: &'app mut Damus,
    /// account index
    poster: usize,
    draft_source: DraftSource<'d>,
    id_source: Option<egui::Id>,
}

pub struct NewPost {
    pub content: String,
    pub account: usize,
}

pub enum PostAction {
    Post(NewPost),
}

pub struct PostResponse {
    pub action: Option<PostAction>,
    pub edit_response: egui::Response,
}

impl<'app, 'd> PostView<'app, 'd> {
    pub fn new(app: &'app mut Damus, draft_source: DraftSource<'d>, poster: usize) -> Self {
        let id_source: Option<egui::Id> = None;
        PostView {
            id_source,
            app,
            poster,
            draft_source,
        }
    }

    pub fn id_source(mut self, id_source: impl std::hash::Hash) -> Self {
        self.id_source = Some(egui::Id::new(id_source));
        self
    }

    fn draft(&mut self) -> &mut Draft {
        self.draft_source.draft(&mut self.app.drafts)
    }

    fn editbox(&mut self, txn: &nostrdb::Transaction, ui: &mut egui::Ui) -> egui::Response {
        ui.spacing_mut().item_spacing.x = 12.0;

        let pfp_size = 24.0;

        let poster_pubkey = self
            .app
            .account_manager
            .get_account(self.poster)
            .map(|acc| acc.pubkey.bytes())
            .unwrap_or(crate::test_data::test_pubkey());

        // TODO: refactor pfp control to do all of this for us
        let poster_pfp = self
            .app
            .ndb
            .get_profile_by_pubkey(txn, poster_pubkey)
            .as_ref()
            .ok()
            .and_then(|p| {
                Some(ui::ProfilePic::from_profile(&mut self.app.img_cache, p)?.size(pfp_size))
            });

        if let Some(pfp) = poster_pfp {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(&mut self.app.img_cache, ui::ProfilePic::no_pfp_url())
                    .size(pfp_size),
            );
        }

        let buffer = &mut self.draft_source.draft(&mut self.app.drafts).buffer;
        let response = ui.add(
            TextEdit::multiline(buffer)
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
                        .with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                            if ui
                                .add_sized([91.0, 32.0], egui::Button::new("Post now"))
                                .clicked()
                            {
                                Some(PostAction::Post(NewPost {
                                    content: self.draft().buffer.clone(),
                                    account: self.poster,
                                }))
                            } else {
                                None
                            }
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
    use crate::test_data;

    pub struct PostPreview {
        app: Damus,
    }

    impl PostPreview {
        fn new(is_mobile: bool) -> Self {
            PostPreview {
                app: test_data::test_app(is_mobile),
            }
        }
    }

    impl View for PostPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let txn = Transaction::new(&self.app.ndb).unwrap();
            PostView::new(&mut self.app, DraftSource::Compose, 0).ui(&txn, ui);
        }
    }

    impl<'app, 'p> Preview for PostView<'app, 'p> {
        type Prev = PostPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            PostPreview::new(cfg.is_mobile)
        }
    }
}
