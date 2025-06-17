use egui::InnerResponse;
use egui_virtual_list::VirtualList;
use enostr::KeypairUnowned;
use nostrdb::{Note, Transaction};
use notedeck::note::root_note_id_from_selected_id;
use notedeck::{MuteFun, NoteAction, NoteContext, RootNoteId, UnknownIds};
use notedeck_ui::jobs::JobsCache;
use notedeck_ui::note::NoteResponse;
use notedeck_ui::{NoteOptions, NoteView};
use tracing::error;

use crate::timeline::thread::NoteSeenFlags;
use crate::timeline::{ThreadSelection, TimelineCache, TimelineKind};
use crate::ui::timeline::TimelineTabView;

pub struct ThreadView<'a, 'd> {
    timeline_cache: &'a mut TimelineCache,
    unknown_ids: &'a mut UnknownIds,
    selected_note_id: &'a [u8; 32],
    note_options: NoteOptions,
    id_source: egui::Id,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> ThreadView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_cache: &'a mut TimelineCache,
        unknown_ids: &'a mut UnknownIds,
        selected_note_id: &'a [u8; 32],
        note_options: NoteOptions,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            timeline_cache,
            unknown_ids,
            selected_note_id,
            note_options,
            id_source,
            is_muted,
            note_context,
            cur_acc,
            jobs,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.note_context.ndb).expect("txn");

        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);

        let offset_id = self
            .id_source
            .with(("scroll_offset", self.selected_note_id));

        if let Some(offset) = ui.data(|i| i.get_temp::<f32>(offset_id)) {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }

        let output = scroll_area.show(ui, |ui| {
            let root_id = match RootNoteId::new(
                self.note_context.ndb,
                self.note_context.note_cache,
                &txn,
                self.selected_note_id,
            ) {
                Ok(root_id) => root_id,

                Err(err) => {
                    ui.label(format!("Error loading thread: {:?}", err));
                    return None;
                }
            };

            let thread_timeline = self
                .timeline_cache
                .notes(
                    self.note_context.ndb,
                    self.note_context.note_cache,
                    &txn,
                    &TimelineKind::Thread(ThreadSelection::from_root_id(root_id.to_owned())),
                )
                .get_ptr();

            // TODO(jb55): skip poll if ThreadResult is fresh?

            let reversed = true;
            // poll for new notes and insert them into our existing notes
            if let Err(err) = thread_timeline.poll_notes_into_view(
                self.note_context.ndb,
                &txn,
                self.unknown_ids,
                self.note_context.note_cache,
                reversed,
            ) {
                error!("error polling notes into thread timeline: {err}");
            }

            TimelineTabView::new(
                thread_timeline.current_view(),
                true,
                self.note_options,
                &txn,
                self.is_muted,
                self.note_context,
                self.cur_acc,
                self.jobs,
            )
            .show(ui)
        });

        ui.data_mut(|d| d.insert_temp(offset_id, output.state.offset.y));

        output.inner
    }
}

#[allow(clippy::too_many_arguments)]
fn show_notes(
    ui: &mut egui::Ui,
    list: &mut VirtualList,
    thread_notes: &ThreadNotes,
    note_context: &mut NoteContext<'_>,
    zapping_acc: Option<&KeypairUnowned<'_>>,
    flags: NoteOptions,
    jobs: &mut JobsCache,
    txn: &Transaction,
    is_muted: &MuteFun,
) -> Option<NoteAction> {
    let mut action = None;

    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().item_spacing.x = 4.0;

    let selected_note_index = thread_notes.selected_index;
    let notes = &thread_notes.notes;

    list.ui_custom_layout(ui, notes.len(), |ui, cur_index| 's: {
        let note = &notes[cur_index];

        // should we mute the thread? we might not have it!
        let muted = root_note_id_from_selected_id(
            note_context.ndb,
            note_context.note_cache,
            txn,
            note.note.id(),
        )
        .ok()
        .is_some_and(|root_id| is_muted(&note.note, root_id.bytes()));

        if muted {
            break 's 0;
        }

        let resp = note.show(note_context, zapping_acc, flags, jobs, ui);

        action = if cur_index == selected_note_index {
            resp.action.and_then(strip_note_action)
        } else {
            resp.action
        }
        .or(action.take());

        1
    });

    action
}

fn strip_note_action(action: NoteAction) -> Option<NoteAction> {
    if matches!(
        action,
        NoteAction::Note {
            note_id: _,
            preview: false,
        }
    ) {
        return None;
    }

    Some(action)
}

struct ThreadNoteBuilder<'a> {
    chain: Vec<Note<'a>>,
    selected: Note<'a>,
    replies: Vec<Note<'a>>,
}

impl<'a> ThreadNoteBuilder<'a> {
    pub fn new(selected: Note<'a>) -> Self {
        Self {
            chain: Vec::new(),
            selected,
            replies: Vec::new(),
        }
    }

    pub fn add_chain(&mut self, note: Note<'a>) {
        self.chain.push(note);
    }

