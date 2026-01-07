//! Publication state management for NKBIP-01 publications (kind 30040/30041)
//!
//! Uses PublicationTree from notedeck_publications crate for hierarchical
//! navigation of nested publications.

use enostr::{NoteId, RelayPool};
use nostrdb::{Filter, Ndb, Note, Transaction};
use notedeck_publications::{
    EventAddress, NodeType, PublicationTree, PublicationTreeNode,
};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info};

use crate::subscriptions;

/// Maximum inline nesting depth before opening a new publication view
pub const MAX_INLINE_DEPTH: usize = 5;

/// State for a single publication being viewed (tree-based)
#[derive(Debug)]
pub struct PublicationTreeState {
    /// The hierarchical tree structure
    pub tree: PublicationTree,

    /// Remote subscription ID for fetching nodes
    pub sub_id: Option<String>,

    /// Last observed version (for detecting changes)
    last_version: u64,

    /// Addresses currently being fetched (to avoid duplicate subscriptions)
    pending_fetch: HashSet<EventAddress>,
}

impl PublicationTreeState {
    /// Create from a root 30040 index note
    pub fn from_root_note(note: &Note) -> Option<Self> {
        let tree = PublicationTree::from_root_note(note.to_owned())?;
        let last_version = tree.resolved_version();

        Some(Self {
            tree,
            sub_id: None,
            last_version,
            pending_fetch: HashSet::new(),
        })
    }

    /// Get addresses that need to be fetched from relays
    pub fn needs_fetch(&self) -> Vec<EventAddress> {
        self.tree
            .pending_addresses()
            .into_iter()
            .filter(|addr| !self.pending_fetch.contains(addr))
            .cloned()
            .collect()
    }

    /// Check if state changed since last check (for UI updates)
    pub fn check_changed(&mut self) -> bool {
        let current = self.tree.resolved_version();
        if current != self.last_version {
            self.last_version = current;
            true
        } else {
            false
        }
    }

    /// Get iterator over resolved leaf nodes (content sections) in reading order
    pub fn resolved_sections(&self) -> impl Iterator<Item = (usize, &PublicationTreeNode)> {
        self.tree.resolved_leaves()
    }

    /// Get total section count (resolved + pending leaves)
    pub fn section_count(&self) -> usize {
        self.tree.leaves().count()
    }

    /// Get resolved section count
    pub fn resolved_section_count(&self) -> usize {
        self.tree.resolved_leaves().count()
    }

    /// Check if all sections have been fetched
    pub fn is_complete(&self) -> bool {
        !self.tree.has_pending()
    }

    /// Get the root node
    pub fn root(&self) -> &PublicationTreeNode {
        self.tree.root()
    }

    /// Get a node by index
    pub fn get_node(&self, index: usize) -> Option<&PublicationTreeNode> {
        self.tree.get_node(index)
    }

    /// Get depth of a node
    pub fn depth(&self, index: usize) -> usize {
        self.tree.depth(index)
    }

    /// Get children of a node
    pub fn children(&self, index: usize) -> Option<Vec<&PublicationTreeNode>> {
        self.tree.children(index)
    }

    /// Check if a branch node should open as a new publication (depth overflow)
    pub fn should_open_as_new_publication(&self, index: usize) -> bool {
        self.depth(index) >= MAX_INLINE_DEPTH
            && self
                .get_node(index)
                .map(|n| n.node_type == NodeType::Branch)
                .unwrap_or(false)
    }

    /// Process incoming notes and resolve pending nodes
    ///
    /// Returns true if any nodes were resolved
    pub fn process_notes(&mut self, ndb: &Ndb, txn: &Transaction) -> bool {
        let mut resolved_any = false;
        let mut to_remove = Vec::new();

        let pending_count = self.pending_fetch.len();
        let tree_pending = self.tree.pending_count();

        if pending_count > 0 || tree_pending > 0 {
            debug!(
                "process_notes: pending_fetch={}, tree_pending={}, tree_resolved={}",
                pending_count,
                tree_pending,
                self.tree.resolved_count()
            );
        }

        for addr in &self.pending_fetch {
            if let Some(note) = find_note_by_address(ndb, txn, addr) {
                info!(
                    "Found note in nostrdb for {}:{}, resolving...",
                    addr.kind, addr.dtag
                );
                if self.tree.resolve_node(addr, note).is_some() {
                    resolved_any = true;
                    info!("Resolved node: {}:{}", addr.kind, addr.dtag);
                }
                to_remove.push(addr.clone());
            }
        }

        for addr in to_remove {
            self.pending_fetch.remove(&addr);
        }

        if resolved_any {
            info!(
                "process_notes: resolved some! tree now has {} resolved, {} pending",
                self.tree.resolved_count(),
                self.tree.pending_count()
            );
        }

        resolved_any
    }

