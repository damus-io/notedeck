use crate::draft::{Draft, Drafts, MentionHint};
use crate::gif::GifStateMap;
use crate::images::fetch_img;
use crate::media_upload::{nostrbuild_nip96_upload, MediaPath};
use crate::post::{downcast_post_buffer, MentionType, NewPost};
use crate::profile::get_display_name;
use crate::ui::search_results::SearchResultsView;
use crate::ui::{self, Preview, PreviewConfig};
use crate::Result;
use egui::text::{CCursorRange, LayoutJob};
use egui::text_edit::TextEditOutput;
use egui::widgets::text_edit::TextEdit;
use egui::{vec2, Frame, Layout, Margin, Pos2, ScrollArea, Sense, TextBuffer};
use enostr::{FilledKeypair, FullKeypair, NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};

use notedeck::{ImageCache, NoteCache};
use tracing::error;

use super::contents::render_note_preview;

pub struct PostView<'a> {
    ndb: &'a Ndb,
    draft: &'a mut Draft,
    post_type: PostType,
    img_cache: &'a mut ImageCache,
    note_cache: &'a mut NoteCache,
    gifs: &'a mut GifStateMap,
    poster: FilledKeypair<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
}

#[derive(Clone)]
pub enum PostType {
    New,
    Quote(NoteId),
    Reply(NoteId),
}

pub struct PostAction {
    post_type: PostType,
    post: NewPost,
}

impl PostAction {
    pub fn new(post_type: PostType, post: NewPost) -> Self {
        PostAction { post_type, post }
    }

    pub fn execute(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
        pool: &mut RelayPool,
        drafts: &mut Drafts,
    ) -> Result<()> {
        let seckey = self.post.account.secret_key.to_secret_bytes();

        let note = match self.post_type {
            PostType::New => self.post.to_note(&seckey),

            PostType::Reply(target) => {
                let replying_to = ndb.get_note_by_id(txn, target.bytes())?;
                self.post.to_reply(&seckey, &replying_to)
            }

            PostType::Quote(target) => {
                let quoting = ndb.get_note_by_id(txn, target.bytes())?;
                self.post.to_quote(&seckey, &quoting)
            }
        };

        pool.send(&enostr::ClientMessage::event(note)?);
        drafts.get_from_post_type(&self.post_type).clear();

        Ok(())
    }
}

pub struct PostResponse {
    pub action: Option<PostAction>,
    pub edit_response: egui::Response,
}

