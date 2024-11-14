use crate::route::{Route, Router};
use crate::timeline::{
    SerializableTimeline, Timeline, Timeline, TimelineId, TimelineId, TimelineRoute,
};
use enostr::RelayPool;
use indexmap::IndexMap;
use nostrdb::Ndb;
use serde::{Deserialize, Deserializer, Serialize};
use std::iter::Iterator;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{error, warn};

#[derive(Clone)]
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

impl serde::Serialize for Column {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.router.routes().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Column {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let routes = Vec::<Route>::deserialize(deserializer)?;

        Ok(Column {
            router: Router::new(routes),
        })
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
        let routes = vec![Route::timeline(timeline.id)];
        self.timelines.push(timeline);
        self.columns.push(Column::new(routes));
    }

    pub fn add_timeline_to_column(&mut self, col: usize, timeline: Timeline) {
        self.column_mut(col)
            .router_mut()
            .route_to_replaced(Route::timeline(timeline.id));
        self.timelines.push(timeline);
    }

    pub fn new_column_picker(&mut self) {
        self.add_column(Column::new(vec![Route::AddColumn(
            crate::ui::add_column::AddColumnRoute::Base,
        )]));
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

    pub fn as_serializable_columns(&self) -> SerializableColumns {
        SerializableColumns {
            columns: self.columns.values().cloned().collect(),
            timelines: self
                .timelines
                .values()
                .map(|t| t.as_serializable_timeline())
                .collect(),
        }
    }

    /// Remove a column and unsubscribe from any subscriptions active in it
    pub fn delete_column(&mut self, col_ind: usize, ndb: &Ndb, pool: &mut RelayPool) {
        // If we have a timeline in this column, then we need to make
        // sure to unsubscribe from it
        if let Some(timeline_id) = self
            .column(col_ind)
            .router()
            .routes()
            .iter()
            .find_map(|r| r.timeline_id())
        {
            let timeline_id = *timeline_id;
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
                        if *tid == timeline_id {
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

#[derive(Serialize, Deserialize)]
pub struct SerializableColumns {
    pub columns: Vec<Column>,
    pub timelines: Vec<SerializableTimeline>,
}

impl SerializableColumns {
    pub fn into_columns(self, ndb: &Ndb, deck_pubkey: Option<&[u8; 32]>) -> Columns {
        let mut columns = Columns::default();

        for column in self.columns {
            let id = Columns::get_new_id();
            let mut routes = Vec::new();
            for route in column.router.routes() {
                match route {
                    Route::Timeline(TimelineRoute::Timeline(timeline_id)) => {
                        if let Some(serializable_tl) =
                            self.timelines.iter().find(|tl| tl.id == *timeline_id)
                        {
                            let tl = serializable_tl.clone().into_timeline(ndb, deck_pubkey);
                            if let Some(tl) = tl {
                                routes.push(Route::Timeline(TimelineRoute::Timeline(tl.id)));
                                columns.timelines.insert(id, tl);
                            } else {
                                error!("Problem deserializing timeline {:?}", serializable_tl);
                            }
                        }
                    }
                    Route::Timeline(TimelineRoute::Thread(_thread)) => {
                        // TODO: open thread before pushing route
                    }
                    Route::Profile(_profile) => {
                        // TODO: open profile before pushing route
                    }
                    _ => routes.push(*route),
                }
            }
            columns.add_column_at(Column::new(routes), id);
        }

        columns
    }
}