    /// Mark addresses as being fetched
    pub fn mark_fetching(&mut self, addresses: &[EventAddress]) {
        for addr in addresses {
            self.pending_fetch.insert(addr.clone());
        }
    }
}

/// Tracks navigation history when drilling into nested publications beyond depth limit
#[derive(Debug, Clone, Default)]
pub struct PublicationHistory {
    /// Stack of parent publication index IDs (oldest first)
    stack: Vec<NoteId>,
}

impl PublicationHistory {
    /// Create new history starting at a root publication
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Push a new publication onto the history (when drilling into nested)
    pub fn push(&mut self, index_id: NoteId) {
        self.stack.push(index_id);
    }

    /// Pop back to previous publication
    pub fn pop(&mut self) -> Option<NoteId> {
        self.stack.pop()
    }

    /// Check if we can go back
    pub fn can_go_back(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Get breadcrumb trail (parent publications)
    pub fn breadcrumbs(&self) -> &[NoteId] {
        &self.stack
    }

    /// Get depth of history
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

/// Manages all publication views
#[derive(Default)]
pub struct Publications {
    /// Tree-based publication states
    pub publications: HashMap<NoteId, PublicationTreeState>,
}

impl Publications {
    /// Open a publication for viewing
    ///
    /// Creates the tree structure and subscribes to fetch pending sections
    pub fn open(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        index_id: &NoteId,
    ) -> Option<&mut PublicationTreeState> {
        // Check if already open
        if self.publications.contains_key(index_id) {
            return self.publications.get_mut(index_id);
        }

        // Get the index note
        let index_note = ndb.get_note_by_id(txn, index_id.bytes()).ok()?;

        // Create tree-based state
        let mut state = PublicationTreeState::from_root_note(&index_note)?;

        info!(
            "Opening publication '{}' with {} pending sections, {} total nodes",
            state.root().display_title(),
            state.tree.pending_count(),
            state.tree.node_count()
        );

        // Debug: list all pending addresses
        for addr in state.tree.pending_addresses() {
            debug!("  Pending: {}:{}:{}", addr.kind, hex::encode(&addr.pubkey[..8]), &addr.dtag);
        }

        // Diagnostic: check if ANY events from this author exist in nostrdb
        let root_author = index_note.pubkey();
        let diagnostic_filter = Filter::new()
            .kinds([30040, 30041])
            .authors([root_author])
            .limit(10)
            .build();
        if let Ok(results) = ndb.query(txn, &[diagnostic_filter], 10) {
            info!(
                "DIAGNOSTIC: Found {} events (30040/30041) from author {} in nostrdb",
                results.len(),
                hex::encode(&root_author[..8])
            );
            for res in results.iter().take(5) {
                if let Ok(note) = ndb.get_note_by_key(txn, res.note_key) {
                    let dtag = get_tag_value(&note, "d").unwrap_or("(none)");
                    debug!("  Found: kind={}, dtag={}", note.kind(), dtag);
                }
            }
        }

        // Try to resolve any pending nodes that are already in nostrdb
        // This handles the case where sections were fetched in a previous session
        let pre_resolved = self.try_resolve_existing(ndb, txn, &mut state);
        if pre_resolved > 0 {
            info!(
                "Pre-resolved {} nodes from existing nostrdb data, {} still pending",
                pre_resolved,
                state.tree.pending_count()
            );
        }

        // Log first few pending addresses for debugging
        let pending_addresses = state.tree.pending_addresses();
        if !pending_addresses.is_empty() {
            let sample: Vec<_> = pending_addresses.iter().take(3).collect();
            info!(
                "Sample of {} pending addresses: {:?}",
                pending_addresses.len(),
                sample.iter().map(|a| format!("{}:{}", a.kind, &a.dtag)).collect::<Vec<_>>()
            );
        }

        // Subscribe to fetch remaining pending sections
        self.subscribe_pending(pool, &mut state);

        // Log subscription status
        if let Some(ref sub_id) = state.sub_id {
            info!("Subscription created: {}, pending_fetch count: {}", sub_id, state.pending_fetch.len());
        } else {
            info!("No subscription created (no pending addresses or all resolved)");
        }

        self.publications.insert(*index_id, state);
        self.publications.get_mut(index_id)
    }

    /// Poll for updates - resolves pending nodes from nostrdb
    ///
    /// Returns true if any nodes were resolved
    pub fn poll_updates(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        index_id: &NoteId,
    ) -> bool {
        let Some(state) = self.publications.get_mut(index_id) else {
            return false;
        };

        let resolved = state.process_notes(ndb, txn);

        // Check if there are pending addresses that need to be subscribed for
        // This handles both: newly discovered children from resolved branches,
        // and addresses that weren't subscribed for initially
        let pending = state.needs_fetch();
        if !pending.is_empty() {
            debug!(
                "poll_updates: {} pending addresses need fetching",
                pending.len()
            );

            let filters = build_filters_for_addresses(&pending);

            if !filters.is_empty() {
                // Unsubscribe from old subscription if exists
                if let Some(old_sub) = state.sub_id.take() {
                    pool.unsubscribe(old_sub);
                }

                let sub_id = subscriptions::new_sub_id();
                info!(
                    "poll_updates: creating subscription {} for {} addresses",
                    sub_id,
                    pending.len()
                );
                pool.subscribe(sub_id.clone(), filters);
                state.sub_id = Some(sub_id);

                // Mark these as being fetched
                state.mark_fetching(&pending);
            }
        }

        resolved
    }

    /// Get a publication state
    pub fn get(&self, index_id: &NoteId) -> Option<&PublicationTreeState> {
        self.publications.get(index_id)
    }

    /// Get a mutable publication state
    pub fn get_mut(&mut self, index_id: &NoteId) -> Option<&mut PublicationTreeState> {
        self.publications.get_mut(index_id)
    }

    /// Close a publication view
    pub fn close(&mut self, pool: &mut RelayPool, index_id: &NoteId) {
        if let Some(state) = self.publications.remove(index_id) {
            // Unsubscribe if we have an active subscription
            if let Some(sub_id) = state.sub_id {
                pool.unsubscribe(sub_id);
            }
        }
    }

    /// Try to resolve pending nodes that already exist in nostrdb
    ///
    /// Iteratively resolves nodes until no more can be found (handles cascading
    /// resolution when branch nodes reveal new children that are also in nostrdb).
    ///
    /// Returns the number of nodes resolved
    fn try_resolve_existing(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
        state: &mut PublicationTreeState,
    ) -> usize {
        let mut total_resolved = 0;

        // Keep resolving until no more nodes can be found
        // This handles cascading: resolving a 30040 adds children, which might also exist
        loop {
            let pending: Vec<_> = state.tree.pending_addresses().into_iter().cloned().collect();
            if pending.is_empty() {
                break;
            }

            let mut resolved_this_round = 0;
            for addr in &pending {
                if let Some(note) = find_note_by_address(ndb, txn, addr) {
                    debug!(
                        "try_resolve_existing: found existing note for {}:{}",
                        addr.kind, addr.dtag
                    );
                    if state.tree.resolve_node(addr, note).is_some() {
                        resolved_this_round += 1;
                    }
                }
            }

            if resolved_this_round == 0 {
                // No progress, remaining nodes aren't in nostrdb
                break;
            }

            total_resolved += resolved_this_round;
            debug!(
                "try_resolve_existing: resolved {} this round, {} pending remain",
                resolved_this_round,
                state.tree.pending_count()
            );
        }

        total_resolved
    }

    /// Subscribe to fetch pending addresses
    fn subscribe_pending(&mut self, pool: &mut RelayPool, state: &mut PublicationTreeState) {
        self.subscribe_pending_internal(pool, state);
    }

    fn subscribe_pending_internal(
        &self,
        pool: &mut RelayPool,
        state: &mut PublicationTreeState,
    ) {
        let pending = state.needs_fetch();
        if pending.is_empty() {
            debug!("subscribe_pending_internal: no pending addresses to fetch");
            return;
        }

        info!(
            "Subscribing for {} pending addresses: {:?}",
            pending.len(),
            pending.iter().map(|a| format!("{}:{}", a.kind, &a.dtag)).collect::<Vec<_>>()
        );

        // Build filters grouped by (kind, author) - each address has its own pubkey from the a-tag
        let filters = build_filters_for_addresses(&pending);

        if filters.is_empty() {
            debug!("subscribe_pending_internal: no filters built");
            return;
        }

        info!("Built {} filters for subscription", filters.len());

        // Log relay status
        let relay_count = pool.relays.len();
        info!(
            "Relay pool has {} relays: {:?}",
            relay_count,
            pool.relays.iter().map(|r| r.url()).collect::<Vec<_>>()
        );

        // Unsubscribe from old subscription if exists
        if let Some(old_sub) = state.sub_id.take() {
            pool.unsubscribe(old_sub);
        }

        let sub_id = subscriptions::new_sub_id();
        pool.subscribe(sub_id.clone(), filters);
        state.sub_id = Some(sub_id.clone());
        info!("Created subscription: {}", sub_id);

        // Mark these as being fetched
        state.mark_fetching(&pending);
    }
}

/// Build nostrdb filters for a set of addresses, grouped by (kind, pubkey)
fn build_filters_for_addresses(addresses: &[EventAddress]) -> Vec<Filter> {
    if addresses.is_empty() {
        return vec![];
    }

    // Group addresses by (kind, pubkey) since different sections may have different authors
    let mut by_kind_and_author: HashMap<(u32, [u8; 32]), Vec<&str>> = HashMap::new();
    for addr in addresses {
        by_kind_and_author
            .entry((addr.kind, addr.pubkey))
            .or_default()
            .push(&addr.dtag);
    }

    // Build a filter for each (kind, author) combination
    let filters: Vec<Filter> = by_kind_and_author
        .into_iter()
        .map(|((kind, author), dtags)| {
            debug!(
                "Building filter: kind={}, author={}, dtags={:?}",
                kind,
                hex::encode(&author[..8]),
                dtags
            );
            Filter::new()
                .kinds([kind as u64])
                .authors([&author])
                .tags(dtags, 'd')
                .build()
        })
        .collect();

    debug!("Built {} filters total", filters.len());
    filters
}

/// Find a note by its event address in nostrdb
fn find_note_by_address<'a>(
    ndb: &'a Ndb,
    txn: &'a Transaction,
    addr: &EventAddress,
) -> Option<Note<'a>> {
    let filter = Filter::new()
        .kinds([addr.kind as u64])
        .authors([&addr.pubkey])
        .tags([addr.dtag.as_str()], 'd')
        .limit(1)
        .build();

    let results = ndb.query(txn, &[filter], 1).ok()?;

    if results.is_empty() {
        debug!(
            "find_note_by_address: NO MATCH for kind={}, author={}, dtag={}",
            addr.kind,
            hex::encode(&addr.pubkey[..8]),
            &addr.dtag
        );
        return None;
    }

    let note_key = results.first()?.note_key;
    debug!(
        "find_note_by_address: FOUND note_key={:?} for {}:{}",
        note_key, addr.kind, &addr.dtag
    );
    ndb.get_note_by_key(txn, note_key).ok()
}

/// Get a tag value by name from a note
pub fn get_tag_value<'a>(note: &'a Note, tag_name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() >= 2 {
            if let Some(name) = tag.get(0).and_then(|t| t.variant().str()) {
                if name == tag_name {
                    return tag.get(1).and_then(|t| t.variant().str());
                }
            }
        }
    }
    None
}
