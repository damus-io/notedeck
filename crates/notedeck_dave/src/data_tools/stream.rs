//! The agentium reads that keep going: `agentium log --follow` and `agentium
//! watch`, as [`StreamTool`]s a caller subscribes to over the socket, so
//! `nd agentium log <session> -f` and `nd agentium watch` are those commands.
//! `watch --once` answers one frame and is a plain read.
//!
//! Each stream subscribes to nostrdb in [`prepare`](StreamTool::prepare),
//! before its first read, so nothing that lands between the two is lost, and
//! then waits on that subscription, re-reading the snapshot on each wake, the
//! way the CLI does. The host's own sync is what lands a remote session's
//! events here, so a stream sees whatever this notedeck sees. Every stream
//! renders its own text (the lines to append, or the whole dashboard), so
//! `nd` only prints it.

use agentium_core::cli::list::{ListFilters, ListScope, SessionJson};
use agentium_core::cli::resolve_selected;
use agentium_core::cli::term::{now_secs, paint, status_style, StatusStyle};
use agentium_core::cli::transcript::{first_after, render_message, MessageFilter};
use agentium_core::cli::watch::{render_frame, select, statuses, transitions, updated_line, TICK};
use agentium_core::messages::Message;
use agentium_core::session_events::{AI_CONVERSATION_KIND, AI_SESSION_STATE_KIND};
use agentium_core::session_fold::{
    fold_sessions, reduce_delta, session_feed_filter, session_state_filter, SessionReducer,
    SessionView,
};
use agentium_core::session_loader::{
    load_deleted_session_states_for_author, load_session_messages_for_author,
    load_session_states_for_author, resolve_session_including_deleted, EventOrder,
};
use futures::StreamExt;
use nostrdb::{Filter, Ndb, NoteKey, SubscriptionStream, Transaction};
use nostrdb_net::Pubkey;
use notedeck::{
    AppTool, Emitter, RegisteredTool, StreamJob, StreamMode, StreamTool, ToolArg, ToolArgType,
    ToolContext, ToolSpec,
};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;

use super::{filter_args, open_txn, session_arg, view_args, FilterArgs, ViewArgs, TOOL_CATEGORY};

/// The streaming tools, and `watch --once`, ready to register.
pub(super) fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::stream(FollowLog),
        RegisteredTool::stream(Watch),
        RegisteredTool::stream(WatchTransitions),
        RegisteredTool::new(WatchOnce),
    ]
}

/// A stream of `filter`'s new notes in `ndb`, from now on.
fn subscribe(ndb: &Ndb, filter: Filter) -> Result<SubscriptionStream, String> {
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .map_err(|e| format!("subscribing to the database: {e}"))?;
    Ok(SubscriptionStream::new(ndb.clone(), sub))
}

/// A read transaction over an owned `ndb`, its error worded for the
/// subscriber.
fn txn(ndb: &Ndb) -> Result<Transaction, String> {
    Transaction::new(ndb).map_err(|e| format!("opening a db transaction: {e}"))
}

/// A switch that only picks its tool on a command line shared with another:
/// `--follow` on `agentium log`, `--once` and `--jsonl` on `agentium watch`.
fn picking_switch(name: &'static str, description: &'static str) -> ToolArg {
    ToolArg::new(name, ToolArgType::Boolean, description).required(true)
}

// ---------------------------------------------------------------------------
// log --follow
// ---------------------------------------------------------------------------

/// `agentium_follow_log`: one session's conversation, followed.
struct FollowLog;

/// Arguments for [`FollowLog`].
#[derive(Deserialize)]
struct FollowArgs {
    /// The selector.
    #[serde(default)]
    session: Option<String>,
    /// How the opening tail and what follows are filtered.
    #[serde(flatten)]
    view: ViewArgs,
}

