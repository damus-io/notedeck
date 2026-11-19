//! Read-only nostr data tools — a profile, a thread, a timeline — so `nd
//! nostr profile|thread|timeline` reads what this notedeck has seen, and an
//! agent spec with a `Notedeck { apps: ["nostr"] }` toolset can look around
//! the user's nostr world from the native loop.
//!
//! ```text
//! nd nostr profile  [who]            ─┐  prepare: read the local nostrdb
//! nd nostr thread   <note>            ├─   hit  ─▶ job answers it
//! nd nostr timeline [--author|--hashtag] ┘   miss ─▶ ToolContext::fetch (REQ)
//!                                                     job: wait ≤ FETCH_BOUND
//!                                                          for the notes,
//!                                                          read the db again
//! ```
//!
//! Every tool reads the local nostrdb first: what the columns, Dave or any
//! other app has already pulled from relays. On a miss (a profile it has
//! no kind 0 for, a thread's note or ancestor it lacks, a timeline with no
//! notes) it asks the selected account's read relays through
//! [`ToolContext::fetch`] and its job, off the render loop, waits up to
//! [`FETCH_BOUND`] for the answer to land before reading the db again.
//! What still isn't there answers as such (`found: false`, a `missing` id).
//! The wait is for notes, not the relays' EOSE, so a miss nobody can answer
//! costs the whole bound. Each is [`read_only`](ToolSpec::read_only), so the
//! capability table makes it `Observe`: auto-approved in every session
//! mode, Plan included.
//!
//! Notes are answered as the engine's [`SimpleNote`], the shape Dave's
//! `query` tool and thread summaries already hand a model.
//!
//! Showing the user one of these in the window, rather than reading it, is
//! [`open`]'s job: `nd nostr open-profile|open-thread|open-hashtag`. Acting
//! as the user (post, reply, react, follow, zap) is [`act`]'s, and every one
//! of those asks first.

use std::time::Duration;

use nostrdb::{Filter, Ndb, Note, NoteReply, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::{
    note_to_simple, RegisteredTool, RelayFetch, SimpleNote, SlowTool, ToolArg, ToolArgType,
    ToolContext, ToolJob, ToolSpec,
};
use serde::{Deserialize, Serialize};

/// What the nostr tools are filed under in a command bar.
const TOOL_CATEGORY: &str = "Nostr";

/// Notes a timeline answers when the caller doesn't say.
const DEFAULT_TIMELINE_LIMIT: u64 = 20;

/// Replies a thread answers when the caller doesn't say.
const DEFAULT_THREAD_LIMIT: u64 = 50;

/// The most notes any one call answers, whatever the caller asks for: a
/// model's context is the budget, not the db.
const MAX_LIMIT: u64 = 200;

/// How far up a reply chain [`LoadThread`] walks before it stops: a loop of
/// e-tags (or a hostile one) never walks forever.
const MAX_ANCESTORS: usize = 64;

/// The longest a read waits for the relays to answer what the local db
/// lacked: long enough for a relay round trip, short enough that a model
/// asking about a key nobody has heard of isn't kept waiting.
pub const FETCH_BOUND: Duration = Duration::from_secs(4);

/// Every nostr data tool, ready to register: the reads here, then
/// [`act`]'s writes (post, reply, react, follow, unfollow).
pub fn tools() -> Vec<RegisteredTool> {
    tools_within(FETCH_BOUND)
}

/// [`tools`], with reads that wait at most `bound` on a fetch.
fn tools_within(bound: Duration) -> Vec<RegisteredTool> {
    let mut tools = vec![
        RegisteredTool::slow(GetProfile { bound }),
        RegisteredTool::slow(LoadThread { bound }),
        RegisteredTool::slow(ReadTimeline { bound }),
    ];
    tools.extend(act::tools());
    tools
}

/// A read transaction, its error worded for the caller.
fn open_txn(ndb: &Ndb) -> Result<Transaction, String> {
    Transaction::new(ndb).map_err(|e| format!("opening a db transaction: {e}"))
}

/// A read's job when the db already had the answer.
fn answered<O: Send + 'static>(out: O) -> ToolJob<O> {
    Box::new(move || Ok(out))
}

