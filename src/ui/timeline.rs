use crate::actionbar::{BarAction, NoteActionResponse};
use crate::timeline::TimelineTab;
use crate::{
    column::Columns, imgcache::ImageCache, notecache::NoteCache, timeline::TimelineId, ui,
};
use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{Direction, Layout};
use egui_tabs::TabColor;
use nostrdb::{Ndb, Transaction};
use tracing::{error, warn};

pub struct TimelineView<'a> {
    timeline_id: TimelineId,
    columns: &'a mut Columns,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    textmode: bool,
    reverse: bool,
}

impl<'a> TimelineView<'a> {
    pub fn new(
        timeline_id: TimelineId,
        columns: &'a mut Columns,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        textmode: bool,
    ) -> TimelineView<'a> {
        let reverse = false;
        TimelineView {
            ndb,
            timeline_id,
            columns,
            note_cache,
            img_cache,
            reverse,
            textmode,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> NoteActionResponse {
        timeline_ui(
            ui,
            self.ndb,
            self.timeline_id,
            self.columns,
            self.note_cache,
            self.img_cache,
            self.reverse,
            self.textmode,
        )
    }

    pub fn reversed(mut self) -> Self {
        self.reverse = true;
        self
    }
}

#[allow(clippy::too_many_arguments)]
fn timeline_ui(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    timeline_id: TimelineId,
    columns: &mut Columns,
    note_cache: &mut NoteCache,
    img_cache: &mut ImageCache,
    reversed: bool,
    textmode: bool,
) -> NoteActionResponse {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;

    */

    let scroll_id = {
        let timeline = if let Some(timeline) = columns.find_timeline_mut(timeline_id) {
            timeline
        } else {
            error!("tried to render timeline in column, but timeline was missing");
            // TODO (jb55): render error when timeline is missing?
            // this shouldn't happen...
            return NoteActionResponse::default();
        };

        timeline.selected_view = tabs_ui(ui);

        // need this for some reason??
        ui.add_space(3.0);

        egui::Id::new(("tlscroll", timeline.view_id()))
    };

    egui::ScrollArea::vertical()
        .id_source(scroll_id)
        .animated(false)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            let timeline = if let Some(timeline) = columns.find_timeline_mut(timeline_id) {
                timeline
            } else {
                error!("tried to render timeline in column, but timeline was missing");
                // TODO (jb55): render error when timeline is missing?
                // this shouldn't happen...
                return NoteActionResponse::default();
            };

            let txn = Transaction::new(ndb).expect("failed to create txn");
            TimelineTabView::new(
                timeline.current_view(),
                reversed,
                textmode,
                &txn,
                ndb,
                note_cache,
                img_cache,
            )
            .show(ui)
        })
        .inner
}

pub fn tabs_ui(ui: &mut egui::Ui) -> i32 {
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

pub struct TimelineTabView<'a> {
    tab: &'a TimelineTab,
    reversed: bool,
    textmode: bool,
    txn: &'a Transaction,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
}

impl<'a> TimelineTabView<'a> {
    pub fn new(
        tab: &'a TimelineTab,
        reversed: bool,
        textmode: bool,
        txn: &'a Transaction,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
    ) -> Self {
        Self {
            tab,
            reversed,
            txn,
            textmode,
            ndb,
            note_cache,
            img_cache,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> NoteActionResponse {
        let mut open_profile = None;
        let mut bar_action: Option<BarAction> = None;
        let len = self.tab.notes.len();

        self.tab
            .list
            .clone()
            .borrow_mut()
            .ui_custom_layout(ui, len, |ui, start_index| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().item_spacing.x = 4.0;

                let ind = if self.reversed {
                    len - start_index - 1
                } else {
                    start_index
                };

                let note_key = self.tab.notes[ind].key;

                let note = if let Ok(note) = self.ndb.get_note_by_key(self.txn, note_key) {
                    note
                } else {
                    warn!("failed to query note {:?}", note_key);
                    return 0;
                };

                ui::padding(8.0, ui, |ui| {
                    let resp = ui::NoteView::new(self.ndb, self.note_cache, self.img_cache, &note)
                        .note_previews(!self.textmode)
                        .selectable_text(false)
                        .options_button(true)
                        .show(ui);

                    bar_action = bar_action.or(resp.action.bar_action);
                    open_profile = open_profile.or(resp.action.open_profile);

                    if let Some(context) = resp.context_selection {
                        context.process(ui, &note);
                    }
                });

                ui::hline(ui);
                //ui.add(egui::Separator::default().spacing(0.0));

                1
            });

        NoteActionResponse {
            open_profile,
            bar_action,
        }
    }
}