impl<'a> PostView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ndb: &'a Ndb,
        draft: &'a mut Draft,
        post_type: PostType,
        img_cache: &'a mut ImageCache,
        note_cache: &'a mut NoteCache,
        gifs: &'a mut GifStateMap,
        poster: FilledKeypair<'a>,
        inner_rect: egui::Rect,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        PostView {
            ndb,
            draft,
            img_cache,
            note_cache,
            gifs,
            poster,
            id_source,
            post_type,
            inner_rect,
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
            .and_then(|p| {
                Some(ui::ProfilePic::from_profile(self.img_cache, self.gifs, p)?.size(pfp_size))
            });

        if let Some(pfp) = poster_pfp {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(self.img_cache, self.gifs, ui::ProfilePic::no_pfp_url())
                    .size(pfp_size),
            );
        }

        let mut updated_layout = false;
        let mut layouter = |ui: &egui::Ui, buf: &dyn TextBuffer, wrap_width: f32| {
            if let Some(post_buffer) = downcast_post_buffer(buf) {
                let maybe_job = if post_buffer.need_new_layout(self.draft.cur_layout.as_ref()) {
                    Some(post_buffer.to_layout_job(ui))
                } else {
                    None
                };

                if let Some(job) = maybe_job {
                    self.draft.cur_layout = Some((post_buffer.text_buffer.clone(), job));
                    updated_layout = true;
                }
            };

            let mut layout_job = if let Some((_, job)) = &self.draft.cur_layout {
                job.clone()
            } else {
                error!("Failed to get custom mentions layouter");
                text_edit_default_layout(ui, buf.as_str().to_owned(), wrap_width)
            };

            layout_job.wrap.max_width = wrap_width;
            ui.fonts(|f| f.layout_job(layout_job))
        };

        let textedit = TextEdit::multiline(&mut self.draft.buffer)
            .hint_text(egui::RichText::new("Write a banger note here...").weak())
            .frame(false)
            .desired_width(ui.available_width())
            .layouter(&mut layouter);

        let out = textedit.show(ui);

        if updated_layout {
            self.draft.buffer.selected_mention = false;
        }

        if let Some(cursor_index) = get_cursor_index(&out.state.cursor.char_range()) {
            self.show_mention_hints(txn, ui, cursor_index, &out);
        }

        let focused = out.response.has_focus();

        ui.ctx().data_mut(|d| d.insert_temp(self.id(), focused));

        out.response
    }

    fn show_mention_hints(
        &mut self,
        txn: &nostrdb::Transaction,
        ui: &mut egui::Ui,
        cursor_index: usize,
        textedit_output: &TextEditOutput,
    ) {
        if let Some(mention) = &self.draft.buffer.get_mention(cursor_index) {
            if mention.info.mention_type == MentionType::Pending {
                let mention_str = self.draft.buffer.get_mention_string(mention);

                if !mention_str.is_empty() {
                    if let Some(mention_hint) = &mut self.draft.cur_mention_hint {
                        if mention_hint.index != mention.index {
                            mention_hint.index = mention.index;
                            mention_hint.pos = calculate_mention_hints_pos(
                                textedit_output,
                                mention.info.start_index,
                            );
                        }
                        mention_hint.text = mention_str.to_owned();
                    } else {
                        self.draft.cur_mention_hint = Some(MentionHint {
                            index: mention.index,
                            text: mention_str.to_owned(),
                            pos: calculate_mention_hints_pos(
                                textedit_output,
                                mention.info.start_index,
                            ),
                        });
                    }
                }

                if let Some(hint) = &self.draft.cur_mention_hint {
                    let hint_rect = {
                        let mut hint_rect = self.inner_rect;
                        hint_rect.set_top(hint.pos.y);
                        hint_rect
                    };

                    if let Ok(res) = self.ndb.search_profile(txn, mention_str, 10) {
                        let hint_selection =
                            SearchResultsView::new(self.img_cache, self.gifs, self.ndb, txn, &res)
                                .show_in_rect(hint_rect, ui);

                        if let Some(hint_index) = hint_selection {
                            if let Some(pk) = res.get(hint_index) {
                                let record = self.ndb.get_profile_by_pubkey(txn, pk);

                                self.draft.buffer.select_mention_and_replace_name(
                                    mention.index,
                                    get_display_name(record.ok().as_ref()).name(),
                                    Pubkey::new(**pk),
                                );
                                self.draft.cur_mention_hint = None;
                            }
                        }
                    }
                }
            }
        }
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

                    if let PostType::Quote(id) = self.post_type {
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
                                        self.gifs,
                                        txn,
                                        id.bytes(),
                                        nostrdb::NoteKey::new(0),
                                    );
                                });
                            });
                        });
                    }

                    Frame::none()
                        .inner_margin(Margin::symmetric(0.0, 8.0))
                        .show(ui, |ui| {
                            ScrollArea::horizontal().show(ui, |ui| {
                                ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                                    ui.add_space(4.0);
                                    self.show_media(ui);
                                });
                            });
                        });

                    self.transfer_uploads(ui);
                    self.show_upload_errors(ui);

                    let action = ui
                        .horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::BOTTOM),
                                |ui| {
                                    self.show_upload_media_button(ui);
                                },
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::BOTTOM), |ui| {
                                let post_button_clicked = ui
                                    .add_sized(
                                        [91.0, 32.0],
                                        post_button(!self.draft.buffer.is_empty()),
                                    )
                                    .clicked();

                                let ctrl_enter_pressed = ui
                                    .input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));

                                if post_button_clicked
                                    || (!self.draft.buffer.is_empty() && ctrl_enter_pressed)
                                {
                                    let output = self.draft.buffer.output();
                                    let new_post = NewPost::new(
                                        output.text,
                                        self.poster.to_full(),
                                        self.draft.uploaded_media.clone(),
                                        output.mentions,
                                    );
                                    Some(PostAction::new(self.post_type.clone(), new_post))
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

    fn show_media(&mut self, ui: &mut egui::Ui) {
        let mut to_remove = Vec::new();
        for (i, media) in self.draft.uploaded_media.iter().enumerate() {
            let (width, height) = if let Some(dims) = media.dimensions {
                (dims.0, dims.1)
            } else {
                (300, 300)
            };
            let m_cached_promise = self.img_cache.map().get(&media.url);
            if m_cached_promise.is_none() {
                let promise = fetch_img(
                    self.img_cache,
                    ui.ctx(),
                    &media.url,
                    crate::images::ImageType::Content(width, height),
                );
                self.img_cache
                    .map_mut()
                    .insert(media.url.to_owned(), promise);
            }

            match self.img_cache.map()[&media.url].ready() {
                Some(Ok(texture)) => {
                    let media_size = vec2(width as f32, height as f32);
                    let max_size = vec2(300.0, 300.0);
                    let size = if media_size.x > max_size.x || media_size.y > max_size.y {
                        max_size
                    } else {
                        media_size
                    };

                    let img_resp = ui.add(egui::Image::new(texture).max_size(size).rounding(12.0));

                    let remove_button_rect = {
                        let top_left = img_resp.rect.left_top();
                        let spacing = 13.0;
                        let center = Pos2::new(top_left.x + spacing, top_left.y + spacing);
                        egui::Rect::from_center_size(center, egui::vec2(26.0, 26.0))
                    };
                    if show_remove_upload_button(ui, remove_button_rect).clicked() {
                        to_remove.push(i);
                    }
                    ui.advance_cursor_after_rect(img_resp.rect);
                }
                Some(Err(e)) => {
                    self.draft.upload_errors.push(e.to_string());
                    error!("{e}");
                }
                None => {
                    ui.spinner();
                }
            }
        }
        to_remove.reverse();
        for i in to_remove {
            self.draft.uploaded_media.remove(i);
        }
    }

    fn show_upload_media_button(&mut self, ui: &mut egui::Ui) {
        if ui.add(media_upload_button()).clicked() {
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    for file in files {
                        match MediaPath::new(file) {
                            Ok(media_path) => {
                                let promise = nostrbuild_nip96_upload(
                                    self.poster.secret_key.secret_bytes(),
                                    media_path,
                                );
                                self.draft.uploading_media.push(promise);
                            }
                            Err(e) => {
                                error!("{e}");
                                self.draft.upload_errors.push(e.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    fn transfer_uploads(&mut self, ui: &mut egui::Ui) {
        let mut indexes_to_remove = Vec::new();
        for (i, promise) in self.draft.uploading_media.iter().enumerate() {
            match promise.ready() {
                Some(Ok(media)) => {
                    self.draft.uploaded_media.push(media.clone());
                    indexes_to_remove.push(i);
                }
                Some(Err(e)) => {
                    self.draft.upload_errors.push(e.to_string());
                    error!("{e}");
                }
                None => {
                    ui.spinner();
                }
            }
        }

        indexes_to_remove.reverse();
        for i in indexes_to_remove {
            let _ = self.draft.uploading_media.remove(i);
        }
    }

    fn show_upload_errors(&mut self, ui: &mut egui::Ui) {
        let mut to_remove = Vec::new();
        for (i, error) in self.draft.upload_errors.iter().enumerate() {
            if ui
                .add(
                    egui::Label::new(egui::RichText::new(error).color(ui.visuals().warn_fg_color))
                        .sense(Sense::click())
                        .selectable(false),
                )
                .on_hover_text_at_pointer("Dismiss")
                .clicked()
            {
                to_remove.push(i);
            }
        }
        to_remove.reverse();

        for i in to_remove {
            self.draft.upload_errors.remove(i);
        }
    }
}

fn post_button(interactive: bool) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let button = egui::Button::new("Post now");
        if interactive {
            ui.add(button)
        } else {
            ui.add(
                button
                    .sense(egui::Sense::hover())
                    .fill(ui.visuals().widgets.noninteractive.bg_fill)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke),
            )
            .on_hover_cursor(egui::CursorIcon::NotAllowed)
        }
    }
}

fn media_upload_button() -> impl egui::Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let resp = ui.allocate_response(egui::vec2(32.0, 32.0), egui::Sense::click());
        let painter = ui.painter();
        let (fill_color, stroke) = if resp.hovered() {
            (
                ui.visuals().widgets.hovered.bg_fill,
                ui.visuals().widgets.hovered.bg_stroke,
            )
        } else if resp.clicked() {
            (
                ui.visuals().widgets.active.bg_fill,
                ui.visuals().widgets.active.bg_stroke,
            )
        } else {
            (
                ui.visuals().widgets.inactive.bg_fill,
                ui.visuals().widgets.inactive.bg_stroke,
            )
        };

        painter.rect_filled(resp.rect, 8.0, fill_color);
        painter.rect_stroke(resp.rect, 8.0, stroke);
        egui::Image::new(egui::include_image!(
            "../../../../../assets/icons/media_upload_dark_4x.png"
        ))
        .max_size(egui::vec2(16.0, 16.0))
        .paint_at(ui, resp.rect.shrink(8.0));
        resp
    }
}

fn show_remove_upload_button(ui: &mut egui::Ui, desired_rect: egui::Rect) -> egui::Response {
    let resp = ui.allocate_rect(desired_rect, egui::Sense::click());
    let size = 24.0;
    let (fill_color, stroke) = if resp.hovered() {
        (
            ui.visuals().widgets.hovered.bg_fill,
            ui.visuals().widgets.hovered.bg_stroke,
        )
    } else if resp.clicked() {
        (
            ui.visuals().widgets.active.bg_fill,
            ui.visuals().widgets.active.bg_stroke,
        )
    } else {
        (
            ui.visuals().widgets.inactive.bg_fill,
            ui.visuals().widgets.inactive.bg_stroke,
        )
    };
    let center = desired_rect.center();
    let painter = ui.painter_at(desired_rect);
    let radius = size / 2.0;

    painter.circle_filled(center, radius, fill_color);
    painter.circle_stroke(center, radius, stroke);

    painter.line_segment(
        [
            Pos2::new(center.x - 4.0, center.y - 4.0),
            Pos2::new(center.x + 4.0, center.y + 4.0),
        ],
        egui::Stroke::new(1.33, ui.visuals().text_color()),
    );

    painter.line_segment(
        [
            Pos2::new(center.x + 4.0, center.y - 4.0),
            Pos2::new(center.x - 4.0, center.y + 4.0),
        ],
        egui::Stroke::new(1.33, ui.visuals().text_color()),
    );
    resp
}

fn get_cursor_index(cursor: &Option<CCursorRange>) -> Option<usize> {
    let range = cursor.as_ref()?;

    if range.primary.index == range.secondary.index {
        Some(range.primary.index)
    } else {
        None
    }
}

fn calculate_mention_hints_pos(out: &TextEditOutput, char_pos: usize) -> egui::Pos2 {
    let mut cur_pos = 0;

    for row in &out.galley.rows {
        if cur_pos + row.glyphs.len() <= char_pos {
            cur_pos += row.glyphs.len();
        } else if let Some(glyph) = row.glyphs.get(char_pos - cur_pos) {
            let mut pos = glyph.pos + out.galley_pos.to_vec2();
            pos.y += row.rect.height();
            return pos;
        }
    }

    out.text_clip_rect.left_bottom()
}

fn text_edit_default_layout(ui: &egui::Ui, text: String, wrap_width: f32) -> LayoutJob {
    LayoutJob::simple(
        text,
        egui::FontSelection::default().resolve(ui.style()),
        ui.visuals()
            .override_text_color
            .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color()),
        wrap_width,
    )
}

