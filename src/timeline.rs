use crate::notecache::CachedNote;
use crate::{ui, Damus};

use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{Direction, Layout};
use egui_tabs::TabColor;
use egui_virtual_list::VirtualList;
use enostr::Filter;
use nostrdb::{Note, NoteKey, Subscription, Transaction};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use log::{debug, warn};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct NoteRef {
    pub key: NoteKey,
    pub created_at: u64,
}

impl Ord for NoteRef {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.created_at.cmp(&other.created_at) {
            Ordering::Equal => self.key.cmp(&other.key),
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        }
    }
}

impl PartialOrd for NoteRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum ViewFilter {
    Notes,
    NotesAndReplies,
}

impl ViewFilter {
    pub fn name(&self) -> &'static str {
        match self {
            ViewFilter::Notes => "Notes",
            ViewFilter::NotesAndReplies => "Notes & Replies",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            ViewFilter::Notes => 0,
            ViewFilter::NotesAndReplies => 1,
        }
    }

    pub fn filter_notes(cache: &CachedNote, note: &Note) -> bool {
        !cache.reply.borrow(note.tags()).is_reply()
    }

    fn identity(_cache: &CachedNote, _note: &Note) -> bool {
        true
    }

    pub fn filter(&self) -> fn(&CachedNote, &Note) -> bool {
        match self {
            ViewFilter::Notes => ViewFilter::filter_notes,
            ViewFilter::NotesAndReplies => ViewFilter::identity,
        }
    }
}

/// A timeline view is a filtered view of notes in a timeline. Two standard views
/// are "Notes" and "Notes & Replies". A timeline is associated with a Filter,
/// but a TimelineView is a further filtered view of this Filter that can't
/// be captured by a Filter itself.
pub struct TimelineView {
    pub notes: Vec<NoteRef>,
    pub selection: i32,
    pub filter: ViewFilter,
    pub list: Rc<RefCell<VirtualList>>,
}

impl TimelineView {
    pub fn new(filter: ViewFilter) -> Self {
        let selection = 0i32;
        let mut list = VirtualList::new();
        list.hide_on_resize(None);
        let list = Rc::new(RefCell::new(list));
        let notes: Vec<NoteRef> = Vec::with_capacity(1000);

        TimelineView {
            notes,
            selection,
            filter,
            list,
        }
    }

    pub fn select_down(&mut self) {
        debug!("select_down {}", self.selection + 1);
        if self.selection + 1 > self.notes.len() as i32 {
            return;
        }

        self.selection += 1;
    }

    pub fn select_up(&mut self) {
        debug!("select_up {}", self.selection - 1);
        if self.selection - 1 < 0 {
            return;
        }

        self.selection -= 1;
    }
}

pub struct Timeline {
    pub filter: Vec<Filter>,
    pub views: Vec<TimelineView>,
    pub selected_view: i32,

    /// Our nostrdb subscription
    pub subscription: Option<Subscription>,
}

impl Timeline {
    pub fn new(filter: Vec<Filter>) -> Self {
        let subscription: Option<Subscription> = None;
        let notes = TimelineView::new(ViewFilter::Notes);
        let replies = TimelineView::new(ViewFilter::NotesAndReplies);
        let views = vec![notes, replies];
        let selected_view = 0;

        Timeline {
            filter,
            views,
            subscription,
            selected_view,
        }
    }

    pub fn current_view(&self) -> &TimelineView {
        &self.views[self.selected_view as usize]
    }

    pub fn current_view_mut(&mut self) -> &mut TimelineView {
        &mut self.views[self.selected_view as usize]
    }

    pub fn notes(&self, view: ViewFilter) -> &[NoteRef] {
        &self.views[view.index()].notes
    }

    pub fn view(&self, view: ViewFilter) -> &TimelineView {
        &self.views[view.index()]
    }

    pub fn view_mut(&mut self, view: ViewFilter) -> &mut TimelineView {
        &mut self.views[view.index()]
    }
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

fn tabs_ui(timeline: &mut Timeline, ui: &mut egui::Ui) {
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

    let sel = if let Some(sel) = tab_res.selected() {
        sel
    } else {
        0
    };

    // fun animation
    timeline.selected_view = sel;

    let (underline, underline_y) = tab_res.inner()[sel as usize].inner;
    let underline_width = underline.span();

    let tab_anim_id = ui.id().with("tab_anim");
    let tab_anim_size = tab_anim_id.with("size");

    let stroke = egui::Stroke {
        color: ui.visuals().hyperlink_color,
        width: 3.0,
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
}

pub fn timeline_view(ui: &mut egui::Ui, app: &mut Damus, timeline: usize) {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;
    */

    tabs_ui(&mut app.timelines[timeline], ui);

    // need this for some reason??
    ui.add_space(3.0);

    let scroll_id = ui.id().with(app.timelines[timeline].selected_view);
    egui::ScrollArea::vertical()
        .id_source(scroll_id)
        .animated(false)
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            let view = app.timelines[timeline].current_view();
            let len = view.notes.len();
            view.list
                .clone()
                .borrow_mut()
                .ui_custom_layout(ui, len, |ui, start_index| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let note_key = app.timelines[timeline].current_view().notes[start_index].key;

                    let txn = if let Ok(txn) = Transaction::new(&app.ndb) {
                        txn
                    } else {
                        warn!("failed to create transaction for {:?}", note_key);
                        return 0;
                    };

                    let note = if let Ok(note) = app.ndb.get_note_by_key(&txn, note_key) {
                        note
                    } else {
                        warn!("failed to query note {:?}", note_key);
                        return 0;
                    };

                    ui::padding(8.0, ui, |ui| {
                        let textmode = app.textmode;
                        ui.add(ui::Note::new(app, &note).note_previews(!textmode));
                    });
                    ui::hline(ui);
                    //ui.add(egui::Separator::default().spacing(0.0));

                    1
                });
        });
}

pub enum MergeKind {
    FrontInsert,
    Spliced,
}

pub fn merge_sorted_vecs<T: Ord + Copy>(vec1: &[T], vec2: &[T]) -> (Vec<T>, MergeKind) {
    let mut merged = Vec::with_capacity(vec1.len() + vec2.len());
    let mut i = 0;
    let mut j = 0;
    let mut result: Option<MergeKind> = None;

    while i < vec1.len() && j < vec2.len() {
        if vec1[i] <= vec2[j] {
            if result.is_none() && j < vec2.len() {
                // if we're pushing from our large list and still have
                // some left in vec2, then this is a splice
                result = Some(MergeKind::Spliced);
            }
            merged.push(vec1[i]);
            i += 1;
        } else {
            merged.push(vec2[j]);
            j += 1;
        }
    }

    // Append any remaining elements from either vector
    if i < vec1.len() {
        merged.extend_from_slice(&vec1[i..]);
    }
    if j < vec2.len() {
        merged.extend_from_slice(&vec2[j..]);
    }

    (merged, result.unwrap_or(MergeKind::FrontInsert))
}