    pub fn add_reply(&mut self, note: Note<'a>) {
        self.replies.push(note);
    }

    pub fn into_notes(mut self, seen_flags: &mut NoteSeenFlags) -> ThreadNotes<'a> {
        let mut notes = Vec::new();

        let selected_is_root = self.chain.is_empty();
        let mut cur_is_root = true;
        while let Some(note) = self.chain.pop() {
            notes.push(ThreadNote {
                unread_and_have_replies: *seen_flags.get(note.id()).unwrap_or(&false),
                note,
                note_type: ThreadNoteType::Chain { root: cur_is_root },
            });
            cur_is_root = false;
        }

        let selected_index = notes.len();
        notes.push(ThreadNote {
            note: self.selected,
            note_type: ThreadNoteType::Selected {
                root: selected_is_root,
            },
            unread_and_have_replies: false,
        });

        for reply in self.replies {
            notes.push(ThreadNote {
                unread_and_have_replies: *seen_flags.get(reply.id()).unwrap_or(&false),
                note: reply,
                note_type: ThreadNoteType::Reply,
            });
        }

        ThreadNotes {
            notes,
            selected_index,
        }
    }
}

enum ThreadNoteType {
    Chain { root: bool },
    Selected { root: bool },
    Reply,
}

struct ThreadNotes<'a> {
    notes: Vec<ThreadNote<'a>>,
    selected_index: usize,
}

struct ThreadNote<'a> {
    pub note: Note<'a>,
    note_type: ThreadNoteType,
    pub unread_and_have_replies: bool,
}

impl<'a> ThreadNote<'a> {
    fn options(&self, mut cur_options: NoteOptions) -> NoteOptions {
        match self.note_type {
            ThreadNoteType::Chain { root: _ } => cur_options,
            ThreadNoteType::Selected { root: _ } => {
                cur_options.set_wide(true);
                cur_options
            }
            ThreadNoteType::Reply => cur_options,
        }
    }

    fn show(
        &self,
        note_context: &'a mut NoteContext<'_>,
        zapping_acc: Option<&'a KeypairUnowned<'a>>,
        flags: NoteOptions,
        jobs: &'a mut JobsCache,
        ui: &mut egui::Ui,
    ) -> NoteResponse {
        let inner = notedeck_ui::padding(8.0, ui, |ui| {
            NoteView::new(
                note_context,
                zapping_acc,
                &self.note,
                self.options(flags),
                jobs,
            )
            .unread_indicator(self.unread_and_have_replies)
            .show(ui)
        });

        match self.note_type {
            ThreadNoteType::Chain { root } => add_chain_adornment(ui, &inner, root),
            ThreadNoteType::Selected { root } => add_selected_adornment(ui, &inner, root),
            ThreadNoteType::Reply => notedeck_ui::hline(ui),
        }

        inner.inner
    }
}

fn add_chain_adornment(ui: &mut egui::Ui, note_resp: &InnerResponse<NoteResponse>, root: bool) {
    let Some(pfp_rect) = note_resp.inner.pfp_rect else {
        return;
    };

    let note_rect = note_resp.response.rect;

    let painter = ui.painter_at(note_rect);

    if !root {
        paint_line_above_pfp(ui, &painter, &pfp_rect, &note_rect);
    }

    // painting line below pfp:
    let top_pt = {
        let mut top = pfp_rect.center();
        top.y = pfp_rect.bottom();
        top
    };

    let bottom_pt = {
        let mut bottom = top_pt;
        bottom.y = note_rect.bottom();
        bottom
    };

    painter.line_segment([top_pt, bottom_pt], LINE_STROKE(ui));

    let hline_min_x = top_pt.x + 6.0;
    notedeck_ui::hline_with_width(
        ui,
        egui::Rangef::new(hline_min_x, ui.available_rect_before_wrap().right()),
    );
}

fn add_selected_adornment(ui: &mut egui::Ui, note_resp: &InnerResponse<NoteResponse>, root: bool) {
    let Some(pfp_rect) = note_resp.inner.pfp_rect else {
        return;
    };
    let note_rect = note_resp.response.rect;
    let painter = ui.painter_at(note_rect);

    if !root {
        paint_line_above_pfp(ui, &painter, &pfp_rect, &note_rect);
    }
    notedeck_ui::hline(ui);
}

fn paint_line_above_pfp(
    ui: &egui::Ui,
    painter: &egui::Painter,
    pfp_rect: &egui::Rect,
    note_rect: &egui::Rect,
) {
    let bottom_pt = {
        let mut center = pfp_rect.center();
        center.y = pfp_rect.top();
        center
    };

    let top_pt = {
        let mut top = bottom_pt;
        top.y = note_rect.top();
        top
    };

    painter.line_segment([bottom_pt, top_pt], LINE_STROKE(ui));
}

const LINE_STROKE: fn(&egui::Ui) -> egui::Stroke = |ui: &egui::Ui| {
    let mut stroke = ui.style().visuals.widgets.noninteractive.bg_stroke;
    stroke.width = 2.0;
    stroke
};