/// What a read's job waits on before it reads the db again: a fetch, and
/// how many of its notes are enough.
struct Refetch {
    fetch: RelayFetch,
    enough: u32,
}

/// Ask the relays for `filters` on a miss, to wait in the job for `enough`
/// of them; `None` in a context that can't fetch (no
/// [`remote`](ToolContext::remote)), where the read answers from the db
/// alone.
fn refetch(
    cx: &mut ToolContext,
    filters: Vec<Filter>,
    enough: u32,
    bound: Duration,
) -> Option<Refetch> {
    match cx.fetch(filters, bound) {
        Ok(fetch) => Some(Refetch { fetch, enough }),
        Err(err) => {
            tracing::debug!("nostr read: answering from the db alone: {err}");
            None
        }
    }
}

/// A read's job after a miss: wait on `refetch`, then answer `read` of the
/// db, whatever landed.
fn reread<O: 'static>(
    ndb: &Ndb,
    refetch: Refetch,
    read: impl FnOnce(&Ndb) -> Result<O, String> + Send + 'static,
) -> ToolJob<O> {
    let ndb = ndb.clone();
    Box::new(move || {
        let end = refetch.fetch.wait(refetch.enough);
        tracing::debug!("nostr read: fetch ended {end:?}");
        read(&ndb)
    })
}

/// `s` without a `nostr:` URI prefix.
fn strip_uri(s: &str) -> &str {
    s.trim().strip_prefix("nostr:").unwrap_or(s.trim())
}

/// A pubkey from an `npub1…`, an `nprofile1…` or 64 hex characters, with or
/// without a `nostr:` prefix.
fn parse_pubkey(s: &str) -> Result<Pubkey, String> {
    let s = strip_uri(s);
    Pubkey::parse(s)
        .ok()
        .or_else(|| Pubkey::from_nprofile_bech(s))
        .ok_or_else(|| format!("{s:?} is not a pubkey: give an npub, an nprofile or 64 hex"))
}

/// A note id from a `note1…`, an `nevent1…` or 64 hex characters, with or
/// without a `nostr:` prefix.
fn parse_note_id(s: &str) -> Result<NoteId, String> {
    let s = strip_uri(s);
    NoteId::from_hex(s)
        .ok()
        .or_else(|| NoteId::from_bech(s))
        .or_else(|| NoteId::from_nevent_bech(s))
        .ok_or_else(|| format!("{s:?} is not a note id: give a note1, an nevent or 64 hex"))
}

/// `limit`, or `default`, kept within `1..=MAX_LIMIT`.
fn clamp_limit(limit: Option<u64>, default: u64) -> u64 {
    limit.unwrap_or(default).clamp(1, MAX_LIMIT)
}

/// The pubkeys `author`'s newest contact list (kind 3) follows, or `None`
/// when this notedeck has no contact list of theirs.
fn follows(ndb: &nostrdb::Ndb, txn: &Transaction, author: &Pubkey) -> Option<Vec<[u8; 32]>> {
    let filter = notedeck::contacts::contacts_filter(author.bytes());
    let result = ndb.query(txn, &[filter], 1).ok()?.into_iter().next()?;
    Some(
        result
            .note
            .tags()
            .into_iter()
            .filter(|tag| tag.count() >= 2 && tag.get_str(0) == Some("p"))
            .filter_map(|tag| tag.get_id(1).copied())
            .collect(),
    )
}

/// The first line of `content`, cut to `max` characters, for a rendered row.
fn first_line(content: &str, max: usize) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max || content.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        out.push('…');
    }
    out
}