mod preview {

    use crate::media_upload::Nip94Event;

    use super::*;
    use notedeck::{App, AppContext};

    pub struct PostPreview {
        draft: Draft,
        poster: FullKeypair,
        gifs: GifStateMap,
    }

    impl PostPreview {
        fn new() -> Self {
            let mut draft = Draft::new();
            // can use any url here
            draft.uploaded_media.push(Nip94Event::new(
                "https://image.nostr.build/41b40657dd6abf7c275dffc86b29bd863e9337a74870d4ee1c33a72a91c9d733.jpg".to_owned(),
                612,
                407,
            ));
            draft.uploaded_media.push(Nip94Event::new(
                "https://image.nostr.build/thumb/fdb46182b039d29af0f5eac084d4d30cd4ad2580ea04fe6c7e79acfe095f9852.png".to_owned(),
                80,
                80,
            ));
            draft.uploaded_media.push(Nip94Event::new(
                "https://i.nostr.build/7EznpHsnBZ36Akju.png".to_owned(),
                2438,
                1476,
            ));
            draft.uploaded_media.push(Nip94Event::new(
                "https://i.nostr.build/qCCw8szrjTydTiMV.png".to_owned(),
                2002,
                2272,
            ));
            PostPreview {
                draft,
                gifs: Default::default(),
                poster: FullKeypair::generate(),
            }
        }
    }

    impl App for PostPreview {
        fn update(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) {
            let txn = Transaction::new(app.ndb).expect("txn");
            PostView::new(
                app.ndb,
                &mut self.draft,
                PostType::New,
                app.img_cache,
                app.note_cache,
                &mut self.gifs,
                self.poster.to_filled(),
                ui.available_rect_before_wrap(),
            )
            .ui(&txn, ui);
        }
    }

    impl Preview for PostView<'_> {
        type Prev = PostPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            PostPreview::new()
        }
    }
}