impl StreamTool for FollowLog {
    type Args = FollowArgs;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![session_arg()];
        arguments.extend(view_args());
        arguments.push(
            picking_switch(
                "follow",
                "Follow the session: its tail, then each new message as it lands, until interrupted.",
            )
            .short('f'),
        );
        ToolSpec::new(
            "agentium_follow_log",
            "Follow one agentium session's conversation as it streams, as `agentium log --follow` does: the tail first, then each new message as it lands. Each event is one message's role-tagged object (`{role, text, …}`), or `{event: \"status\", status}` when the session's status changes. `last` bounds the opening tail; `role` and `tools` filter all of it.",
            arguments,
        )
        .titled("Follow agent session log")
        .read_only()
        .command(&["agentium", "log"])
        .filed_under(TOOL_CATEGORY)
        .streams(StreamMode::Append)
    }

    fn prepare(&self, cx: &mut ToolContext, args: FollowArgs) -> Result<StreamJob, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let state = {
            let txn = open_txn(cx)?;
            resolve_selected(cx.ndb, &txn, &author, args.session.as_deref())?
        };
        // Subscribed before the tail is read, so a message that lands
        // between the two still wakes the stream.
        let messages = subscribe(
            cx.ndb,
            conversation_filter(&author, &state.claude_session_id),
        )?;
        let states = subscribe(cx.ndb, session_state_filter(&author))?;
        let follow = Follow {
            ndb: cx.ndb.clone(),
            author,
            session_id: state.claude_session_id,
            status: state.status,
            filter: args.view.filter(),
            last_order: None,
            printed_any: false,
        };
        Ok(Box::new(move |emitter| {
            Box::pin(follow.run(emitter, messages, states))
        }))
    }
}

/// `author`'s conversation events in session `session_id`.
fn conversation_filter(author: &Pubkey, session_id: &str) -> Filter {
    Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .authors([author.bytes()])
        .tags([session_id], 'd')
        .build()
}

/// A followed session, and how far the stream has shown it.
struct Follow {
    ndb: Ndb,
    author: Pubkey,
    session_id: String,
    /// The status last shown, or the one it started with.
    status: String,
    filter: MessageFilter,
    /// The order of the newest message read, shown or filtered out: what
    /// follows is strictly past it.
    last_order: Option<EventOrder>,
    /// Whether any message has been shown, so the next is set off by a blank
    /// line, as `log` sets off each entry.
    printed_any: bool,
}

impl Follow {
    /// Stream the session until it, the database or the subscriber goes.
    async fn run(
        mut self,
        mut emitter: Emitter,
        mut messages: SubscriptionStream,
        mut states: SubscriptionStream,
    ) {
        if let Err(err) = self.follow(&mut emitter, &mut messages, &mut states).await {
            emitter.fail(err);
        }
    }

    /// The tail, then each wake's new messages and status change. `Ok` when
    /// the stream ends without an error.
    async fn follow(
        &mut self,
        emitter: &mut Emitter,
        messages: &mut SubscriptionStream,
        states: &mut SubscriptionStream,
    ) -> Result<(), String> {
        if !self.tail(emitter)? {
            return Ok(());
        }
        loop {
            tokio::select! {
                _ = emitter.gone() => return Ok(()),
                batch = messages.next() => if batch.is_none() { return Ok(()) },
                batch = states.next() => if batch.is_none() { return Ok(()) },
            }
            if !self.fresh(emitter)? || !self.status_change(emitter)? {
                return Ok(());
            }
        }
    }

    /// Show the opening tail, `--last` and all, and move the cursor past the
    /// whole conversation, not only what was shown. `false` once the
    /// subscriber is gone.
    fn tail(&mut self, emitter: &mut Emitter) -> Result<bool, String> {
        let txn = txn(&self.ndb)?;
        let loaded =
            load_session_messages_for_author(&self.ndb, &txn, &self.author, &self.session_id);
        let shown = self.filter.select(&loaded.messages);
        let open = emit_messages(emitter, shown.into_iter(), &mut self.printed_any);
        self.last_order = loaded.max_order;
        Ok(open)
    }

    /// Show the messages past the cursor that the filter keeps, and move it.
    /// A queued user message waits for its turn: it sorts past every real
    /// order until the host dispatches it, then shows once, where it joined.
    fn fresh(&mut self, emitter: &mut Emitter) -> Result<bool, String> {
        let txn = txn(&self.ndb)?;
        let loaded =
            load_session_messages_for_author(&self.ndb, &txn, &self.author, &self.session_id);
        let start = first_after(&loaded.orders, self.last_order);
        let filter = &self.filter;
        let fresh = loaded.messages[start..]
            .iter()
            .filter(|m| filter.keep(m) && !matches!(m, Message::User(user) if user.queued));
        let open = emit_messages(emitter, fresh, &mut self.printed_any);
        if loaded.max_order.is_some() {
            self.last_order = loaded.max_order;
        }
        Ok(open)
    }