/// `notes` as rows for a person: when, who, the short id, the first line.
fn render_notes(notes: &[SimpleNote]) -> String {
    let mut out = String::new();
    for note in notes {
        let short = note.note_id.get(..8).unwrap_or(&note.note_id);
        out.push_str(&format!(
            "{}  {}  {}\n    {}\n",
            note.created_at,
            note.name,
            short,
            first_line(&note.content, 100)
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// get_profile
// ---------------------------------------------------------------------------

/// `nd nostr profile [who]`: one person's kind-0 profile, fetched from the
/// relays when the db has none.
struct GetProfile {
    /// How long a fetch is waited on.
    bound: Duration,
}

/// [`GetProfile`]'s arguments.
#[derive(Deserialize)]
struct ProfileArgs {
    /// Whose profile; the selected account's when absent.
    #[serde(default)]
    who: Option<String>,
}

/// What [`GetProfile`] answers. Every profile field is `None` when neither
/// this notedeck nor the account's relays (within the wait) had a kind 0
/// for the key (`found: false`).
#[derive(Debug, Serialize)]
struct ProfileOut {
    /// The key, hex.
    pubkey: String,
    /// The key, `npub1…`.
    npub: String,
    /// Whether a kind-0 profile for the key was found, locally or fetched.
    found: bool,
    name: Option<String>,
    display_name: Option<String>,
    about: Option<String>,
    picture: Option<String>,
    banner: Option<String>,
    website: Option<String>,
    nip05: Option<String>,
    lud16: Option<String>,
    /// How many keys their newest contact list follows; `None` when this
    /// notedeck has no contact list of theirs.
    following: Option<usize>,
    /// Whether the selected account's contact list follows them; `None`
    /// when this notedeck has no contact list of the account's.
    followed_by_you: Option<bool>,
    /// Whether the key is the selected account's own.
    is_you: bool,
}

impl SlowTool for GetProfile {
    type Args = ProfileArgs;
    type Output = ProfileOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_get_profile",
            "Read a nostr user's profile (kind 0): `{pubkey, npub, found, name, display_name, about, picture, banner, website, nip05, lud16, following, followed_by_you, is_you}`. Read from the local nostrdb, or, when it has none, fetched from the user's relays (a wait of a few seconds). `found: false` means neither had their profile, not that the key is wrong. With no `who`, the user's own profile.",
            vec![ToolArg::new(
                "who",
                ToolArgType::String,
                "Whose profile: an npub, an nprofile or a 64-hex pubkey (a `nostr:` prefix is fine). Defaults to the user's own account.",
            )
            .positional()],
        )
        .titled("Show nostr profile")
        .read_only()
        .command(&["nostr", "profile"])
        .filed_under(TOOL_CATEGORY)
    }

    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<Self::Output>, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let pubkey = match args.who.as_deref() {
            Some(who) => parse_pubkey(who)?,
            None => me,
        };
        let out = read_profile(cx.ndb, &me, &pubkey)?;
        if out.found {
            return Ok(answered(out));
        }
        let filter = Filter::new()
            .authors([pubkey.bytes()])
            .kinds([0])
            .limit(1)
            .build();
        let Some(refetch) = refetch(cx, vec![filter], 1, self.bound) else {
            return Ok(answered(out));
        };
        Ok(reread(cx.ndb, refetch, move |ndb| {
            read_profile(ndb, &me, &pubkey)
        }))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let mut text = String::new();
        let name = out
            .display_name
            .as_deref()
            .or(out.name.as_deref())
            .unwrap_or("(no profile)");
        text.push_str(&format!("{name}\n{}\n", out.npub));
        let rows = [
            ("name", out.name.as_deref()),
            ("nip05", out.nip05.as_deref()),
            ("website", out.website.as_deref()),
            ("lud16", out.lud16.as_deref()),
            ("picture", out.picture.as_deref()),
        ];
        for (label, value) in rows {
            if let Some(value) = value {
                text.push_str(&format!("{label:<9}{value}\n"));
            }
        }
        if let Some(n) = out.following {
            text.push_str(&format!("{:<9}{n}\n", "follows"));
        }
        if let Some(yes) = out.followed_by_you {
            text.push_str(&format!(
                "{:<9}{}\n",
                "you",
                if yes { "follow" } else { "don't follow" }
            ));
        }
        if let Some(about) = &out.about {
            text.push_str(&format!("\n{about}\n"));
        }
        Some(text)
    }
}

/// `pubkey`'s profile as the db holds it now, with the follow graph as seen
/// from `me`.
fn read_profile(ndb: &Ndb, me: &Pubkey, pubkey: &Pubkey) -> Result<ProfileOut, String> {
    let txn = open_txn(ndb)?;
    let record = ndb.get_profile_by_pubkey(&txn, pubkey.bytes()).ok();
    let profile = record.as_ref().and_then(|r| r.record().profile());
    let field = |value: Option<&str>| value.map(str::to_owned);
    let followed_by_you = follows(ndb, &txn, me).map(|f| f.contains(pubkey.bytes()));
    Ok(ProfileOut {
        pubkey: pubkey.hex(),
        npub: pubkey.npub().unwrap_or_default(),
        found: profile.is_some(),
        name: field(profile.as_ref().and_then(|p| p.name())),
        display_name: field(profile.as_ref().and_then(|p| p.display_name())),
        about: field(profile.as_ref().and_then(|p| p.about())),
        picture: field(profile.as_ref().and_then(|p| p.picture())),
        banner: field(profile.as_ref().and_then(|p| p.banner())),
        website: field(profile.as_ref().and_then(|p| p.website())),
        nip05: field(profile.as_ref().and_then(|p| p.nip05())),
        lud16: field(profile.as_ref().and_then(|p| p.lud16())),
        following: follows(ndb, &txn, pubkey).map(|f| f.len()),
        followed_by_you,
        is_you: pubkey == me,
    })
}

// ---------------------------------------------------------------------------
// load_thread
// ---------------------------------------------------------------------------

/// `nd nostr thread <note>`: the conversation a note is part of, its note
/// or ancestors fetched from the relays when the db lacks them.
struct LoadThread {
    /// How long a fetch is waited on.
    bound: Duration,
}

/// [`LoadThread`]'s arguments.
#[derive(Deserialize)]
struct ThreadArgs {
    /// Any note in the thread.
    note: String,
    /// How many replies to answer at most.
    #[serde(default)]
    limit: Option<u64>,
}

/// What [`LoadThread`] answers.
#[derive(Debug, Serialize)]
struct ThreadOut {
    /// The thread's root note id, hex: the asked-for note's NIP-10 root, or
    /// the note itself when it replies to nothing.
    root: String,
    /// The note asked for, hex.
    focus: String,
    /// The root, the asked-for note's ancestors, the note and the replies
    /// under the root, oldest first, each once. A note's `reply_to` names
    /// its parent, so the tree can be rebuilt from this list.
    notes: Vec<SimpleNote>,
    /// Ids, hex, of notes the thread references (the root, an ancestor)
    /// that neither this notedeck nor, within the wait, the account's
    /// relays had.
    missing: Vec<String>,
}

impl SlowTool for LoadThread {
    type Args = ThreadArgs;
    type Output = ThreadOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_load_thread",
            "Load the nostr thread a note belongs to: `{root, focus, notes, missing}`, read from the local nostrdb, with the note or its ancestors fetched from the user's relays when it lacks them (a wait of a few seconds). `notes` holds the root, the note's ancestors, the note and the replies under the root, oldest first, each `{note_id, pubkey, name, content, created_at, note_kind, reply_to}`; `reply_to` is the parent's id, so the tree can be rebuilt. `missing` lists referenced notes that couldn't be found; calling again fetches the next ones up.",
            vec![
                ToolArg::new(
                    "note",
                    ToolArgType::String,
                    "Any note in the thread: a note1, an nevent or a 64-hex note id (a `nostr:` prefix is fine).",
                )
                .required(true)
                .positional(),
                ToolArg::new(
                    "limit",
                    ToolArgType::Integer,
                    "The most replies to answer, newest kept (default 50, at most 200).",
                ),
            ],
        )
        .titled("Show nostr thread")
        .read_only()
        .command(&["nostr", "thread"])
        .filed_under(TOOL_CATEGORY)
    }

    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<Self::Output>, String> {
        let focus_id = parse_note_id(&args.note)?;
        let limit = clamp_limit(args.limit, DEFAULT_THREAD_LIMIT);
        let have_focus = {
            let txn = open_txn(cx.ndb)?;
            let found = cx.ndb.get_note_by_id(&txn, focus_id.bytes()).is_ok();
            found
        };
        // Missing the note itself, fetch it; missing an ancestor (or the
        // root), fetch those. Each call fetches what it found missing, so a
        // deep chain the relays answer one hop at a time fills in over
        // calls.
        let wanted: Vec<[u8; 32]> = if have_focus {
            let out = read_thread(cx.ndb, &focus_id, limit)?;
            if out.missing.is_empty() {
                return Ok(answered(out));
            }
            out.missing
                .iter()
                .filter_map(|id| NoteId::from_hex(id).ok())
                .map(|id| *id.bytes())
                .collect()
        } else {
            vec![*focus_id.bytes()]
        };
        let filter = Filter::new()
            .ids(wanted.iter())
            .limit(wanted.len() as u64)
            .build();
        let enough = wanted.len() as u32;
        let Some(refetch) = refetch(cx, vec![filter], enough, self.bound) else {
            return read_thread(cx.ndb, &focus_id, limit).map(answered);
        };
        let bound = self.bound;
        Ok(reread(cx.ndb, refetch, move |ndb| {
            read_thread(ndb, &focus_id, limit).map_err(|err| {
                format!("{err}, and the user's relays didn't answer with it within {bound:?}")
            })
        }))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let mut text = render_notes(&out.notes);
        for id in &out.missing {
            text.push_str(&format!("(missing {id})\n"));
        }
        Some(text)
    }
}

