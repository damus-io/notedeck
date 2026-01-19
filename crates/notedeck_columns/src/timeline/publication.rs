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

/// Default maximum nodes to resolve from cache per frame (to avoid UI freezing)
const DEFAULT_MAX_NODES_PER_FRAME: usize = 10;

/// Strategy for resolving pending nodes in publications
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ResolutionStrategy {
    /// Resolve N nodes per frame, spreading work across frames (default, prevents freezing)
    #[default]
    Incremental,
    /// Resolve all pending nodes at once (may cause UI freezing for large publications)
    AllAtOnce,
    /// Only resolve nodes as they become visible (requires visibility tracking)
    LazyOnDemand,
}

/// Configuration for publication loading behavior
#[derive(Clone, Debug)]
pub struct PublicationConfig {
    /// Strategy for resolving pending nodes
    pub resolution_strategy: ResolutionStrategy,
    /// Maximum nodes to resolve per frame (for Incremental strategy)
    pub max_nodes_per_frame: usize,
}

impl Default for PublicationConfig {
    fn default() -> Self {
        Self {
            resolution_strategy: ResolutionStrategy::Incremental,
            max_nodes_per_frame: DEFAULT_MAX_NODES_PER_FRAME,
        }
    }
}

/// Number of nodes to prefetch beyond visible area for smooth scrolling
const VISIBILITY_PREFETCH_BUFFER: usize = 5;

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

    /// Node indices currently visible on screen (for lazy resolution)
    visible_nodes: HashSet<usize>,
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
            visible_nodes: HashSet::new(),
        })
    }

    /// Set the currently visible node indices (for lazy resolution)
    ///
    /// Call this from the UI layer to inform which nodes are on screen.
    /// The resolution system will prioritize these nodes and prefetch nearby ones.
    pub fn set_visible_nodes(&mut self, nodes: impl IntoIterator<Item = usize>) {
        self.visible_nodes.clear();
        self.visible_nodes.extend(nodes);
    }

    /// Get pending addresses that are visible or near visible nodes
    ///
    /// Returns addresses for visible pending nodes plus a prefetch buffer.
    /// Used when `LazyOnDemand` strategy is active.
    pub fn visible_pending_addresses(&self) -> Vec<EventAddress> {
        if self.visible_nodes.is_empty() {
            // No visibility info - return first few pending addresses
            return self
                .tree
                .pending_addresses()
                .into_iter()
                .take(VISIBILITY_PREFETCH_BUFFER)
                .cloned()
                .collect();
        }

        // Collect addresses for visible nodes and their children (for prefetch)
        let mut addresses = Vec::new();
        let mut visited = HashSet::new();

        for &node_idx in &self.visible_nodes {
            self.collect_node_addresses(node_idx, &mut addresses, &mut visited);

            // Also prefetch children if this is a branch node
            if let Some(children) = self.tree.children(node_idx) {
                for child in children {
                    if let Some(child_idx) = self.tree.get_index(&child.address) {
                        self.collect_node_addresses(child_idx, &mut addresses, &mut visited);
                    }
                }
            }
        }

        // Add some buffer from siblings of visible nodes
        for &node_idx in &self.visible_nodes {
            let (prev, next) = self.tree.siblings(node_idx);
            if let Some(prev_idx) = prev {
                self.collect_node_addresses(prev_idx, &mut addresses, &mut visited);
            }
            if let Some(next_idx) = next {
                self.collect_node_addresses(next_idx, &mut addresses, &mut visited);
            }
        }

        addresses
    }

    /// Helper to collect pending address for a node if it's not already fetching
    fn collect_node_addresses(
        &self,
        node_idx: usize,
        addresses: &mut Vec<EventAddress>,
        visited: &mut HashSet<usize>,
    ) {
        if visited.contains(&node_idx) {
            return;
        }
        visited.insert(node_idx);

        if let Some(node) = self.tree.get_node(node_idx) {
            if !node.is_resolved() && !self.pending_fetch.contains(&node.address) {
                addresses.push(node.address.clone());
            }
        }
    }

    /// Get addresses that need to be fetched from relays
    ///
    /// Returns up to `batch_size` addresses to avoid overwhelming relays.
    /// Use `RelayInfoCache::min_max_event_tags()` to determine the batch size
    /// based on connected relay limits.
    pub fn needs_fetch(&self, batch_size: usize) -> Vec<EventAddress> {
        self.tree
            .pending_addresses()
            .into_iter()
            .filter(|addr| !self.pending_fetch.contains(addr))
            .take(batch_size)
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

/// Manages all publication views
pub struct Publications {
    /// Tree-based publication states
    pub publications: HashMap<NoteId, PublicationTreeState>,
    /// Configuration for loading behavior
    pub config: PublicationConfig,
}

impl Default for Publications {
    fn default() -> Self {
        Self {
            publications: HashMap::new(),
            config: PublicationConfig::default(),
        }
    }
}

impl Publications {
    /// Open a publication for viewing
    ///
    /// Creates the tree structure and subscribes to fetch pending sections.
    ///
    /// `batch_size` controls how many addresses to fetch per subscription.
    /// Use `RelayInfoCache::min_max_event_tags()` to get this value based on relay limits.
    pub fn open(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        index_id: &NoteId,
        batch_size: usize,
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

        // Try to resolve pending nodes that are already in nostrdb
        // This handles the case where sections were fetched in a previous session
        // Behavior depends on resolution strategy:
        // - AllAtOnce: resolve everything (may freeze UI for large publications)
        // - Incremental: resolve limited per frame (remaining resolved via poll_updates)
        // - LazyOnDemand: don't pre-resolve (resolved when visible)
        let max_to_resolve = match self.config.resolution_strategy {
            ResolutionStrategy::AllAtOnce => usize::MAX,
            ResolutionStrategy::Incremental => self.config.max_nodes_per_frame,
            ResolutionStrategy::LazyOnDemand => 0,
        };
        let pre_resolved = self.try_resolve_existing(ndb, txn, &mut state, max_to_resolve);
        if pre_resolved > 0 {
            info!(
                "Pre-resolved {} nodes from existing nostrdb data, {} still pending (incremental resolution continues)",
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
        self.subscribe_pending(pool, &mut state, batch_size);

        // Log subscription status
        if let Some(ref sub_id) = state.sub_id {
            info!(
                "Subscription created: {}, fetching {} of {} total pending",
                sub_id,
                state.pending_fetch.len(),
                state.tree.pending_count()
            );
        } else {
            info!("No subscription created (no pending addresses or all resolved)");
        }

        self.publications.insert(*index_id, state);
        self.publications.get_mut(index_id)
    }

    /// Poll for updates - resolves pending nodes from nostrdb
    ///
    /// `batch_size` controls how many addresses to fetch per subscription.
    /// Use `RelayInfoCache::min_max_event_tags()` to get this value based on relay limits.
    ///
    /// Returns true if any nodes were resolved
    pub fn poll_updates(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        index_id: &NoteId,
        batch_size: usize,
    ) -> bool {
        // Copy config values to avoid borrow conflicts
        let resolution_strategy = self.config.resolution_strategy;
        let max_nodes_per_frame = self.config.max_nodes_per_frame;

        let Some(state) = self.publications.get_mut(index_id) else {
            return false;
        };

        // Process notes that were fetched from subscriptions (pending_fetch)
        let mut resolved = state.process_notes(ndb, txn);

        // Resolution behavior depends on strategy
        match resolution_strategy {
            ResolutionStrategy::Incremental => {
                // Try to resolve more nodes from cache each frame
                if state.tree.has_pending() {
                    let cache_resolved =
                        Self::try_resolve_existing_on_state(ndb, txn, state, max_nodes_per_frame);
                    if cache_resolved > 0 {
                        debug!(
                            "poll_updates: incrementally resolved {} nodes from cache, {} pending remain",
                            cache_resolved,
                            state.tree.pending_count()
                        );
                        resolved = true;
                    }
                }
            }
            ResolutionStrategy::LazyOnDemand => {
                // Only resolve visible nodes from cache
                let visible_addrs = state.visible_pending_addresses();
                if !visible_addrs.is_empty() {
                    for addr in &visible_addrs {
                        if let Some(note) = find_note_by_address(ndb, txn, addr) {
                            if state.tree.resolve_node(addr, note).is_some() {
                                debug!("poll_updates: lazy-resolved visible node {}:{}", addr.kind, addr.dtag);
                                resolved = true;
                            }
                        }
                    }
                }
            }
            ResolutionStrategy::AllAtOnce => {
                // AllAtOnce resolved everything in open(), nothing to do here
            }
        }

        // Check if there are pending addresses that need to be subscribed for
        // For LazyOnDemand, only subscribe for visible nodes
        // For others, subscribe for any pending addresses
        let pending = if resolution_strategy == ResolutionStrategy::LazyOnDemand {
            let visible = state.visible_pending_addresses();
            // Filter to those not already being fetched and limit by batch_size
            visible
                .into_iter()
                .filter(|addr| !state.pending_fetch.contains(addr))
                .take(batch_size)
                .collect()
        } else {
            state.needs_fetch(batch_size)
        };
        if !pending.is_empty() {
            debug!(
                "poll_updates: {} pending addresses need fetching (batch_size={})",
                pending.len(),
                batch_size
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
    /// Resolves up to `max_nodes` pending nodes per call to avoid UI freezing.
    /// Handles cascading resolution when branch nodes reveal new children.
    ///
    /// Returns the number of nodes resolved
    fn try_resolve_existing(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
        state: &mut PublicationTreeState,
        max_nodes: usize,
    ) -> usize {
        Self::try_resolve_existing_on_state(ndb, txn, state, max_nodes)
    }

    /// Static helper to resolve pending nodes from cache
    /// Used by both try_resolve_existing and poll_updates
    fn try_resolve_existing_on_state(
        ndb: &Ndb,
        txn: &Transaction,
        state: &mut PublicationTreeState,
        max_nodes: usize,
    ) -> usize {
        let mut total_resolved = 0;

        // Keep resolving until we hit the limit or no more nodes can be found
        // This handles cascading: resolving a 30040 adds children, which might also exist
        loop {
            if total_resolved >= max_nodes {
                debug!(
                    "try_resolve_existing: hit limit of {} nodes, {} pending remain",
                    max_nodes,
                    state.tree.pending_count()
                );
                break;
            }

            let remaining = max_nodes - total_resolved;
            let pending: Vec<_> = state
                .tree
                .pending_addresses()
                .into_iter()
                .take(remaining)
                .cloned()
                .collect();
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
    ///
    /// `batch_size` controls how many addresses to fetch per subscription.
    fn subscribe_pending(
        &mut self,
        pool: &mut RelayPool,
        state: &mut PublicationTreeState,
        batch_size: usize,
    ) {
        self.subscribe_pending_internal(pool, state, batch_size);
    }

    fn subscribe_pending_internal(
        &self,
        pool: &mut RelayPool,
        state: &mut PublicationTreeState,
        batch_size: usize,
    ) {
        let pending = state.needs_fetch(batch_size);
        if pending.is_empty() {
            debug!("subscribe_pending_internal: no pending addresses to fetch");
            return;
        }

        info!(
            "Subscribing for {} pending addresses (batch_size={})",
            pending.len(),
            batch_size
        );
        debug!(
            "Addresses: {:?}",
            pending.iter().take(5).map(|a| format!("{}:{}", a.kind, &a.dtag)).collect::<Vec<_>>()
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
