use crate::{actionbar::BarResult, draft::DraftSource, ui, ui::note::PostAction, Damus};
use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{Direction, Layout};
use egui_tabs::TabColor;
use nostrdb::Transaction;
use tracing::{debug, info, warn};

pub struct TimelineView<'a> {
    app: &'a mut Damus,
    reverse: bool,
    timeline: usize,
}

impl<'a> TimelineView<'a> {
    pub fn new(app: &'a mut Damus, timeline: usize) -> TimelineView<'a> {
        let reverse = false;
        TimelineView {
            app,
            timeline,
            reverse,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        timeline_ui(ui, self.app, self.timeline, self.reverse);
    }

    pub fn reversed(mut self) -> Self {
        self.reverse = true;
        self
    }
}

fn timeline_ui(ui: &mut egui::Ui, app: &mut Damus, timeline: usize, reversed: bool) {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;
    */

    if timeline == 0 {
        postbox_view(app, ui);
    }

    app.timelines[timeline].selected_view = tabs_ui(ui);

    // need this for some reason??
    ui.add_space(3.0);

    let scroll_id = egui::Id::new(("tlscroll", app.timelines[timeline].selected_view, timeline));
    egui::ScrollArea::vertical()
        .id_source(scroll_id)
        .animated(false)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            let view = app.timelines[timeline].current_view();
            let len = view.notes.len();
            let mut bar_result: Option<BarResult> = None;
            let txn = if let Ok(txn) = Transaction::new(&app.ndb) {
                txn
            } else {
                warn!("failed to create transaction");
                return 0;
            };

            view.list
                .clone()
                .borrow_mut()
                .ui_custom_layout(ui, len, |ui, start_index| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let ind = if reversed {
                        len - start_index - 1
                    } else {
                        start_index
                    };

                    let note_key = app.timelines[timeline].current_view().notes[ind].key;

                    let note = if let Ok(note) = app.ndb.get_note_by_key(&txn, note_key) {
                        note
                    } else {
                        warn!("failed to query note {:?}", note_key);
                        return 0;
                    };

                    ui::padding(8.0, ui, |ui| {
                        let textmode = app.textmode;
                        let resp = ui::NoteView::new(app, &note)
                            .note_previews(!textmode)
                            .show(ui);

                        if let Some(action) = resp.action {
                            let br = action.execute(app, timeline, note.id(), &txn);
                            if br.is_some() {
                                bar_result = br;
                            }
                        } else if resp.response.clicked() {
                            debug!("clicked note");
                        }
                    });

                    ui::hline(ui);
                    //ui.add(egui::Separator::default().spacing(0.0));

                    1
                });

            if let Some(br) = bar_result {
                match br {
                    // update the thread for next render if we have new notes
                    BarResult::NewThreadNotes(new_notes) => {
                        let thread = app
                            .threads
                            .thread_mut(&app.ndb, &txn, new_notes.root_id.bytes())
                            .get_ptr();
                        new_notes.process(thread);
                    }
                }
            }

            1
        });
}

fn postbox_view(app: &mut Damus, ui: &mut egui::Ui) {
    // show a postbox in the first timeline

    if let Some(account) = app.account_manager.get_selected_account_index() {
        if app
            .account_manager
            .get_selected_account()
            .map_or(false, |a| a.secret_key.is_some())
        {
            if let Ok(txn) = Transaction::new(&app.ndb) {
                let response = ui::PostView::new(app, DraftSource::Compose, account).ui(&txn, ui);

                if let Some(action) = response.action {
                    match action {
                        PostAction::Post(np) => {
                            let seckey = app
                                .account_manager
                                .get_account(account)
                                .unwrap()
                                .secret_key
                                .as_ref()
                                .unwrap()
                                .to_secret_bytes();

                            let note = np.to_note(&seckey);
                            let raw_msg = format!("[\"EVENT\",{}]", note.json().unwrap());
                            info!("sending {}", raw_msg);
                            app.pool.send(&enostr::ClientMessage::raw(raw_msg));
                            app.drafts.clear(DraftSource::Compose);
                        }
                    }
                }
            }
        }
    }
}

fn tabs_ui(ui: &mut egui::Ui) -> i32 {
    ui.spacing_mut().item_spacing.y = 0.0;

    let tab_res = egui_tabs::Tabs::new(2)
        .selected(1)
        .hover_bg(TabColor::none())
        .selected_fg(TabColor::none())
        .selected_bg(TabColor::none())
        .hover_bg(TabColor::none())
        //.hover_bg(TabColor::custom(egui::Color32::RED))
        .height(32.0)
        .layout(Layout::centered_and_justified(Direction::TopDown))
        .show(ui, |ui, state| {
            ui.spacing_mut().item_spacing.y = 0.0;

            let ind = state.index();

            let txt = if ind == 0 { "Notes" } else { "Notes & Replies" };

            let res = ui.add(egui::Label::new(txt).selectable(false));

            // underline
            if state.is_selected() {
                let rect = res.rect;
                let underline =
                    shrink_range_to_width(rect.x_range(), get_label_width(ui, txt) * 1.15);
                let underline_y = ui.painter().round_to_pixel(rect.bottom()) - 1.5;
                return (underline, underline_y);
            }

            (egui::Rangef::new(0.0, 0.0), 0.0)
        });

    //ui.add_space(0.5);
    ui::hline(ui);

    let sel = tab_res.selected().unwrap_or_default();

    let (underline, underline_y) = tab_res.inner()[sel as usize].inner;
    let underline_width = underline.span();

    let tab_anim_id = ui.id().with("tab_anim");
    let tab_anim_size = tab_anim_id.with("size");

    let stroke = egui::Stroke {
        color: ui.visuals().hyperlink_color,
        width: 2.0,
    };

    let speed = 0.1f32;

    // animate underline position
    let x = ui
        .ctx()
        .animate_value_with_time(tab_anim_id, underline.min, speed);

    // animate underline width
    let w = ui
        .ctx()
        .animate_value_with_time(tab_anim_size, underline_width, speed);

    let underline = egui::Rangef::new(x, x + w);

    ui.painter().hline(underline, underline_y, stroke);

    sel
}

fn get_label_width(ui: &mut egui::Ui, text: &str) -> f32 {
    let font_id = egui::FontId::default();
    let galley = ui.fonts(|r| r.layout_no_wrap(text.to_string(), font_id, egui::Color32::WHITE));
    galley.rect.width()
}

fn shrink_range_to_width(range: egui::Rangef, width: f32) -> egui::Rangef {
    let midpoint = (range.min + range.max) / 2.0;
    let half_width = width / 2.0;

    let min = midpoint - half_width;
    let max = midpoint + half_width;

    egui::Rangef::new(min, max)
}