/// The thread `focus_id` is part of as the db holds it now: an error when
/// the db lacks the note itself.
fn read_thread(ndb: &Ndb, focus_id: &NoteId, limit: u64) -> Result<ThreadOut, String> {
    let txn = open_txn(ndb)?;
    let focus = ndb
        .get_note_by_id(&txn, focus_id.bytes())
        .map_err(|_| format!("note {} isn't in the local db", focus_id.hex()))?;

    let mut thread = Thread::default();
    let mut missing = Vec::new();
    thread.push(&txn, ndb, &focus);

    // Walk the parents up: a reply's root may tag only the root, so the
    // chain between it and the asked-for note comes from here.
    let mut parent = NoteReply::new(focus.tags()).reply().map(|r| *r.id);
    while let Some(id) = parent.take() {
        if thread.ids.len() > MAX_ANCESTORS || thread.ids.contains(&id) {
            break;
        }
        let Ok(note) = ndb.get_note_by_id(&txn, &id) else {
            missing.push(hex::encode(id));
            break;
        };
        thread.push(&txn, ndb, &note);
        parent = NoteReply::new(note.tags()).reply().map(|r| *r.id);
    }

    let root = NoteReply::new(focus.tags())
        .root()
        .map(|r| *r.id)
        .unwrap_or(*focus_id.bytes());
    // The root may be the parent the walk already found missing.
    let root_hex = hex::encode(root);
    if !thread.ids.contains(&root) && !missing.contains(&root_hex) {
        match ndb.get_note_by_id(&txn, &root) {
            Ok(note) => thread.push(&txn, ndb, &note),
            Err(_) => missing.push(root_hex.clone()),
        }
    }

    let replies = Filter::new().kinds([1]).event(&root).build();
    let results = ndb
        .query(&txn, &[replies], limit as i32)
        .map_err(|e| format!("querying replies: {e}"))?;
    for result in &results {
        thread.push(&txn, ndb, &result.note);
    }

    let mut notes = thread.notes;
    notes.sort_by_key(|n| n.created_at);
    Ok(ThreadOut {
        root: root_hex,
        focus: focus_id.hex(),
        notes: notes.into_iter().map(|n| n.note).collect(),
        missing,
    })
}

