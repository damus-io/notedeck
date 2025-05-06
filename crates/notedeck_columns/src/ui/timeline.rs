use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{vec2, Direction, Layout, Pos2, Stroke};
use egui_tabs::TabColor;
use enostr::KeypairUnowned;
use nostrdb::Transaction;
use std::f32::consts::PI;
use tracing::{error, warn};

use crate::timeline::{TimelineCache, TimelineKind, TimelineTab, ViewFilter};
use notedeck::{note::root_note_id_from_selected_id, MuteFun, NoteAction, NoteContext, UnknownIds};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    show_pointer, NoteOptions, NoteView,
};

pub struct TimelineView<'a, 'd> {
    timeline_id: &'a TimelineKind,
    timeline_cache: &'a mut TimelineCache,
    note_options: NoteOptions,
    reverse: bool,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    unknown_ids: &'a mut UnknownIds,
}

impl<'a, 'd> TimelineView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_id: &'a TimelineKind,
        timeline_cache: &'a mut TimelineCache,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        note_options: NoteOptions,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        unknown_ids: &'a mut UnknownIds,
    ) -> Self {
        let reverse = false;
        TimelineView {
            timeline_id,
            timeline_cache,
            note_options,
            reverse,
            is_muted,
            note_context,
            cur_acc,
            unknown_ids,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        timeline_ui(
            ui,
            self.timeline_id,
            self.timeline_cache,
            self.reverse,
            self.note_options,
            self.is_muted,
            self.note_context,
            self.cur_acc,
            self.unknown_ids,
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
    timeline_id: &TimelineKind,
    timeline_cache: &mut TimelineCache,
    reversed: bool,
    note_options: NoteOptions,
    is_muted: &MuteFun,
    note_context: &mut NoteContext,
    cur_acc: &Option<KeypairUnowned>,
    unknown_ids: &mut UnknownIds,
) -> Option<NoteAction> {
    let mut note_action: Option<NoteAction> = None;

    let timeline = if let Some(timeline) = timeline_cache.timelines.get_mut(timeline_id) {
        timeline
    } else {
        error!("tried to render timeline in column, but timeline was missing");
        return None;
    };

    timeline.selected_view = tabs_ui(ui, timeline.selected_view, &timeline.views);
    ui.add_space(3.0);

    if !timeline.pending_notes.is_empty() {
        ui.vertical_centered(|ui| {
            let button_text = format!("Load {} new notes", timeline.pending_notes.len());
            let button = egui::Button::new(button_text).fill(ui.visuals().widgets.active.bg_fill);
            if ui.add(button).clicked() {
                match Transaction::new(note_context.ndb) {
                    Ok(txn) => {
                        if let Err(e) = timeline.apply_pending_notes(
                            note_context.ndb,
                            &txn,
                            unknown_ids,
                            note_context.note_cache,
                        ) {
                            error!("Failed to apply pending notes: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Failed to create transaction for applying notes: {}", e);
                    }
                }
            }
        });
        ui.add_space(5.0);
    }

    let scroll_id = egui::Id::new(("tlscroll", timeline.view_id()));

    let show_top_button_id = ui.id().with((scroll_id, "at_top"));

    let show_top_button = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(show_top_button_id))
        .unwrap_or(false);

    let goto_top_resp = if show_top_button {
        let top_button_pos = ui.available_rect_before_wrap().right_top() - vec2(48.0, -24.0);
        egui::Area::new(ui.id().with("foreground_area"))
            .order(egui::Order::Middle)
            .fixed_pos(top_button_pos)
            .show(ui.ctx(), |ui| Some(ui.add(goto_top_button(top_button_pos))))
            .inner
    } else {
        None
    };

    let mut scroll_area = egui::ScrollArea::vertical()
        .id_salt(scroll_id)
        .animated(false)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible);

    if let Some(goto_top_resp) = goto_top_resp {
        if goto_top_resp.clicked() {
            scroll_area = scroll_area.vertical_scroll_offset(0.0);
        } else if goto_top_resp.hovered() {
            show_pointer(ui);
        }
    }

    let scroll_output = scroll_area.show(ui, |ui| {
        let timeline = if let Some(timeline) = timeline_cache.timelines.get(timeline_id) {
            timeline
        } else {
            error!("tried to render timeline in column, but timeline was missing");
            return None;
        };

        let txn = Transaction::new(note_context.ndb).expect("failed to create txn");

        TimelineTabView::new(
            timeline.current_view(),
            reversed,
            note_options,
            &txn,
            is_muted,
            note_context,
            cur_acc,
        )
        .show(ui)
    });

    let at_top_after_scroll = scroll_output.state.offset.y == 0.0;
    let cur_show_top_button = ui.ctx().data(|d| d.get_temp::<bool>(show_top_button_id));

    if at_top_after_scroll {
        if cur_show_top_button != Some(false) {
            ui.ctx()
                .data_mut(|d| d.insert_temp(show_top_button_id, false));
        }
    } else if cur_show_top_button == Some(false) {
        ui.ctx()
            .data_mut(|d| d.insert_temp(show_top_button_id, true));
    }

    scroll_output.inner.or(note_action)
}

fn goto_top_button(center: Pos2) -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let radius = 12.0;
        let max_size = vec2(
            ICON_EXPANSION_MULTIPLE * 2.0 * radius,
            ICON_EXPANSION_MULTIPLE * 2.0 * radius,
        );
        let helper = AnimationHelper::new_from_rect(ui, "goto_top", {
            let painter = ui.painter();
            #[allow(deprecated)]
            let center = painter.round_pos_to_pixel_center(center);
            egui::Rect::from_center_size(center, max_size)
        });

        let painter = ui.painter();
        painter.circle_filled(
            center,
            helper.scale_1d_pos(radius),
            notedeck_ui::colors::PINK,
        );

        let create_pt = |angle: f32| {
            let side = radius / 2.0;
            let x = side * angle.cos();
            let mut y = side * angle.sin();

            let height = (side * (3.0_f32).sqrt()) / 2.0;
            y += height / 2.0;
            Pos2 { x, y }
        };

        #[allow(deprecated)]
        let left_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(-PI)));
        #[allow(deprecated)]
        let center_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(-PI / 2.0)));
        #[allow(deprecated)]
        let right_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(0.0)));

        let line_width = helper.scale_1d_pos(4.0);
        let line_color = ui.visuals().text_color();
        painter.line_segment([left_pt, center_pt], Stroke::new(line_width, line_color));
        painter.line_segment([center_pt, right_pt], Stroke::new(line_width, line_color));

        let end_radius = (line_width - 1.0) / 2.0;
        painter.circle_filled(left_pt, end_radius, line_color);
        painter.circle_filled(center_pt, end_radius, line_color);
        painter.circle_filled(right_pt, end_radius, line_color);

        helper.take_animation_response()
    }
}