    /// Show the session's status when it changed since last shown, as a line
    /// of its own (`── ◆ Needs Input ──`).
    fn status_change(&mut self, emitter: &mut Emitter) -> Result<bool, String> {
        let txn = txn(&self.ndb)?;
        let live = load_session_states_for_author(&self.ndb, &txn, &self.author);
        let deleted = load_deleted_session_states_for_author(&self.ndb, &txn, &self.author);
        let Ok(state) = resolve_session_including_deleted(&live, &deleted, &self.session_id) else {
            return Ok(true);
        };
        if state.status == self.status {
            return Ok(true);
        }
        self.status.clone_from(&state.status);
        let StatusStyle { glyph, label, sgr } = status_style(&self.status);
        let text = format!(
            "\n{}\n",
            paint(true, sgr, &format!("── {glyph} {label} ──"))
        );
        Ok(emitter.emit(&StatusEvent::new(&self.status), Some(text)))
    }
}

/// A followed session's new status, as `log --follow --json` prints it.
#[derive(Serialize)]
struct StatusEvent<'a> {
    /// Always `status`, telling it from a message.
    event: &'static str,
    status: &'a str,
}

impl<'a> StatusEvent<'a> {
    fn new(status: &'a str) -> Self {
        Self {
            event: "status",
            status,
        }
    }
}

/// Emit each of `messages` as its role-tagged object, its text set off from
/// the last one shown by a blank line. `false` once the subscriber is gone.
fn emit_messages<'m>(
    emitter: &mut Emitter,
    messages: impl Iterator<Item = &'m Message>,
    printed_any: &mut bool,
) -> bool {
    for message in messages {
        let mut text = String::new();
        if *printed_any {
            text.push('\n');
        }
        text.push_str(&render_message(message, true));
        *printed_any = true;
        if !emitter.emit(&message.to_json(), Some(text)) {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

/// Arguments for the `watch` tools: `list`'s filters.
#[derive(Deserialize)]
struct WatchArgs {
    #[serde(flatten)]
    filter: FilterArgs,
}

impl WatchArgs {
    /// The sessions a frame shows.
    fn selection(&self) -> Selection {
        Selection {
            filters: self.filter.filters(),
            scope: ListScope::from_flags(self.filter.all, self.filter.deleted),
        }
    }
}

/// Which of the folded sessions a frame shows, owned so a stream keeps it.
struct Selection {
    filters: ListFilters,
    scope: ListScope,
}

impl Selection {
    /// `fold`'s sessions this selection keeps, in dashboard order.
    fn rows<'f>(&self, fold: &'f SessionReducer) -> Vec<&'f SessionView> {
        select(fold, &self.filters, self.scope)
    }
}

/// The dashboard's sessions as JSON: each as `list --json` shows it, with
/// `last_activity` beside it, in dashboard order.
struct Frame<'a>(&'a [&'a SessionView]);

impl Serialize for Frame<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut frame = serializer.serialize_struct("Frame", 1)?;
        frame.serialize_field("sessions", &Rows(self.0))?;
        frame.end()
    }
}

/// A [`Frame`]'s rows.
struct Rows<'a>(&'a [&'a SessionView]);

impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// One row: the session as `list --json` shows it, and its last
        /// activity.
        #[derive(Serialize)]
        struct Row<'a> {
            last_activity: u64,
            #[serde(flatten)]
            session: SessionJson<'a>,
        }

        serializer.collect_seq(self.0.iter().map(|view| Row {
            last_activity: view.last_activity,
            session: SessionJson::new(&view.state),
        }))
    }
}

/// The filter argument descriptions of the `watch` tools, then `extra`.
fn watch_args(extra: Option<ToolArg>) -> Vec<ToolArg> {
    let mut arguments: Vec<ToolArg> = filter_args("Live and deleted sessions together.").into();
    arguments.extend(extra);
    arguments
}