/// A thread being gathered: each note once, with its timestamp to sort on.
#[derive(Default)]
struct Thread {
    /// The ids gathered so far.
    ids: Vec<[u8; 32]>,
    /// The notes, unsorted.
    notes: Vec<ThreadNote>,
}

/// A gathered note and the timestamp the thread is sorted on.
struct ThreadNote {
    created_at: u64,
    note: SimpleNote,
}

impl Thread {
    /// Add `note` unless it's already here.
    fn push(&mut self, txn: &Transaction, ndb: &nostrdb::Ndb, note: &Note<'_>) {
        if self.ids.contains(note.id()) {
            return;
        }
        self.ids.push(*note.id());
        self.notes.push(ThreadNote {
            created_at: note.created_at(),
            note: note_to_simple(txn, ndb, note),
        });
    }
}

// ---------------------------------------------------------------------------
// read_timeline
// ---------------------------------------------------------------------------

/// `nd nostr timeline`: the newest notes of the user's follows, one author,
/// or one hashtag, fetched from the relays when the db has none.
struct ReadTimeline {
    /// How long a fetch is waited on.
    bound: Duration,
}

/// [`ReadTimeline`]'s arguments. At most one of `author` and `hashtag`;
/// neither reads the home timeline.
#[derive(Deserialize, Clone)]
struct TimelineArgs {
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    hashtag: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    until: Option<u64>,
    #[serde(default)]
    since: Option<u64>,
    /// Keep replies, which are left out by default.
    #[serde(default)]
    replies: bool,
}

