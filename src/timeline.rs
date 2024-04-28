use crate::{ui, Damus};
use egui::containers::scroll_area::ScrollBarVisibility;
use egui_virtual_list::VirtualList;
use enostr::Filter;
use nostrdb::{NoteKey, Subscription, Transaction};
use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

use log::warn;

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

pub struct Timeline {
    pub filter: Vec<Filter>,
    pub notes: Vec<NoteRef>,

    /// Our nostrdb subscription
    pub subscription: Option<Subscription>,

    /// State for our virtual list egui widget
    pub list: Arc<Mutex<VirtualList>>,
}

impl Timeline {
    pub fn new(filter: Vec<Filter>) -> Self {
        let notes: Vec<NoteRef> = Vec::with_capacity(1000);
        let subscription: Option<Subscription> = None;
        let list = Arc::new(Mutex::new(VirtualList::new()));

        Timeline {
            filter,
            notes,
            subscription,
            list,
        }
    }
}

pub fn timeline_view(ui: &mut egui::Ui, app: &mut Damus, timeline: usize) {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;
    */

    egui::ScrollArea::vertical()
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
        //.auto_shrink([false; 2])
        /*
        .show_viewport(ui, |ui, viewport| {
            render_notes_in_viewport(ui, app, viewport, row_height, font_id);
        });
        */
        .show(ui, |ui| {
            let len = app.timelines[timeline].notes.len();
            let list = app.timelines[timeline].list.clone();
            list.lock()
                .unwrap()
                .ui_custom_layout(ui, len, |ui, start_index| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let note_key = app.timelines[timeline].notes[start_index].key;

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

                    let note_ui = ui::Note::new(app, &note);
                    ui.add(note_ui);
                    ui.add(egui::Separator::default().spacing(0.0));

                    1
                });
        });
}

pub fn merge_sorted_vecs<T: Ord + Copy>(vec1: &[T], vec2: &[T]) -> Vec<T> {
    let mut merged = Vec::with_capacity(vec1.len() + vec2.len());
    let mut i = 0;
    let mut j = 0;

    while i < vec1.len() && j < vec2.len() {
        if vec1[i] <= vec2[j] {
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

    merged
}