pub fn tabs_ui(ui: &mut egui::Ui, selected: usize, views: &[TimelineTab]) -> usize {
    ui.spacing_mut().item_spacing.y = 0.0;

    let tab_res = egui_tabs::Tabs::new(views.len() as i32)
        .selected(selected as i32)
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

            let txt = match views[ind as usize].filter {
                ViewFilter::Notes => "Notes",
                ViewFilter::NotesAndReplies => "Notes & Replies",
            };

            let res = ui.add(egui::Label::new(txt).selectable(false));

            // underline
            if state.is_selected() {
                let rect = res.rect;
                let underline =
                    shrink_range_to_width(rect.x_range(), get_label_width(ui, txt) * 1.15);
                #[allow(deprecated)]
                let underline_y = ui.painter().round_to_pixel(rect.bottom()) - 1.5;
                return (underline, underline_y);
            }

            (egui::Rangef::new(0.0, 0.0), 0.0)
        });

    //ui.add_space(0.5);
    notedeck_ui::hline(ui);

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

    sel as usize
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

pub struct TimelineTabView<'a, 'd> {
    tab: &'a TimelineTab,
    reversed: bool,
    note_options: NoteOptions,
    txn: &'a Transaction,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
}

impl<'a, 'd> TimelineTabView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab: &'a TimelineTab,
        reversed: bool,
        note_options: NoteOptions,
        txn: &'a Transaction,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
    ) -> Self {
        Self {
            tab,
            reversed,
            note_options,
            txn,
            is_muted,
            note_context,
            cur_acc,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let mut action: Option<NoteAction> = None;
        let len = self.tab.notes.len();

        let is_muted = self.is_muted;
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

                let note =
                    if let Ok(note) = self.note_context.ndb.get_note_by_key(self.txn, note_key) {
                        note
                    } else {
                        warn!("failed to query note {:?}", note_key);
                        return 0;
                    };

                // should we mute the thread? we might not have it!
                let muted = if let Ok(root_id) = root_note_id_from_selected_id(
                    self.note_context.ndb,
                    self.note_context.note_cache,
                    self.txn,
                    note.id(),
                ) {
                    is_muted(&note, root_id.bytes())
                } else {
                    false
                };

                if !muted {
                    notedeck_ui::padding(8.0, ui, |ui| {
                        let resp = NoteView::new(
                            self.note_context,
                            self.cur_acc,
                            &note,
                            self.note_options,
                        )
                        .show(ui);

                        if let Some(note_action) = resp.action {
                            action = Some(note_action)
                        }
                    });

                    notedeck_ui::hline(ui);
                }

                1
            });

        action
    }
}