impl TimelineArgs {
    /// Whether these read the home timeline: neither `author` nor
    /// `hashtag`.
    fn is_home(&self) -> bool {
        self.author.is_none() && self.hashtag.is_none()
    }
}

/// What [`ReadTimeline`] answers.
#[derive(Debug, Serialize)]
struct TimelineOut {
    /// Which timeline: `home`, `author` or `hashtag`.
    source: &'static str,
    /// The npub or hashtag it's of.
    of: String,
    /// Newest first.
    notes: Vec<SimpleNote>,
}

/// Which notes a timeline reads, resolved against the db.
struct TimelineSource {
    /// `home`, `author` or `hashtag`, as [`TimelineOut::source`].
    name: &'static str,
    /// The npub or hashtag it's of.
    of: String,
    /// Whose notes, or which hashtag's.
    scope: TimelineScope,
}

/// The notes a [`TimelineSource`] is of.
enum TimelineScope {
    /// These authors' (one, or the account's follows).
    Authors(Vec<[u8; 32]>),
    /// Those tagged with this hashtag, lowercased, without the `#`.
    Hashtag(String),
}

impl TimelineSource {
    /// What `args` read, as `me` sees it: an error for both `author` and
    /// `hashtag`, a malformed author, or a home timeline with no contact
    /// list (or an empty one) in the db.
    fn resolve(
        ndb: &Ndb,
        txn: &Transaction,
        args: &TimelineArgs,
        me: &Pubkey,
    ) -> Result<Self, String> {
        match (args.author.as_deref(), args.hashtag.as_deref()) {
            (Some(_), Some(_)) => Err("give `author` or `hashtag`, not both".to_owned()),
            (Some(author), None) => {
                let author = parse_pubkey(author)?;
                Ok(Self {
                    name: "author",
                    of: author.npub().unwrap_or_else(|| author.hex()),
                    scope: TimelineScope::Authors(vec![*author.bytes()]),
                })
            }
            (None, Some(tag)) => {
                let tag = tag.trim().trim_start_matches('#').to_lowercase();
                Ok(Self {
                    name: "hashtag",
                    of: tag.clone(),
                    scope: TimelineScope::Hashtag(tag),
                })
            }
            (None, None) => {
                let npub = me.npub().unwrap_or_else(|| me.hex());
                let followed = follows(ndb, txn, me).ok_or_else(|| {
                    format!(
                        "no contact list for {npub} in the local db; give `author` or `hashtag`"
                    )
                })?;
                if followed.is_empty() {
                    return Err(format!("{npub} follows no one; give `author` or `hashtag`"));
                }
                Ok(Self {
                    name: "home",
                    of: npub,
                    scope: TimelineScope::Authors(followed),
                })
            }
        }
    }

    /// The filter for `limit` of its notes in `args`' window. `local`
    /// adds the db-only test that drops replies (unless `args` keeps
    /// them); a relay can't run it, so the filter a fetch sends goes
    /// without and the db read drops them after.
    fn filter(&self, args: &TimelineArgs, limit: u64, local: bool) -> Filter {
        let mut filter = Filter::new().kinds([1]).limit(limit);
        filter = match &self.scope {
            TimelineScope::Authors(authors) => filter.authors(authors.iter()),
            TimelineScope::Hashtag(tag) => filter.tags([tag.as_str()], 't'),
        };
        if let Some(until) = args.until {
            filter = filter.until(until);
        }
        if let Some(since) = args.since {
            filter = filter.since(since);
        }
        if local && !args.replies {
            filter = filter.custom(|note| NoteReply::new(note.tags()).reply().is_none());
        }
        filter.build()
    }
}

