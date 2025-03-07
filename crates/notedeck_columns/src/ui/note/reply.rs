use crate::draft::Draft;
use crate::ui;
use crate::ui::note::{PostResponse, PostType};
use enostr::{FilledKeypair, NoteId};

use super::contents::NoteContext;
use super::NoteOptions;

pub struct PostReplyView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
}

impl<'a, 'd> PostReplyView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        poster: FilledKeypair<'a>,
        draft: &'a mut Draft,
        note: &'a nostrdb::Note<'a>,
        inner_rect: egui::Rect,
        note_options: NoteOptions,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        PostReplyView {
            note_context,
            poster,
            draft,
            note,
            id_source,
            inner_rect,
            note_options,
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

            let selection = egui::Frame::none()
                .outer_margin(egui::Margin::same(note_offset))
                .show(ui, |ui| {
                    ui::NoteView::new(self.note_context, self.note, self.note_options)
                        .actionbar(false)
                        .medium_pfp(true)
                        .options_button(true)
                        .show(ui)
                })
                .inner
                .context_selection;

            let id = self.id();
            let replying_to = self.note.id();
            let rect_before_post = ui.min_rect();

            let mut post_response = {
                ui::PostView::new(
                    self.note_context,
                    self.draft,
                    PostType::Reply(NoteId::new(*replying_to)),
                    self.poster,
                    self.inner_rect,
                    self.note_options,
                )
                .id_source(id)
                .ui(self.note.txn().unwrap(), ui)
            };

            post_response.context_selection = selection;

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
