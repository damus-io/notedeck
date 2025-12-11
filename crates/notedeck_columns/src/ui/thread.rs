use egui::InnerResponse;
use egui_virtual_list::VirtualList;
use nostrdb::{Note, Transaction};
use notedeck::note::root_note_id_from_selected_id;
use notedeck::{NoteAction, NoteContext};
use notedeck_ui::note::NoteResponse;
use notedeck_ui::{NoteOptions, NoteView};

use crate::timeline::thread::{NoteSeenFlags, ParentState, Threads};
use notedeck::BodyResponse;

pub struct ThreadView<'a, 'd> {
    threads: &'a mut Threads,
    selected_note_id: &'a [u8; 32],
    note_options: NoteOptions,
    col: usize,
    note_context: &'a mut NoteContext<'d>,
}

impl<'a, 'd> ThreadView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        threads: &'a mut Threads,
        selected_note_id: &'a [u8; 32],
        note_options: NoteOptions,
        note_context: &'a mut NoteContext<'d>,
        col: usize,
    ) -> Self {
        ThreadView {
            threads,
            selected_note_id,
            note_options,
            note_context,
            col,
        }
    }

    pub fn scroll_id(selected_note_id: &[u8; 32], col: usize) -> egui::Id {
        egui::Id::new(("threadscroll", selected_note_id, col))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> BodyResponse<NoteAction> {
        let txn = Transaction::new(self.note_context.ndb).expect("txn");

        let scroll_id = ThreadView::scroll_id(self.selected_note_id, self.col);
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);

        if let Some(thread) = self.threads.threads.get_mut(&self.selected_note_id) {
            if let Some(new_offset) = thread.set_scroll_offset.take() {
                scroll_area = scroll_area.vertical_scroll_offset(new_offset);
            }
        }

        let output = scroll_area.show(ui, |ui| self.notes(ui, &txn));

        let out_id = output.id;
        let mut resp = output.inner;

        if let Some(NoteAction::Note {
            note_id: _,
            preview: _,
            scroll_offset,
        }) = &mut resp
        {
            *scroll_offset = output.state.offset.y;
        }

        BodyResponse::output(resp).scroll_raw(out_id)
    }

    fn notes(&mut self, ui: &mut egui::Ui, txn: &Transaction) -> Option<NoteAction> {
        let Ok(cur_note) = self
            .note_context
            .ndb
            .get_note_by_id(txn, self.selected_note_id)
        else {
            let id = *self.selected_note_id;
            tracing::error!("ndb: Did not find note {}", enostr::NoteId::new(id).hex());
            return None;
        };

        self.threads.update(
            &cur_note,
            self.note_context.note_cache,
            self.note_context.ndb,
            txn,
            self.note_context.unknown_ids,
            self.col,
        );

        let cur_node = self.threads.threads.get(&self.selected_note_id).unwrap();

        let full_chain = cur_node.have_all_ancestors;
        let mut note_builder = ThreadNoteBuilder::new(cur_note);

        let mut parent_state = cur_node.prev.clone();
        while let ParentState::Parent(id) = parent_state {
            if let Ok(note) = self.note_context.ndb.get_note_by_id(txn, id.bytes()) {
                note_builder.add_chain(note);
                if let Some(res) = self.threads.threads.get(&id.bytes()) {
                    parent_state = res.prev.clone();
                    continue;
                }
            }
            parent_state = ParentState::Unknown;
        }

        for note_ref in cur_node.replies.values() {
            if let Ok(note) = self.note_context.ndb.get_note_by_key(txn, note_ref.key) {
                note_builder.add_reply(note);
            }
        }

        let list = &mut self
            .threads
            .threads
            .get_mut(&self.selected_note_id)
            .unwrap()
            .list;

        let notes = note_builder.into_notes(
            self.note_options.contains(NoteOptions::RepliesNewestFirst),
            &mut self.threads.seen_flags,
        );

        if !full_chain {
            // TODO(kernelkind): insert UI denoting we don't have the full chain yet
            ui.colored_label(ui.visuals().error_fg_color, "LOADING NOTES");
        }

        show_notes(ui, list, &notes, self.note_context, self.note_options, txn)
    }
}

#[allow(clippy::too_many_arguments)]
fn show_notes(
    ui: &mut egui::Ui,
    list: &mut VirtualList,
    thread_notes: &ThreadNotes,
    note_context: &mut NoteContext<'_>,
    flags: NoteOptions,
    txn: &Transaction,
) -> Option<NoteAction> {
    let mut action = None;

    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().item_spacing.x = 4.0;

    let selected_note_index = thread_notes.selected_index;
    let notes = &thread_notes.notes;

    let is_muted = note_context.accounts.mutefun();

    list.ui_custom_layout(ui, notes.len(), |ui, cur_index| {
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
            return 1;
        }

        let resp = note.show(note_context, flags, ui);

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
            scroll_offset: _,
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

    pub fn into_notes(
        mut self,
        replies_newer_first: bool,
        seen_flags: &mut NoteSeenFlags,
    ) -> ThreadNotes<'a> {
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

        if replies_newer_first {
            self.replies
                .sort_by_key(|b| std::cmp::Reverse(b.created_at()));
        }

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

impl ThreadNoteType {
    fn is_selected(&self) -> bool {
        matches!(self, ThreadNoteType::Selected { .. })
    }
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
                cur_options.set(NoteOptions::Wide, true);
                cur_options.set(NoteOptions::SelectableText, true);
                cur_options.set(NoteOptions::FullCreatedDate, true);
                cur_options
            }
            ThreadNoteType::Reply => cur_options,
        }
    }

    fn show(
        &self,
        note_context: &'a mut NoteContext<'_>,
        flags: NoteOptions,
        ui: &mut egui::Ui,
    ) -> NoteResponse {
        let inner = notedeck_ui::padding(8.0, ui, |ui| {
            NoteView::new(note_context, &self.note, self.options(flags))
                .selected_style(self.note_type.is_selected())
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