/// Seed the session fold, as every `watch` does once its subscription is up.
fn seed(ndb: &Ndb, author: &Pubkey) -> Result<SessionReducer, String> {
    fold_sessions(ndb, &txn(ndb)?, author)
        .ok_or_else(|| "could not read sessions from the database".to_owned())
}

/// `agentium_watch`: the live dashboard.
struct Watch;

impl StreamTool for Watch {
    type Args = WatchArgs;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_watch",
            "A live dashboard of this account's agentium sessions, as `agentium watch` draws it: one row per session, the ones waiting on the user first, redrawn whenever a session changes and every 30s so ages keep moving. Each event is the whole frame: `{sessions: [{…session, agentium_uri, last_activity}]}`. Takes `list`'s filters.",
            watch_args(None),
        )
        .titled("Watch agent sessions")
        .read_only()
        .command(&["agentium", "watch"])
        .filed_under(TOOL_CATEGORY)
        .streams(StreamMode::Redraw)
    }

    fn prepare(&self, cx: &mut ToolContext, args: WatchArgs) -> Result<StreamJob, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let feed = subscribe(cx.ndb, session_feed_filter(&author))?;
        let dashboard = Dashboard {
            ndb: cx.ndb.clone(),
            author,
            selection: args.selection(),
        };
        Ok(Box::new(move |emitter| {
            Box::pin(dashboard.run(emitter, feed))
        }))
    }
}

/// A [`Watch`] stream's state.
struct Dashboard {
    ndb: Ndb,
    author: Pubkey,
    selection: Selection,
}

impl Dashboard {
    /// Draw, then redraw on each change and tick, until the subscriber or
    /// the database goes.
    async fn run(self, mut emitter: Emitter, mut feed: SubscriptionStream) {
        let mut fold = match seed(&self.ndb, &self.author) {
            Ok(fold) => fold,
            Err(err) => return emitter.fail(err),
        };
        let mut shown: Option<String> = None;
        let mut deferred: Vec<NoteKey> = Vec::new();
        let mut tick = tokio::time::interval(TICK);
        tick.tick().await; // the first tick is immediate; the loop draws anyway.
        loop {
            if !self.draw(&fold, &mut shown, &mut emitter) {
                return;
            }
            tokio::select! {
                _ = emitter.gone() => return,
                keys = feed.next() => {
                    let Some(keys) = keys else { return };
                    deferred.extend(keys);
                    match txn(&self.ndb) {
                        Ok(txn) => deferred = reduce_delta(&mut fold, &self.ndb, &txn, &deferred),
                        Err(err) => return emitter.fail(err),
                    }
                }
                _ = tick.tick() => {}
            }
        }
    }

    /// Emit the frame when it differs from the one `shown`, under a clock
    /// line. `false` once the subscriber is gone.
    fn draw(
        &self,
        fold: &SessionReducer,
        shown: &mut Option<String>,
        emitter: &mut Emitter,
    ) -> bool {
        let rows = self.selection.rows(fold);
        let now = now_secs();
        let frame = render_frame(&rows, now, true, None);
        if shown.as_deref() == Some(frame.as_str()) {
            return true;
        }
        let text = format!("{}\n{frame}", updated_line(now));
        *shown = Some(frame);
        emitter.emit(&Frame(&rows), Some(text))
    }
}

/// `agentium_watch_transitions`: `watch --jsonl`, a line per status change.
struct WatchTransitions;

impl StreamTool for WatchTransitions {
    type Args = WatchArgs;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_watch_transitions",
            "Stream each status change of this account's agentium sessions, as `agentium watch --jsonl` does, for a script to act on (notify when one is done): `{from, to, last_activity, session}`, `from` null for a session first seen mid-stream. The statuses when it starts are the baseline and print nothing. Takes `list`'s filters.",
            watch_args(Some(picking_switch(
                "jsonl",
                "Stream one JSON line per session status change instead of the dashboard.",
            ))),
        )
        .titled("Watch agent session status changes")
        .read_only()
        .command(&["agentium", "watch"])
        .filed_under(TOOL_CATEGORY)
        .streams(StreamMode::Append)
    }

    fn prepare(&self, cx: &mut ToolContext, args: WatchArgs) -> Result<StreamJob, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let feed = subscribe(cx.ndb, session_feed_filter(&author))?;
        let watcher = Transitions {
            ndb: cx.ndb.clone(),
            author,
            selection: args.selection(),
        };
        Ok(Box::new(move |emitter| {
            Box::pin(watcher.run(emitter, feed))
        }))
    }
}

