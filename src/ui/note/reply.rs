use crate::draft::DraftSource;
use crate::ui::note::{PostAction, PostResponse};
use crate::{ui, Damus};
use tracing::info;

pub struct PostReplyView<'a> {
    app: &'a mut Damus,
    id_source: Option<egui::Id>,
    note: &'a nostrdb::Note<'a>,
}

impl<'a> PostReplyView<'a> {
    pub fn new(app: &'a mut Damus, note: &'a nostrdb::Note<'a>) -> Self {
        let id_source: Option<egui::Id> = None;
        PostReplyView {
            app,
            id_source,
            note,
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

            let note_offset =
                pfp_offset - ui::ProfilePic::medium_size() / 2.0 - ui::Note::expand_size() / 2.0;

            egui::Frame::none()
                .outer_margin(egui::Margin::same(note_offset))
                .show(ui, |ui| {
                    ui::Note::new(self.app, self.note)
                        .actionbar(false)
                        .medium_pfp(true)
                        .show(ui);
                });

            let id = self.id();
            let replying_to = self.note.id();
            let draft_source = DraftSource::Reply(replying_to);
            let poster = self
                .app
                .account_manager
                .get_selected_account_index()
                .unwrap_or(0);
            let rect_before_post = ui.min_rect();
            let post_response = ui::PostView::new(self.app, draft_source, poster)
                .id_source(id)
                .ui(self.note.txn().unwrap(), ui);

            if self
                .app
                .account_manager
                .get_selected_account()
                .map_or(false, |a| a.secret_key.is_some())
            {
                if let Some(action) = &post_response.action {
                    match action {
                        PostAction::Post(np) => {
                            let seckey = self
                                .app
                                .account_manager
                                .get_account(poster)
                                .unwrap()
                                .secret_key
                                .as_ref()
                                .unwrap()
                                .to_secret_bytes();

                            let note = np.to_reply(&seckey, self.note);

                            let raw_msg = format!("[\"EVENT\",{}]", note.json().unwrap());
                            info!("sending {}", raw_msg);
                            self.app.pool.send(&enostr::ClientMessage::raw(raw_msg));
                            self.app.drafts.clear(DraftSource::Reply(replying_to));
                        }
                    }
                }
            }

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
                    + ui::Note::expand_size() * 2.0)
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
