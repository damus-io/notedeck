use crate::draft::Draft;
use crate::imgcache::ImageCache;
use crate::notecache::NoteCache;
use crate::ui;
use crate::ui::note::PostResponse;
use enostr::FilledKeypair;
use nostrdb::Ndb;

pub struct PostReplyView<'a> {
    ndb: &'a Ndb,
    poster: FilledKeypair<'a>,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    draft: &'a mut Draft,
    note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
}

impl<'a> PostReplyView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        poster: FilledKeypair<'a>,
        draft: &'a mut Draft,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        note: &'a nostrdb::Note<'a>,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        PostReplyView {
            ndb,
            poster,
            draft,
            note,
            note_cache,
            img_cache,
            id_source,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = Some(id);
        self
    }

    pub fn id(&self) -> egui::Id {
        self.id_source
            .unwrap_or_else(|| egui::Id::new("post-reply-view"))
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        ui.vertical(|ui| {
            let avail_rect = ui.available_rect_before_wrap();

            // This is the offset of the post view's pfp. We use this
            // to indent things so that the reply line is aligned
            let pfp_offset = ui::PostView::outer_margin()
                + ui::PostView::inner_margin()
                + ui::ProfilePic::small_size() / 2.0;

            let note_offset = pfp_offset
                - ui::ProfilePic::medium_size() / 2.0
                - ui::NoteView::expand_size() / 2.0;

            egui::Frame::none()
                .outer_margin(egui::Margin::same(note_offset))
                .show(ui, |ui| {
                    ui::NoteView::new(self.ndb, self.note_cache, self.img_cache, self.note)
                        .actionbar(false)
                        .medium_pfp(true)
                        .use_more_options_button(true)
                        .show(ui);
                });

            let id = self.id();
            let replying_to = self.note.id();
            let rect_before_post = ui.min_rect();

            let post_response = {
                ui::PostView::new(
                    self.ndb,
                    self.draft,
                    crate::draft::DraftSource::Reply(replying_to),
                    self.img_cache,
                    self.note_cache,
                    self.poster,
                )
                .id_source(id)
                .ui(self.note.txn().unwrap(), ui)
            };

            //
            // reply line
            //

            // Position and draw the reply line
            let mut rect = ui.min_rect();

            // Position the line right above the poster's profile pic in
            // the post box. Use the PostView's margin values to
            // determine this offset.
            rect.min.x = avail_rect.min.x + pfp_offset;

            // honestly don't know what the fuck I'm doing here. just trying
            // to get the line under the profile picture
            rect.min.y = avail_rect.min.y
                + (ui::ProfilePic::medium_size() / 2.0
                    + ui::ProfilePic::medium_size()
                    + ui::NoteView::expand_size() * 2.0)
                + 1.0;

            // For some reason we need to nudge the reply line's height a
            // few more pixels?
            let nudge = if post_response.edit_response.has_focus() {
                // we nudge by one less pixel if focused, otherwise it
                // overlaps the focused PostView purple border color
                2.0
            } else {
                // we have to nudge by one more pixel when not focused
                // otherwise it looks like there's a gap(?)
                3.0
            };

            rect.max.y = rect_before_post.max.y + ui::PostView::outer_margin() + nudge;

            ui.painter().vline(
                rect.left(),
                rect.y_range(),
                ui.visuals().widgets.noninteractive.bg_stroke,
            );

            post_response
        })
        .inner
    }
}
