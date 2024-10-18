use crate::route::{Route, Router};
use crate::timeline::{Timeline, TimelineId};
use indexmap::IndexMap;
use std::iter::Iterator;
use std::sync::atomic::{AtomicU32, Ordering};
use nostrdb::Ndb;
use enostr::RelayPool;
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
static UIDS: AtomicU32 = AtomicU32::new(0);

impl Columns {
    pub fn new() -> Self {
        Columns::default()
    }

    pub fn add_new_timeline_column(&mut self, timeline: Timeline) {
        let id = Self::get_new_id();
        let routes = vec![Route::timeline(timeline.id)];
        self.timelines.insert(id, timeline);
        self.columns.insert(id, Column::new(routes));
    }

    pub fn add_timeline_to_column(&mut self, col: usize, timeline: Timeline) {
        self.column_mut(col)
            .router_mut()
            .route_to_replaced(Route::timeline(timeline.id));
        self.timelines.insert(timeline);
    }

    pub fn new_column_picker(&mut self) {
        self.add_column(Column::new(vec![Route::AddColumn]));
    }

    pub fn add_column(&mut self, column: Column) {
        self.columns.push(column);
    }

    pub fn columns_mut(&mut self) -> &mut Vec<Column> {
        &mut self.columns
    }

    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    // Get the first router in the columns if there are columns present.
    // Otherwise, create a new column picker and return the router
    pub fn get_first_router(&mut self) -> &mut Router<Route> {
        if self.columns.is_empty() {
            self.new_column_picker();
        }
        self.columns
            .get_mut(0)
            .expect("There should be at least one column")
            .router_mut()
    }

    pub fn timeline_mut(&mut self, timeline_ind: usize) -> &mut Timeline {
        self.timelines
            .get_mut(timeline_ind)
            .expect("expected index to be in bounds")
    }

    pub fn column(&self, ind: usize) -> &Column {
        &self.columns[ind]
    }

    pub fn columns(&self) -> &Vec<Column> {
        &self.columns
    }

    pub fn selected(&mut self) -> &mut Column {
        self.columns
            .get_mut(self.selected as usize)
            .expect("Expected selected index to be in bounds")
    }

    pub fn timelines_mut(&mut self) -> &mut Vec<Timeline> {
        &mut self.timelines
    }

    pub fn timelines(&self) -> &Vec<Timeline> {
        &self.timelines
    }

    pub fn find_timeline_mut(&mut self, id: TimelineId) -> Option<&mut Timeline> {
        self.timelines_mut().into_iter().find(|tl| tl.id == id)
    }

    pub fn find_timeline(&self, id: TimelineId) -> Option<&Timeline> {
        self.timelines().into_iter().find(|tl| tl.id == id)
    }

    pub fn column_mut(&mut self, ind: usize) -> &mut Column {
        self.columns
            .get_mut(ind)
            .expect("Expected index to be in bounds")
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

    /// Remove a column and unsubscribe from any subscriptions active in it
    pub fn remove_column(&mut self, ndb: &Ndb, pool: &mut RelayPool, col_ind: usize) {
        // If we have a timeline in this column, then we need to make
        // sure to unsubscribe from it
        if let Some(timeline_id) = self.column(col_ind).router().find_map(|r| r.timeline_id()) {
            let mut timelines: i32 = 0;
            // We may have multiple of the same timeline in different
            // columns. We shouldn't unsubscribe the timeline in this
            // case, we only want to unsubscribe if its the last one.
            // Albeit this is probably a weird case, but we should still
            // handle it properly. Let's count and make sure we only have 1

            // Traverse all columns
            for column in self.columns {
                // Look at each route in each column
                for route in column.router().routes() {
                    // Does the column's timeline we're removing exist in
                    // this column?
                    if let Some(tid) = route.timeline_id() {
                        if tid == timeline_id {
                            // if so, we increase the count
                            timelines += 1;
                        }
                    }
                }
            }

            // We only have one timeline in all of the columns, so we can
            // unsubscribe
            if timelines == 1 {
                let timeline = self.find_timeline(timeline_id).expect("timeline");
                timeline.unsubscribe(ndb, pool);
                self.timelines = self
                    .timelines
                    .iter()
                    .filter(|tl| tl.id != timeline_id)
                    .collect();
            }
        }

        self.columns.remove(col_ind);

        if self.columns.is_empty() {
            self.new_column_picker();
        }
    }
}
