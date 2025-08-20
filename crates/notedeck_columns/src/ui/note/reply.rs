use crate::draft::Draft;
use crate::ui::{
    self,
    note::{PostAction, PostResponse, PostType},
};

use egui::{Rect, Response, ScrollArea, Ui};
use enostr::{FilledKeypair, NoteId};
use notedeck::{JobsCache, NoteContext};
use notedeck_ui::{NoteOptions, NoteView, ProfilePic};

pub struct PostReplyView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    note: &'a nostrdb::Note<'a>,
    scroll_id: egui::Id,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
    jobs: &'a mut JobsCache,
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
        jobs: &'a mut JobsCache,
        col: usize,
    ) -> Self {
        PostReplyView {
            note_context,
            poster,
            draft,
            note,
            scroll_id: PostReplyView::scroll_id(col, note.id()),
            inner_rect,
            note_options,
            jobs,
        }
    }

    fn id(col: usize, note_id: &[u8; 32]) -> egui::Id {
        egui::Id::new(("reply_view", col, note_id))
    }

    pub fn scroll_id(col: usize, note_id: &[u8; 32]) -> egui::Id {
        PostReplyView::id(col, note_id).with("scroll")
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        ScrollArea::vertical()
            .id_salt(self.scroll_id)
            .stick_to_bottom(true)
            .show(ui, |ui| self.show_internal(ui))
            .inner
    }

    // no scroll
    fn show_internal(&mut self, ui: &mut egui::Ui) -> PostResponse {
        ui.vertical(|ui| {
            let avail_rect = ui.available_rect_before_wrap();

            // This is the offset of the post view's pfp. We use this
            // to indent things so that the reply line is aligned
            let pfp_offset: i8 = ui::PostView::outer_margin()
                + ui::PostView::inner_margin()
                + ProfilePic::small_size() / 2;

            let note_offset: i8 =
                pfp_offset - ProfilePic::medium_size() / 2 - NoteView::expand_size() / 2;

            let quoted_note = egui::Frame::NONE
                .outer_margin(egui::Margin::same(note_offset))
                .show(ui, |ui| {
                    NoteView::new(self.note_context, self.note, self.note_options, self.jobs)
                        .truncate(false)
                        .selectable_text(true)
                        .actionbar(false)
                        .medium_pfp(true)
                        .options_button(true)
                        .show(ui)
                })
                .inner;

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
                    self.jobs,
                )
                .ui_no_scroll(self.note.txn().unwrap(), ui)
            };

            post_response.action = post_response
                .action
                .or(quoted_note.action.map(PostAction::QuotedNoteAction));

            reply_line_ui(
                &rect_before_post,
                &post_response.edit_response,
                pfp_offset as f32,
                &avail_rect,
                ui,
            );

            //
            // NOTE(jb55): We add some space so that you can scroll to
            // put the input box higher.  This can happen in some
            // situations where the input box gets covered or if its too
            // large and things start breaking. I think this is an ok
            // solution but there could be a better one.
            //
            //ui.add_space(500.0);

            post_response
        })
        .inner
    }
}

/// The vertical line in the reply view
fn reply_line_ui(
    rect_before_post: &Rect,
    edit_response: &Response,
    pfp_offset: f32,
    avail_rect: &Rect,
    ui: &mut Ui,
) {
    // Position and draw the reply line
    let mut rect = ui.min_rect();

    // Position the line right above the poster's profile pic in
    // the post box. Use the PostView's margin values to
    // determine this offset.
    rect.min.x = avail_rect.min.x + pfp_offset;

    // honestly don't know what the fuck I'm doing here. just trying
    // to get the line under the profile picture
    rect.min.y = avail_rect.min.y
        + (ProfilePic::medium_size() as f32 / 2.0
            + ProfilePic::medium_size() as f32
            + NoteView::expand_size() as f32 * 2.0)
        + 1.0;

    // For some reason we need to nudge the reply line's height a
    // few more pixels?
    let nudge = if edit_response.has_focus() {
        // we nudge by one less pixel if focused, otherwise it
        // overlaps the focused PostView purple border color
        2.0
    } else {
        // we have to nudge by one more pixel when not focused
        // otherwise it looks like there's a gap(?)
        3.0
    };

    rect.max.y = rect_before_post.max.y + ui::PostView::outer_margin() as f32 + nudge;

    ui.painter().vline(
        rect.left(),
        rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}
