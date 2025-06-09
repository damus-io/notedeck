use crate::draft::{Draft, Drafts, MentionHint};
#[cfg(not(target_os = "android"))]
use crate::media_upload::{nostrbuild_nip96_upload, MediaPath};
use crate::post::{downcast_post_buffer, MentionType, NewPost};
use crate::ui::search_results::SearchResultsView;
use crate::ui::{self, Preview, PreviewConfig};
use crate::Result;

use egui::{
    text::{CCursorRange, LayoutJob},
    text_edit::TextEditOutput,
    widgets::text_edit::TextEdit,
    Frame, Layout, Margin, Pos2, ScrollArea, Sense, TextBuffer,
};
use enostr::{FilledKeypair, FullKeypair, KeypairUnowned, NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};
use notedeck_ui::blur::PixelDimensions;
use notedeck_ui::images::{get_render_state, RenderState};
use notedeck_ui::jobs::JobsCache;
use notedeck_ui::{
    gif::{handle_repaint, retrieve_latest_texture},
    note::render_note_preview,
    NoteOptions, ProfilePic,
};

use notedeck::{name::get_display_name, supported_mime_hosted_at_url, NoteAction, NoteContext};
use tracing::error;

pub struct PostView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    draft: &'a mut Draft,
    post_type: PostType,
    poster: FilledKeypair<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
    jobs: &'a mut JobsCache,
}

#[derive(Clone)]
pub enum PostType {
    New,
    Quote(NoteId),
    Reply(NoteId),
}

pub enum PostAction {
    /// The NoteAction on a note you are replying to.
    QuotedNoteAction(NoteAction),

    /// The reply/new post action
    NewPostAction(NewPostAction),
}

pub struct NewPostAction {
    post_type: PostType,
    post: NewPost,
}

impl NewPostAction {
    pub fn new(post_type: PostType, post: NewPost) -> Self {
        NewPostAction { post_type, post }
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

        pool.send(&enostr::ClientMessage::event(&note)?);
        drafts.get_from_post_type(&self.post_type).clear();

        Ok(())
    }
}

pub struct PostResponse {
    pub action: Option<PostAction>,
    pub edit_response: egui::Response,
}

