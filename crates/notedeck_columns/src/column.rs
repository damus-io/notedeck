use crate::{
    actionbar::TimelineOpenResult,
    route::{ColumnsRouter, Route, SingletonRouter},
    timeline::{RemoteSubscriptionPolicy, Timeline, TimelineCache, TimelineKind},
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{NoteCache, ScopedSubApi};
use std::iter::Iterator;
use tracing::warn;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ColumnId {
    deck_id: u64,
    local_id: u64,
}

impl ColumnId {
    const UNASSIGNED: Self = Self {
        deck_id: 0,
        local_id: 0,
    };

    fn new(deck_id: u64, local_id: u64) -> Self {
        Self { deck_id, local_id }
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: u64) -> Self {
        Self::for_test_in_deck(1, value)
    }

    #[cfg(test)]
    pub(crate) fn for_test_in_deck(deck_id: u64, local_id: u64) -> Self {
        Self::new(deck_id, local_id)
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    id: ColumnId,
    pub router: ColumnsRouter<Route>,
    pub sheet_router: SingletonRouter<Route>,
}

impl Column {
    pub fn new(routes: Vec<Route>) -> Self {
        let router = ColumnsRouter::new(routes);
        Column {
            id: ColumnId::UNASSIGNED,
            router,
            sheet_router: SingletonRouter::default(),
        }
    }

    pub fn id(&self) -> ColumnId {
        self.id
    }

    fn assign_id(&mut self, id: ColumnId) {
        self.id = id;
    }

    pub(crate) fn reassign_id(&mut self, id: ColumnId) {
        self.assign_id(id);
    }

    pub fn router(&self) -> &ColumnsRouter<Route> {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut ColumnsRouter<Route> {
        &mut self.router
    }
}

#[derive(Debug)]
pub struct Columns {
    /// Columns are simply routers into settings, timelines, etc
    columns: Vec<Column>,

    /// The selected column for key navigation
    pub selected: i32,

    deck_id: u64,
    next_column_id: u64,
}

impl Default for Columns {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RemovedColumn {
    pub id: ColumnId,
    #[cfg(test)]
    pub index: usize,
    pub routes: Vec<Route>,
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
        Self {
            columns: Vec::new(),
            selected: 0,
            deck_id: 0,
            next_column_id: 1,
        }
    }

    fn next_column_id(&mut self) -> ColumnId {
        let id = ColumnId::new(self.deck_id, self.next_column_id);
        self.next_column_id = self.next_column_id.saturating_add(1);
        id
    }

    pub(crate) fn assign_deck_id(&mut self, deck_id: u64) {
        self.deck_id = deck_id;
        let mut next_local_id = 1;
        for column in &mut self.columns {
            column.reassign_id(ColumnId::new(deck_id, next_local_id));
            next_local_id = next_local_id.saturating_add(1);
        }
        self.next_column_id = next_local_id;
    }

    fn assign_column_id(&mut self, column: &mut Column) {
        column.assign_id(self.next_column_id());
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
    pub fn select_by_route(&mut self, desired_route: Route) -> SelectionResult {
        for (i, col) in self.columns.iter().enumerate() {
            for route in col.router().routes() {
                if *route == desired_route {
                    if self.selected as usize == i {
                        return SelectionResult::AlreadySelected(i);
                    } else {
                        self.select_column(i as i32);
                        return SelectionResult::NewSelection(i);
                    }
                }
            }
        }

        if matches!(&desired_route, Route::Timeline(_))
            || matches!(&desired_route, Route::Thread(_))
        {
            // these require additional handling to add state
            tracing::error!("failed to select {desired_route:?} column");
            return SelectionResult::Failed;
        }

        self.add_column(Column::new(vec![desired_route]));

        let selected_index = self.columns.len() - 1;
        self.select_column(selected_index as i32);
        SelectionResult::NewSelection(selected_index)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_new_timeline_column(
        &mut self,
        timeline_cache: &mut TimelineCache,
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        scoped_subs: &mut ScopedSubApi<'_>,
        kind: &TimelineKind,
        account_pk: Pubkey,
        remote_policy: RemoteSubscriptionPolicy,
    ) -> Option<TimelineOpenResult> {
        self.add_column(Column::new(vec![Route::timeline(kind.to_owned())]));
        timeline_cache.open(
            ndb,
            note_cache,
            txn,
            scoped_subs,
            kind,
            account_pk,
            false,
            remote_policy,
        )
    }

    pub fn new_column_picker(&mut self) {
        self.add_column(Column::new(vec![Route::AddColumn(
            crate::ui::add_column::AddColumnRoute::Base,
        )]));
    }

    pub fn insert_intermediary_routes(
        &mut self,
        timeline_cache: &mut TimelineCache,
        account_pk: Pubkey,
        intermediary_routes: Vec<IntermediaryRoute>,
    ) {
        let routes = intermediary_routes
            .into_iter()
            .map(|r| match r {
                IntermediaryRoute::Timeline(timeline) => {
                    let route = Route::timeline(timeline.kind.clone());
                    timeline_cache.insert(timeline.kind.clone(), account_pk, *timeline);
                    route
                }
                IntermediaryRoute::Route(route) => route,
            })
            .collect();

        self.add_column(Column::new(routes));
    }

    #[inline]
    pub fn add_column_at(&mut self, mut column: Column, index: u32) {
        self.assign_column_id(&mut column);
        self.columns.insert(index as usize, column);
    }

    #[inline]
    pub fn add_column(&mut self, mut column: Column) {
        self.assign_column_id(&mut column);
        self.columns.push(column);
    }

    #[inline]
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    // Get the first router in the columns if there are columns present.
    // Otherwise, create a new column picker and return the router
    pub fn get_selected_router(&mut self) -> &mut ColumnsRouter<Route> {
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

    /// Return the column with a stable id plus its current display index.
    pub(crate) fn column_mut_by_id(&mut self, id: ColumnId) -> Option<(usize, &mut Column)> {
        let index = self.columns.iter().position(|column| column.id() == id)?;
        self.columns.get_mut(index).map(|column| (index, column))
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

    #[must_use = "you must dispose every returned route"]
    pub fn delete_column(&mut self, index: usize) -> RemovedColumn {
        let removed = self.columns.remove(index);
        let removed = RemovedColumn {
            id: removed.id,
            #[cfg(test)]
            index,
            routes: removed.router.routes_for_disposal(),
        };

        // if we've removed the selected column, reduce the index by 1
        if self.selected == (index as i32) && self.selected != 0 {
            self.selected -= 1;
        }

        if self.columns.is_empty() {
            self.new_column_picker();
        }

        removed
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

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::RootNoteIdBuf;

    #[test]
    fn delete_column_returns_entire_removed_route_stack() {
        let account = Pubkey::new([1; 32]);
        let timeline = Route::timeline(TimelineKind::contact_list(account));
        let thread = Route::thread(crate::timeline::ThreadSelection::from_root_id(
            RootNoteIdBuf::new_unsafe([2; 32]),
        ));
        let mut columns = Columns::default();
        columns.add_column(Column::new(vec![timeline.clone(), thread.clone()]));

        let removed = columns.delete_column(0);

        assert_eq!(removed.index, 0);
        assert_ne!(removed.id, ColumnId::UNASSIGNED);
        assert_eq!(removed.routes, vec![timeline, thread]);
    }

    #[test]
    fn delete_column_after_thread_overlay_collapse_returns_visible_stack() {
        let account = Pubkey::new([1; 32]);
        let timeline = Route::timeline(TimelineKind::contact_list(account));
        let thread_a = Route::thread(crate::timeline::ThreadSelection::from_root_id(
            RootNoteIdBuf::new_unsafe([2; 32]),
        ));
        let thread_b = Route::thread(crate::timeline::ThreadSelection::from_root_id(
            RootNoteIdBuf::new_unsafe([3; 32]),
        ));
        let mut column = Column::new(vec![timeline.clone()]);
        column.router_mut().route_to_overlaid(thread_a.clone());
        column.router_mut().route_to_overlaid(thread_b.clone());
        column.router_mut().go_back();
        let mut columns = Columns::default();
        columns.add_column(column);

        let removed = columns.delete_column(0);

        // `thread_a` and `thread_b` are one thread scope: click/back cleanup of
        // the retained top route (`thread_b`) drops the whole scope. Returning
        // the drained route here would double-dispose the same scope.
        assert_eq!(removed.routes, vec![timeline, thread_b]);
    }
}

pub enum ColumnsAction {
    Switch(usize, usize), // from Switch.0 to Switch.1,
    Remove(usize),
}
