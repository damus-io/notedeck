use crate::{
    actionbar::TimelineOpenResult,
    drag::DragSwitch,
    route::{Route, Router, SingletonRouter},
    timeline::{Timeline, TimelineCache, TimelineKind},
};
use enostr::RelayPool;
use nostrdb::{Ndb, Transaction};
use notedeck::NoteCache;
use std::iter::Iterator;
use tracing::warn;

#[derive(Clone, Debug)]
pub struct Column {
    pub router: Router<Route>,
    pub sheet_router: SingletonRouter<Route>,
    pub drag: DragSwitch,
}

impl Column {
    pub fn new(routes: Vec<Route>) -> Self {
        let router = Router::new(routes);
        Column {
            router,
            sheet_router: SingletonRouter::default(),
            drag: DragSwitch::default(),
        }
    }

    pub fn router(&self) -> &Router<Route> {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router<Route> {
        &mut self.router
    }
}

#[derive(Default, Debug)]
pub struct Columns {
    /// Columns are simply routers into settings, timelines, etc
    columns: Vec<Column>,

    /// The selected column for key navigation
    pub selected: i32,
}

/// When selecting columns, return what happened
pub enum SelectionResult {
    /// We're already selecting that
    AlreadySelected(usize),

    /// New selection success!
    NewSelection(usize),

    /// Failed to make a selection
    Failed,
}

impl Columns {
    pub fn new() -> Self {
        Columns::default()
    }

    /// Choose which column is selected. If in narrow mode, this
    /// decides which column to render in the main view
    pub fn select_column(&mut self, index: i32) {
        let len = self.columns.len();

        if index < (len as i32) {
            self.selected = index;
        }
    }

    /// Select the column based on the timeline kind.
    ///
    /// TODO: add timeline if missing?
    pub fn select_by_kind(&mut self, kind: &TimelineKind) -> SelectionResult {
        for (i, col) in self.columns.iter().enumerate() {
            for route in col.router().routes() {
                if let Some(timeline) = route.timeline_id() {
                    if timeline == kind {
                        tracing::info!("selecting {kind:?} column");
                        if self.selected as usize == i {
                            return SelectionResult::AlreadySelected(i);
                        } else {
                            self.select_column(i as i32);
                            return SelectionResult::NewSelection(i);
                        }
                    }
                }
            }
        }

        tracing::error!("failed to select {kind:?} column");
        SelectionResult::Failed
    }

    pub fn add_new_timeline_column(
        &mut self,
        timeline_cache: &mut TimelineCache,
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        kind: &TimelineKind,
    ) -> Option<TimelineOpenResult> {
        self.columns
            .push(Column::new(vec![Route::timeline(kind.to_owned())]));
        timeline_cache.open(ndb, note_cache, txn, pool, kind)
    }

    pub fn new_column_picker(&mut self) {
        self.add_column(Column::new(vec![Route::AddColumn(
            crate::ui::add_column::AddColumnRoute::Base,
        )]));
    }

    pub fn insert_intermediary_routes(
        &mut self,
        timeline_cache: &mut TimelineCache,
        intermediary_routes: Vec<IntermediaryRoute>,
    ) {
        let routes = intermediary_routes
            .into_iter()
            .map(|r| match r {
                IntermediaryRoute::Timeline(mut timeline) => {
                    let route = Route::timeline(timeline.kind.clone());
                    timeline.subscription.increment();
                    timeline_cache.insert(timeline.kind.clone(), *timeline);
                    route
                }
                IntermediaryRoute::Route(route) => route,
            })
            .collect();

        self.columns.push(Column::new(routes));
    }

    #[inline]
    pub fn add_column_at(&mut self, column: Column, index: u32) {
        self.columns.insert(index as usize, column);
    }

    #[inline]
    pub fn add_column(&mut self, column: Column) {
        self.columns.push(column);
    }

    #[inline]
    pub fn columns_mut(&mut self) -> &mut Vec<Column> {
        &mut self.columns
    }

    #[inline]
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    // Get the first router in the columns if there are columns present.
    // Otherwise, create a new column picker and return the router
    pub fn get_selected_router(&mut self) -> &mut Router<Route> {
        self.ensure_column();
        self.selected_mut().router_mut()
    }

    #[inline]
    pub fn column(&self, ind: usize) -> &Column {
        &self.columns[ind]
    }

    #[inline]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    #[inline]
    pub fn selected(&self) -> Option<&Column> {
        if self.columns.is_empty() {
            return None;
        }
        Some(&self.columns[self.selected as usize])
    }

    // TODO(jb55): switch to non-empty container for columns?
    fn ensure_column(&mut self) {
        if self.columns.is_empty() {
            self.new_column_picker();
        }
    }

    /// Get the selected column. If you're looking to route something
    /// and you're not sure which one to choose, use this one
    #[inline]
    pub fn selected_mut(&mut self) -> &mut Column {
        self.ensure_column();
        assert!(self.selected < self.columns.len() as i32);
        &mut self.columns[self.selected as usize]
    }

    #[inline]
    pub fn column_mut(&mut self, ind: usize) -> &mut Column {
        self.ensure_column();
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

    pub fn delete_all_columns(&mut self) -> Vec<TimelineKind> {
        let cols: &mut Vec<Column> = self.columns_mut();
        let num_cols = cols.len();

        let mut kinds_to_pop: Vec<TimelineKind> = vec![];
        for i in (0..num_cols).rev() {
            kinds_to_pop.append(self.delete_column(i).as_mut());
        }
        self.selected = 0;
        if self.columns.is_empty() {
            self.new_column_picker();
        }
        kinds_to_pop
    }

    pub fn delete_other(&mut self, index: usize) -> Vec<TimelineKind> {
        let cols = self.columns_mut();
        let num_cols = cols.len();

        let mut kinds_to_pop: Vec<TimelineKind> = vec![];
        'col_loop: for i in (0..num_cols).rev() {
            if i == index {
                continue 'col_loop;
            }
            kinds_to_pop.append(self.delete_column(i).as_mut());
        }
        self.selected = 0;
        kinds_to_pop
    }

    pub fn delete_dir(&mut self, index: usize, left: bool) -> Vec<TimelineKind> {
        let cols = self.columns_mut();
        let num_cols = cols.len();

        let mut kinds_to_pop: Vec<TimelineKind> = vec![];
        let cols = if left { 0..index } else { index + 1..num_cols };
        for i in cols.rev() {
            if i < num_cols {
                kinds_to_pop.append(self.delete_column(i).as_mut());
            }
        }
        self.selected = 0;
        kinds_to_pop
    }

    #[must_use = "you must call timeline_cache.pop() for each returned value"]
    pub fn delete_column(&mut self, index: usize) -> Vec<TimelineKind> {
        let mut kinds_to_pop: Vec<TimelineKind> = vec![];
        for route in self.columns[index].router().routes() {
            if let Route::Timeline(kind) = route {
                kinds_to_pop.push(kind.clone());
            }
        }

        self.columns.remove(index);

        if self.columns.is_empty() {
            self.new_column_picker();
        }

        kinds_to_pop
    }

    pub fn move_col(&mut self, from_index: usize, to_index: usize) {
        if from_index == to_index
            || from_index >= self.columns.len()
            || to_index >= self.columns.len()
        {
            return;
        }

        self.columns.swap(from_index, to_index);
    }
}

pub enum IntermediaryRoute {
    Timeline(Box<Timeline>),
    Route(Route),
}

pub enum ColumnsAction {
    Switch(usize, usize), // from Switch.0 to Switch.1,
    Remove(usize),
}
