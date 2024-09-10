use crate::route::Route;
use crate::timeline::{Timeline, TimelineId};
use std::iter::Iterator;
use tracing::warn;

pub struct Column {
    kind: ColumnKind,
    routes: Vec<Route>,

    pub navigating: bool,
    pub returning: bool,
}

impl Column {
    pub fn timeline(timeline: Timeline) -> Self {
        let routes = vec![Route::Timeline(format!("{}", &timeline.kind))];
        let kind = ColumnKind::Timeline(timeline);
        Column::new(kind, routes)
    }

    pub fn kind(&self) -> &ColumnKind {
        &self.kind
    }

    pub fn kind_mut(&mut self) -> &mut ColumnKind {
        &mut self.kind
    }

    pub fn view_id(&self) -> egui::Id {
        self.kind.view_id()
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    pub fn routes_mut(&mut self) -> &mut Vec<Route> {
        &mut self.routes
    }

    pub fn new(kind: ColumnKind, routes: Vec<Route>) -> Self {
        let navigating = false;
        let returning = false;
        Column {
            kind,
            routes,
            navigating,
            returning,
        }
    }
}

pub struct Columns {
    columns: Vec<Column>,

    /// The selected column for key navigation
    selected: i32,
}

impl Columns {
    pub fn columns_mut(&mut self) -> &mut Vec<Column> {
        &mut self.columns
    }

    pub fn column(&self, ind: usize) -> &Column {
        &self.columns()[ind]
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn new(columns: Vec<Column>) -> Self {
        let selected = -1;
        Columns { columns, selected }
    }

    pub fn selected(&mut self) -> &mut Column {
        &mut self.columns[self.selected as usize]
    }

    pub fn timelines_mut(&mut self) -> impl Iterator<Item = &mut Timeline> {
        self.columns
            .iter_mut()
            .filter_map(|c| c.kind_mut().timeline_mut())
    }

    pub fn timelines(&self) -> impl Iterator<Item = &Timeline> {
        self.columns.iter().filter_map(|c| c.kind().timeline())
    }

    pub fn find_timeline_mut(&mut self, id: TimelineId) -> Option<&mut Timeline> {
        self.timelines_mut().find(|tl| tl.id == id)
    }

    pub fn find_timeline(&self, id: TimelineId) -> Option<&Timeline> {
        self.timelines().find(|tl| tl.id == id)
    }

    pub fn column_mut(&mut self, ind: usize) -> &mut Column {
        &mut self.columns[ind]
    }

    pub fn select_down(&mut self) {
        self.selected().kind_mut().select_down();
    }

    pub fn select_up(&mut self) {
        self.selected().kind_mut().select_up();
    }

    pub fn select_left(&mut self) {
        if self.selected - 1 < 0 {
            return;
        }
        self.selected -= 1;
    }

    pub fn select_right(&mut self) {
        if self.selected + 1 >= self.columns.len() as i32 {
            return;
        }
        self.selected += 1;
    }
}

/// What type of column is it?
#[derive(Debug)]
pub enum ColumnKind {
    Timeline(Timeline),

    ManageAccount,
}

impl ColumnKind {
    pub fn timeline_mut(&mut self) -> Option<&mut Timeline> {
        match self {
            ColumnKind::Timeline(tl) => Some(tl),
            _ => None,
        }
    }

    pub fn timeline(&self) -> Option<&Timeline> {
        match self {
            ColumnKind::Timeline(tl) => Some(tl),
            _ => None,
        }
    }

    pub fn view_id(&self) -> egui::Id {
        match self {
            ColumnKind::Timeline(timeline) => timeline.view_id(),
            ColumnKind::ManageAccount => egui::Id::new("manage_account"),
        }
    }

    pub fn select_down(&mut self) {
        match self {
            ColumnKind::Timeline(tl) => tl.current_view_mut().select_down(),
            ColumnKind::ManageAccount => warn!("todo: manage account select_down"),
        }
    }

    pub fn select_up(&mut self) {
        match self {
            ColumnKind::Timeline(tl) => tl.current_view_mut().select_down(),
            ColumnKind::ManageAccount => warn!("todo: manage account select_down"),
        }
    }
}
