#[derive(Default, Clone, Debug)]
pub struct DragSwitch {
    state: Option<DragState>,
}

#[derive(Clone, Debug)]
struct DragState {
    start_pos: egui::Pos2,
    cur_direction: DragDirection,
}

impl DragSwitch {
    /// should call BEFORE both drag directions get rendered
    pub fn update(&mut self, horizontal: egui::Id, vertical: egui::Id, ctx: &egui::Context) {
        let horiz_being_dragged = ctx.is_being_dragged(horizontal);
        let vert_being_dragged = ctx.is_being_dragged(vertical);

        if !horiz_being_dragged && !vert_being_dragged {
            self.state = None;
            return;
        }

        let Some(state) = &mut self.state else {
            return;
        };

        let Some(cur_pos) = ctx.pointer_interact_pos() else {
            return;
        };

        let dx = (state.start_pos.x - cur_pos.x).abs();
        let dy = (state.start_pos.y - cur_pos.y).abs();

        let new_direction = if dx > dy {
            DragDirection::Horizontal
        } else {
            DragDirection::Vertical
        };

        if new_direction == DragDirection::Horizontal
            && state.cur_direction == DragDirection::Vertical
        {
            // drag is occuring mostly in the horizontal direction
            ctx.set_dragged_id(horizontal);
            let new_dir = DragDirection::Horizontal;
            state.cur_direction = new_dir;
        } else if new_direction == DragDirection::Vertical
            && state.cur_direction == DragDirection::Horizontal
        {
            // drag is occuring mostly in the vertical direction
            let new_dir = DragDirection::Vertical;
            state.cur_direction = new_dir;
            ctx.set_dragged_id(vertical);
        }
    }

    /// should call AFTER both drag directions rendered
    pub fn check_for_drag_start(
        &mut self,
        ctx: &egui::Context,
        horizontal: egui::Id,
        vertical: egui::Id,
    ) {
        let Some(drag_id) = ctx.drag_started_id() else {
            return;
        };

        let cur_direction = if drag_id == horizontal {
            DragDirection::Horizontal
        } else if drag_id == vertical {
            DragDirection::Vertical
        } else {
            return;
        };

        let Some(cur_pos) = ctx.pointer_interact_pos() else {
            return;
        };

        self.state = Some(DragState {
            start_pos: cur_pos,
            cur_direction,
        });
    }
}

#[derive(Debug, PartialEq, Clone)]
enum DragDirection {
    Horizontal,
    Vertical,
}

pub fn get_drag_id(ui: &egui::Ui, scroll_id: egui::Id) -> egui::Id {
    ui.id().with(egui::Id::new(scroll_id)).with("area")
}

// unfortunately a Frame makes a new id for the Ui
pub fn get_drag_id_through_frame(ui: &egui::Ui, scroll_id: egui::Id) -> egui::Id {
    ui.id()
        .with(egui::Id::new("child"))
        .with(egui::Id::new(scroll_id))
        .with("area")
}
