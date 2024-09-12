use crate::route::{Route, Router};
use crate::timeline::{Timeline, TimelineId};
use std::iter::Iterator;
use tracing::warn;

pub struct Column {
    router: Router<Route>,
}

impl Column {
    pub fn new(routes: Vec<Route>) -> Self {
        let router = Router::new(routes);
        Column { router }
    }

    pub fn router(&self) -> &Router<Route> {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router<Route> {
        &mut self.router
    }
}

#[derive(Default)]
pub struct Columns {
    /// Columns are simply routers into settings, timelines, etc
    columns: Vec<Column>,

    /// Timeline state is not tied to routing logic separately, so that
    /// different columns can navigate to and from settings to timelines,
    /// etc.
    pub timelines: Vec<Timeline>,

    /// The selected column for key navigation
    selected: i32,
}

impl Columns {
    pub fn new() -> Self {
        Columns::default()
    }

    pub fn add_timeline(&mut self, timeline: Timeline) {
        let routes = vec![Route::timeline(timeline.id)];
        self.timelines.push(timeline);
        self.columns.push(Column::new(routes))
    }

    pub fn columns_mut(&mut self) -> &mut Vec<Column> {
        &mut self.columns
    }

    pub fn timeline_mut(&mut self, timeline_ind: usize) -> &mut Timeline {
        &mut self.timelines[timeline_ind]
    }

    pub fn column(&self, ind: usize) -> &Column {
        &self.columns()[ind]
    }

    pub fn columns(&self) -> &Vec<Column> {
        &self.columns
    }

    pub fn selected(&mut self) -> &mut Column {
        &mut self.columns[self.selected as usize]
    }

    pub fn timelines_mut(&mut self) -> &mut Vec<Timeline> {
        &mut self.timelines
    }

    pub fn timelines(&self) -> &Vec<Timeline> {
        &self.timelines
    }

    pub fn find_timeline_mut(&mut self, id: TimelineId) -> Option<&mut Timeline> {
        self.timelines_mut().iter_mut().find(|tl| tl.id == id)
    }

    pub fn find_timeline(&self, id: TimelineId) -> Option<&Timeline> {
        self.timelines().iter().find(|tl| tl.id == id)
    }

    pub fn column_mut(&mut self, ind: usize) -> &mut Column {
        &mut self.columns[ind]
    }

    pub fn select_down(&mut self) {
        warn!("todo: implement select_down");
    }

    pub fn select_up(&mut self) {
        warn!("todo: implement select_up");
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