impl<'a, 'd> PostView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        draft: &'a mut Draft,
        post_type: PostType,
        poster: FilledKeypair<'a>,
        inner_rect: egui::Rect,
        note_options: NoteOptions,
        jobs: &'a mut JobsCache,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        PostView {
            note_context,
            draft,
            poster,
            id_source,
            post_type,
            inner_rect,
            note_options,
            jobs,
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
            .note_context
            .ndb
            .get_profile_by_pubkey(txn, self.poster.pubkey.bytes())
            .as_ref()
            .ok()
            .and_then(|p| {
                Some(ProfilePic::from_profile(self.note_context.img_cache, p)?.size(pfp_size))
            });

        if let Some(mut pfp) = poster_pfp {
            ui.add(&mut pfp);
        } else {
            ui.add(
                &mut ProfilePic::new(self.note_context.img_cache, notedeck::profile::no_pfp_url())
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
        let Some(mention) = self.draft.buffer.get_mention(cursor_index) else {
            return;
        };

        if mention.info.mention_type != MentionType::Pending {
            return;
        }

        if ui.ctx().input(|r| r.key_pressed(egui::Key::Escape)) {
            self.draft.buffer.delete_mention(mention.index);
            return;
        }

        let mention_str = self.draft.buffer.get_mention_string(&mention);

        if !mention_str.is_empty() {
            if let Some(mention_hint) = &mut self.draft.cur_mention_hint {
                if mention_hint.index != mention.index {
                    mention_hint.index = mention.index;
                    mention_hint.pos =
                        calculate_mention_hints_pos(textedit_output, mention.info.start_index);
                }
                mention_hint.text = mention_str.to_owned();
            } else {
                self.draft.cur_mention_hint = Some(MentionHint {
                    index: mention.index,
                    text: mention_str.to_owned(),
                    pos: calculate_mention_hints_pos(textedit_output, mention.info.start_index),
                });
            }
        }

        let hint_rect = {
            let hint = if let Some(hint) = &self.draft.cur_mention_hint {
                hint
            } else {
                return;
            };

            let mut hint_rect = self.inner_rect;
            hint_rect.set_top(hint.pos.y);
            hint_rect
        };

        let Ok(res) = self.note_context.ndb.search_profile(txn, mention_str, 10) else {
            return;
        };

        let resp = SearchResultsView::new(
            self.note_context.img_cache,
            self.note_context.ndb,
            txn,
            &res,
        )
        .show_in_rect(hint_rect, ui);

        match resp {
            ui::search_results::SearchResultsResponse::SelectResult(selection) => {
                if let Some(hint_index) = selection {
                    if let Some(pk) = res.get(hint_index) {
                        let record = self.note_context.ndb.get_profile_by_pubkey(txn, pk);

                        self.draft.buffer.select_mention_and_replace_name(
                            mention.index,
                            get_display_name(record.ok().as_ref()).name(),
                            Pubkey::new(**pk),
                        );
                        self.draft.cur_mention_hint = None;
                    }
                }
            }

            ui::search_results::SearchResultsResponse::DeleteMention => {
                self.draft.buffer.delete_mention(mention.index)
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

    pub fn outer_margin() -> i8 {
        16
    }

    pub fn inner_margin() -> i8 {
        12
    }

    pub fn ui(&mut self, txn: &Transaction, ui: &mut egui::Ui) -> PostResponse {
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
            .corner_radius(12.0);

        if focused {
            frame = frame.shadow(egui::epaint::Shadow {
                offset: [0, 0],
                blur: 8,
                spread: 0,
                color: stroke.color,
            });
        }

        frame
            .show(ui, |ui| ui.vertical(|ui| self.input_ui(txn, ui)).inner)
            .inner
    }

    fn input_ui(&mut self, txn: &Transaction, ui: &mut egui::Ui) -> PostResponse {
        let edit_response = ui.horizontal(|ui| self.editbox(txn, ui)).inner;

        let note_response = if let PostType::Quote(id) = self.post_type {
            let avail_size = ui.available_size_before_wrap();
            Some(
                ui.with_layout(Layout::left_to_right(egui::Align::TOP), |ui| {
                    Frame::NONE
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_max_width(avail_size.x * 0.8);

                                let zapping_acc = self
                                    .note_context
                                    .current_account_has_wallet
                                    .then(|| KeypairUnowned::from(&self.poster));

                                render_note_preview(
                                    ui,
                                    self.note_context,
                                    zapping_acc.as_ref(),
                                    txn,
                                    id.bytes(),
                                    nostrdb::NoteKey::new(0),
                                    self.note_options,
                                    self.jobs,
                                )
                            })
                            .inner
                        })
                        .inner
                })
                .inner,
            )
        } else {
            None
        };

        Frame::new()
            .inner_margin(Margin::symmetric(0, 8))
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

        let post_action = ui.horizontal(|ui| self.input_buttons(ui)).inner;

        let action = note_response
            .and_then(|nr| nr.action.map(PostAction::QuotedNoteAction))
            .or(post_action.map(PostAction::NewPostAction));

        PostResponse {
            action,
            edit_response,
        }
    }

    fn input_buttons(&mut self, ui: &mut egui::Ui) -> Option<NewPostAction> {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::BOTTOM), |ui| {
            self.show_upload_media_button(ui);
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::BOTTOM), |ui| {
            let post_button_clicked = ui
                .add_sized([91.0, 32.0], post_button(!self.draft.buffer.is_empty()))
                .clicked();

            let shortcut_pressed = ui.input(|i| {
                (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::Enter)
            });

            if post_button_clicked
                || (!self.draft.buffer.is_empty() && shortcut_pressed && self.focused(ui))
            {
                let output = self.draft.buffer.output();
                let new_post = NewPost::new(
                    output.text,
                    self.poster.to_full(),
                    self.draft.uploaded_media.clone(),
                    output.mentions,
                );
                Some(NewPostAction::new(self.post_type.clone(), new_post))
            } else {
                None
            }
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

            let Some(cache_type) =
                supported_mime_hosted_at_url(&mut self.note_context.img_cache.urls, &media.url)
            else {
                self.draft
                    .upload_errors
                    .push("Uploaded media is not supported.".to_owned());
                error!("Unsupported mime type at url: {}", &media.url);
                continue;
            };

            let url = &media.url;
            let cur_state = get_render_state(
                ui.ctx(),
                self.note_context.img_cache,
                cache_type,
                url,
                notedeck_ui::images::ImageType::Content,
            );

            render_post_view_media(
                ui,
                &mut self.draft.upload_errors,
                &mut to_remove,
                i,
                width,
                height,
                cur_state,
                url,
            )
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

#[allow(clippy::too_many_arguments)]
fn render_post_view_media(
    ui: &mut egui::Ui,
    upload_errors: &mut Vec<String>,
    to_remove: &mut Vec<usize>,
    cur_index: usize,
    width: u32,
    height: u32,
    render_state: RenderState,
    url: &str,
) {
    match render_state.texture_state {
        notedeck::TextureState::Pending => {
            ui.spinner();
        }
        notedeck::TextureState::Error(e) => {
            upload_errors.push(e.to_string());
            error!("{e}");
        }
        notedeck::TextureState::Loaded(renderable_media) => {
            let max_size = 300;
            let size = if width > max_size || height > max_size {
                PixelDimensions { x: 300, y: 300 }
            } else {
                PixelDimensions {
                    x: width,
                    y: height,
                }
            }
            .to_points(ui.pixels_per_point())
            .to_vec();

            let texture_handle = handle_repaint(
                ui,
                retrieve_latest_texture(url, render_state.gifs, renderable_media),
            );
            let img_resp = ui.add(
                egui::Image::new(texture_handle)
                    .max_size(size)
                    .corner_radius(12.0),
            );

            let remove_button_rect = {
                let top_left = img_resp.rect.left_top();
                let spacing = 13.0;
                let center = Pos2::new(top_left.x + spacing, top_left.y + spacing);
                egui::Rect::from_center_size(center, egui::vec2(26.0, 26.0))
            };
            if show_remove_upload_button(ui, remove_button_rect).clicked() {
                to_remove.push(cur_index);
            }
            ui.advance_cursor_after_rect(img_resp.rect);
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
        painter.rect_stroke(resp.rect, 8.0, stroke, egui::StrokeKind::Middle);
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
    use notedeck::{App, AppAction, AppContext};

    pub struct PostPreview {
        draft: Draft,
        poster: FullKeypair,
        jobs: JobsCache,
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
                poster: FullKeypair::generate(),
                jobs: Default::default(),
            }
        }
    }

    impl App for PostPreview {
        fn update(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
            let txn = Transaction::new(app.ndb).expect("txn");
            let mut note_context = NoteContext {
                ndb: app.ndb,
                img_cache: app.img_cache,
                note_cache: app.note_cache,
                zaps: app.zaps,
                pool: app.pool,
                job_pool: app.job_pool,
                unknown_ids: app.unknown_ids,
                current_account_has_wallet: false,
            };

            PostView::new(
                &mut note_context,
                &mut self.draft,
                PostType::New,
                self.poster.to_filled(),
                ui.available_rect_before_wrap(),
                NoteOptions::default(),
                &mut self.jobs,
            )
            .ui(&txn, ui);

            None
        }
    }

    impl Preview for PostView<'_, '_> {
        type Prev = PostPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            PostPreview::new()
        }
    }
}