/// A [`WatchTransitions`] stream's state.
struct Transitions {
    ndb: Ndb,
    author: Pubkey,
    selection: Selection,
}

impl Transitions {
    /// Emit each status change until the subscriber or the database goes.
    async fn run(self, mut emitter: Emitter, mut feed: SubscriptionStream) {
        let mut fold = match seed(&self.ndb, &self.author) {
            Ok(fold) => fold,
            Err(err) => return emitter.fail(err),
        };
        let mut seen = statuses(&self.selection.rows(&fold));
        let mut deferred: Vec<NoteKey> = Vec::new();
        loop {
            tokio::select! {
                _ = emitter.gone() => return,
                keys = feed.next() => {
                    let Some(keys) = keys else { return };
                    deferred.extend(keys);
                }
            }
            match self.fold_in(&mut fold, &mut seen, &mut deferred, &mut emitter) {
                Ok(true) => {}
                Ok(false) => return,
                Err(err) => return emitter.fail(err),
            }
        }
    }

    /// Fold `keys` in one at a time, diffing after each state revision: a
    /// quick turn's working and done revisions can arrive together, and
    /// folding them as one would hide the move. A key not visible yet is
    /// kept for the next wake. `false` once the subscriber is gone.
    fn fold_in(
        &self,
        fold: &mut SessionReducer,
        seen: &mut HashMap<String, String>,
        keys: &mut Vec<NoteKey>,
        emitter: &mut Emitter,
    ) -> Result<bool, String> {
        let txn = txn(&self.ndb)?;
        let mut later = Vec::new();
        for key in keys.drain(..) {
            let Ok(note) = self.ndb.get_note_by_key(&txn, key) else {
                later.push(key);
                continue;
            };
            fold.ingest(&note);
            if note.kind() != AI_SESSION_STATE_KIND {
                continue;
            }
            let rows = self.selection.rows(fold);
            for transition in transitions(seen, &rows) {
                let mut line = serde_json::to_string(&transition)
                    .map_err(|e| format!("failed to serialize a transition: {e}"))?;
                line.push('\n');
                if !emitter.emit(&transition, Some(line)) {
                    return Ok(false);
                }
            }
            *seen = statuses(&rows);
        }
        *keys = later;
        Ok(true)
    }
}

/// `agentium_watch_once`: one dashboard frame.
struct WatchOnce;

/// [`WatchOnce`]'s answer: the frame's sessions, and its text.
struct Snapshot {
    views: Vec<SessionView>,
    frame: String,
}

impl Serialize for Snapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let rows: Vec<&SessionView> = self.views.iter().collect();
        Frame(&rows).serialize(serializer)
    }
}

impl AppTool for WatchOnce {
    type Args = WatchArgs;
    type Output = Snapshot;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_watch_once",
            "One frame of the agentium session dashboard, as `agentium watch --once` prints it: `{sessions: [{…session, agentium_uri, last_activity}]}`, the ones waiting on the user first. Takes `list`'s filters.",
            watch_args(Some(picking_switch(
                "once",
                "Print one frame and exit, rather than following.",
            ))),
        )
        .titled("Show agent session dashboard")
        .read_only()
        .command(&["agentium", "watch"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: WatchArgs) -> Result<Snapshot, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let fold = seed(cx.ndb, &author)?;
        let rows = args.selection().rows(&fold);
        let frame = render_frame(&rows, now_secs(), true, None);
        Ok(Snapshot {
            views: rows.into_iter().cloned().collect(),
            frame,
        })
    }

    fn render(&self, out: &Snapshot) -> Option<String> {
        Some(out.frame.clone())
    }
}
