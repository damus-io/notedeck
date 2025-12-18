use egui::scroll_area::ScrollAreaOutput;

pub struct DragResponse<R> {
    pub drag_id: Option<egui::Id>, // the id which was used for dragging.
    pub output: Option<R>,
}

impl<R> DragResponse<R> {
    pub fn none() -> Self {
        Self {
            drag_id: None,
            output: None,
        }
    }

    pub fn scroll(output: ScrollAreaOutput<Option<R>>) -> Self {
        Self {
            drag_id: Some(Self::scroll_output_to_drag_id(output.id)),
            output: output.inner,
        }
    }

    pub fn set_scroll_id(&mut self, output: &ScrollAreaOutput<Option<R>>) {
        self.drag_id = Some(Self::scroll_output_to_drag_id(output.id));
    }

    pub fn output(output: Option<R>) -> Self {
        Self {
            drag_id: None,
            output,
        }
    }

    pub fn set_output(&mut self, output: R) {
        self.output = Some(output);
    }

    /// The id of an `egui::ScrollAreaOutput`
    /// Should use `Self::scroll` when possible
    pub fn scroll_raw(mut self, id: egui::Id) -> Self {
        self.drag_id = Some(Self::scroll_output_to_drag_id(id));
        self
    }

    /// The id which is directly used for dragging
    pub fn set_drag_id_raw(&mut self, id: egui::Id) {
        self.drag_id = Some(id);
    }

    fn scroll_output_to_drag_id(id: egui::Id) -> egui::Id {
        id.with("area")
    }

    pub fn map_output<S>(self, f: impl FnOnce(R) -> S) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: self.output.map(f),
        }
    }

    pub fn map_output_maybe<S>(self, f: impl FnOnce(R) -> Option<S>) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: self.output.and_then(f),
        }
    }

    pub fn maybe_map_output<S>(self, f: impl FnOnce(Option<R>) -> S) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: Some(f(self.output)),
        }
    }

    /// insert the contents of the new DragResponse if they are empty in Self
    pub fn insert(&mut self, body: DragResponse<R>) {
        self.drag_id = self.drag_id.or(body.drag_id);
        if self.output.is_none() {
            self.output = body.output;
        }
    }
}
