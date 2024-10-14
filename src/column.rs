use crate::route::{Route, Router};
use crate::timeline::{Timeline, TimelineId};
use indexmap::IndexMap;
use std::iter::Iterator;
use std::sync::atomic::{AtomicU32, Ordering};
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
    columns: IndexMap<u32, Column>,

    /// Timeline state is not tied to routing logic separately, so that
    /// different columns can navigate to and from settings to timelines,
    /// etc.
    pub timelines: IndexMap<u32, Timeline>,

    /// The selected column for key navigation
    selected: i32,
    should_delete_column_at_index: Option<usize>,
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
        let col_id = self.get_column_id_at_index(col);
        self.column_mut(col)
            .router_mut()
            .route_to_replaced(Route::timeline(timeline.id));
        self.timelines.insert(col_id, timeline);
    }

    pub fn new_column_picker(&mut self) {
        self.add_column(Column::new(vec![Route::AddColumn]));
    }

    fn get_new_id() -> u32 {
        UIDS.fetch_add(1, Ordering::Relaxed)
    }

    pub fn add_column(&mut self, column: Column) {
        self.columns.insert(Self::get_new_id(), column);
    }

    pub fn columns_mut(&mut self) -> Vec<&mut Column> {
        self.columns.values_mut().collect()
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
            .get_index_mut(0)
            .expect("There should be at least one column")
            .1
            .router_mut()
    }

    pub fn timeline_mut(&mut self, timeline_ind: usize) -> &mut Timeline {
        self.timelines
            .get_index_mut(timeline_ind)
            .expect("expected index to be in bounds")
            .1
    }

    pub fn column(&self, ind: usize) -> &Column {
        self.columns
            .get_index(ind)
            .expect("Expected index to be in bounds")
            .1
    }

    pub fn columns(&self) -> Vec<&Column> {
        self.columns.values().collect()
    }

    pub fn get_column_id_at_index(&self, ind: usize) -> u32 {
        *self
            .columns
            .get_index(ind)
            .expect("expected index to be within bounds")
            .0
    }

    pub fn selected(&mut self) -> &mut Column {
        self.columns
            .get_index_mut(self.selected as usize)
            .expect("Expected selected index to be in bounds")
            .1
    }

    pub fn timelines_mut(&mut self) -> Vec<&mut Timeline> {
        self.timelines.values_mut().collect()
    }

    pub fn timelines(&self) -> Vec<&Timeline> {
        self.timelines.values().collect()
    }

    pub fn find_timeline_mut(&mut self, id: TimelineId) -> Option<&mut Timeline> {
        self.timelines_mut().into_iter().find(|tl| tl.id == id)
    }

    pub fn find_timeline(&self, id: TimelineId) -> Option<&Timeline> {
        self.timelines().into_iter().find(|tl| tl.id == id)
    }

    pub fn column_mut(&mut self, ind: usize) -> &mut Column {
        self.columns
            .get_index_mut(ind)
            .expect("Expected index to be in bounds")
            .1
    }

    pub fn find_timeline_for_column_index(&self, ind: usize) -> Option<&Timeline> {
        let col_id = self.get_column_id_at_index(ind);
        self.timelines.get(&col_id)
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

    pub fn request_deletion_at_index(&mut self, index: usize) {
        self.should_delete_column_at_index = Some(index);
    }

    pub fn attempt_perform_deletion_request(&mut self) {
        if let Some(index) = self.should_delete_column_at_index {
            if let Some((key, _)) = self.columns.get_index_mut(index) {
                self.timelines.shift_remove(key);
            }

            self.columns.shift_remove_index(index);
            self.should_delete_column_at_index = None;

            if self.columns.is_empty() {
                self.new_column_picker();
            }
        }
    }
}