/// The timeline `args` read as the db holds it now, as `me` sees it.
fn read_timeline(ndb: &Ndb, me: &Pubkey, args: &TimelineArgs) -> Result<TimelineOut, String> {
    let txn = open_txn(ndb)?;
    let source = TimelineSource::resolve(ndb, &txn, args, me)?;
    query_timeline(ndb, &txn, &source, args)
}

/// `source`'s notes in `args`' window, newest first.
fn query_timeline(
    ndb: &Ndb,
    txn: &Transaction,
    source: &TimelineSource,
    args: &TimelineArgs,
) -> Result<TimelineOut, String> {
    let limit = clamp_limit(args.limit, DEFAULT_TIMELINE_LIMIT);
    let mut results = ndb
        .query(txn, &[source.filter(args, limit, true)], limit as i32)
        .map_err(|e| format!("querying the timeline: {e}"))?;
    results.sort_by_key(|r| std::cmp::Reverse(r.note.created_at()));
    Ok(TimelineOut {
        source: source.name,
        of: source.of.clone(),
        notes: results
            .iter()
            .map(|r| note_to_simple(txn, ndb, &r.note))
            .collect(),
    })
}

impl SlowTool for ReadTimeline {
    type Args = TimelineArgs;
    type Output = TimelineOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_read_timeline",
            "Read a nostr timeline of kind-1 notes, newest first: `{source, of, notes}`, each note `{note_id, pubkey, name, content, created_at, note_kind, reply_to}`. With no `author` or `hashtag`, the user's home timeline: the accounts their contact list follows. Replies are left out unless `replies` is true. Page back with `until` set to the oldest note's timestamp. Read from the local nostrdb; when it has no such notes (or no contact list for the home timeline), they are fetched from the user's relays first (a wait of a few seconds).",
            vec![
                ToolArg::new(
                    "author",
                    ToolArgType::String,
                    "Only this author's notes: an npub, an nprofile or a 64-hex pubkey.",
                ),
                ToolArg::new(
                    "hashtag",
                    ToolArgType::String,
                    "Only notes tagged with this hashtag, without the #.",
                ),
                ToolArg::new(
                    "limit",
                    ToolArgType::Integer,
                    "How many notes (default 20, at most 200).",
                ),
                ToolArg::new(
                    "until",
                    ToolArgType::Integer,
                    "Only notes at or before this unix timestamp.",
                ),
                ToolArg::new(
                    "since",
                    ToolArgType::Integer,
                    "Only notes at or after this unix timestamp.",
                ),
                ToolArg::new("replies", ToolArgType::Boolean, "Keep replies too."),
            ],
        )
        .titled("Read nostr timeline")
        .read_only()
        .command(&["nostr", "timeline"])
        .filed_under(TOOL_CATEGORY)
    }

    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<Self::Output>, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let limit = clamp_limit(args.limit, DEFAULT_TIMELINE_LIMIT);
        let txn = open_txn(cx.ndb)?;

        // A home timeline needs the account's contact list first.
        if args.is_home() && follows(cx.ndb, &txn, &me).is_none() {
            drop(txn);
            let contacts = notedeck::contacts::contacts_filter(me.bytes());
            let Some(refetch) = refetch(cx, vec![contacts], 1, self.bound) else {
                return read_timeline(cx.ndb, &me, &args).map(answered);
            };
            return Ok(reread(cx.ndb, refetch, move |ndb| {
                read_timeline(ndb, &me, &args)
            }));
        }

        let source = TimelineSource::resolve(cx.ndb, &txn, &args, &me)?;
        let out = query_timeline(cx.ndb, &txn, &source, &args)?;
        if !out.notes.is_empty() {
            return Ok(answered(out));
        }
        drop(txn);
        let wanted = source.filter(&args, limit, false);
        let Some(refetch) = refetch(cx, vec![wanted], limit as u32, self.bound) else {
            return Ok(answered(out));
        };
        Ok(reread(cx.ndb, refetch, move |ndb| {
            read_timeline(ndb, &me, &args)
        }))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_notes(&out.notes))
    }
}

#[path = "nostr_tools/act.rs"]
pub mod act;
#[path = "nostr_tools/open.rs"]
pub mod open;

#[cfg(test)]
#[path = "nostr_tools/tests.rs"]
mod tests;
