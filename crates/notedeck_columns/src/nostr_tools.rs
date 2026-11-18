//! Read-only nostr data tools — a profile, a thread, a timeline — so `nd
//! nostr profile|thread|timeline` reads what this notedeck has seen, and an
//! agent spec with a `Notedeck { apps: ["nostr"] }` toolset can look around
//! the user's nostr world from the native loop.
//!
//! ```text
//! nd nostr profile  [who]            ─┐
//! nd nostr thread   <note>            ├─ ToolContext.ndb ── local nostrdb
//! nd nostr timeline [--author|--hashtag] ┘   (no relay round trip)
//! ```
//!
//! Every tool answers from the local nostrdb only: what the columns, Dave
//! or any other app has already pulled from relays. A profile or note this
//! notedeck never saw answers as such (`found: false`, a `missing` id) rather
//! than as a fetch, since a [`ToolContext`] holds no relay pool. Each is
//! [`read_only`](ToolSpec::read_only), so the capability table makes it
//! `Observe`: auto-approved in every session mode, Plan included.
//!
//! Notes are answered as the engine's [`SimpleNote`], the shape Dave's
//! `query` tool and thread summaries already hand a model.
//!
//! Showing the user one of these in the window, rather than reading it, is
//! [`open`]'s job: `nd nostr open-profile|open-thread|open-hashtag`.

use nostrdb::{Filter, Note, NoteReply, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::{
    note_to_simple, AppTool, RegisteredTool, SimpleNote, ToolArg, ToolArgType, ToolContext,
    ToolSpec,
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

/// Every nostr data tool, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(GetProfile),
        RegisteredTool::new(LoadThread),
        RegisteredTool::new(ReadTimeline),
    ]
}

/// A read transaction, its error worded for the caller.
fn open_txn(cx: &ToolContext) -> Result<Transaction, String> {
    Transaction::new(cx.ndb).map_err(|e| format!("opening a db transaction: {e}"))
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

/// `nd nostr profile [who]`: one person's kind-0 profile.
struct GetProfile;

/// [`GetProfile`]'s arguments.
#[derive(Deserialize)]
struct ProfileArgs {
    /// Whose profile; the selected account's when absent.
    #[serde(default)]
    who: Option<String>,
}

/// What [`GetProfile`] answers. Every profile field is `None` when this
/// notedeck has no kind 0 for the key (`found: false`).
#[derive(Debug, Serialize)]
struct ProfileOut {
    /// The key, hex.
    pubkey: String,
    /// The key, `npub1…`.
    npub: String,
    /// Whether this notedeck holds a kind-0 profile for the key.
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

impl AppTool for GetProfile {
    type Args = ProfileArgs;
    type Output = ProfileOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_get_profile",
            "Read a nostr user's profile (kind 0) from the local nostrdb: `{pubkey, npub, found, name, display_name, about, picture, banner, website, nip05, lud16, following, followed_by_you, is_you}`. `found: false` means this notedeck has never seen their profile, not that the key is wrong. With no `who`, the user's own profile.",
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

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let pubkey = match args.who.as_deref() {
            Some(who) => parse_pubkey(who)?,
            None => me,
        };
        let txn = open_txn(cx)?;
        let record = cx.ndb.get_profile_by_pubkey(&txn, pubkey.bytes()).ok();
        let profile = record.as_ref().and_then(|r| r.record().profile());
        let field = |value: Option<&str>| value.map(str::to_owned);
        let followed_by_you = follows(cx.ndb, &txn, &me).map(|f| f.contains(pubkey.bytes()));
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
            following: follows(cx.ndb, &txn, &pubkey).map(|f| f.len()),
            followed_by_you,
            is_you: pubkey == me,
        })
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

// ---------------------------------------------------------------------------
// load_thread
// ---------------------------------------------------------------------------

/// `nd nostr thread <note>`: the conversation a note is part of.
struct LoadThread;

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
    /// that this notedeck doesn't have.
    missing: Vec<String>,
}

impl AppTool for LoadThread {
    type Args = ThreadArgs;
    type Output = ThreadOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_load_thread",
            "Load the nostr thread a note belongs to from the local nostrdb: `{root, focus, notes, missing}`. `notes` holds the root, the note's ancestors, the note and the replies under the root, oldest first, each `{note_id, pubkey, name, content, created_at, note_kind, reply_to}`; `reply_to` is the parent's id, so the tree can be rebuilt. `missing` lists referenced notes this notedeck hasn't seen.",
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

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let focus_id = parse_note_id(&args.note)?;
        let limit = clamp_limit(args.limit, DEFAULT_THREAD_LIMIT);
        let txn = open_txn(cx)?;
        let focus = cx
            .ndb
            .get_note_by_id(&txn, focus_id.bytes())
            .map_err(|_| format!("note {} isn't in the local db", focus_id.hex()))?;

        let mut thread = Thread::default();
        let mut missing = Vec::new();
        thread.push(&txn, cx.ndb, &focus);

        // Walk the parents up: a reply's root may tag only the root, so the
        // chain between it and the asked-for note comes from here.
        let mut parent = NoteReply::new(focus.tags()).reply().map(|r| *r.id);
        while let Some(id) = parent.take() {
            if thread.ids.len() > MAX_ANCESTORS || thread.ids.contains(&id) {
                break;
            }
            let Ok(note) = cx.ndb.get_note_by_id(&txn, &id) else {
                missing.push(hex::encode(id));
                break;
            };
            thread.push(&txn, cx.ndb, &note);
            parent = NoteReply::new(note.tags()).reply().map(|r| *r.id);
        }

        let root = NoteReply::new(focus.tags())
            .root()
            .map(|r| *r.id)
            .unwrap_or(*focus_id.bytes());
        // The root may be the parent the walk already found missing.
        let root_hex = hex::encode(root);
        if !thread.ids.contains(&root) && !missing.contains(&root_hex) {
            match cx.ndb.get_note_by_id(&txn, &root) {
                Ok(note) => thread.push(&txn, cx.ndb, &note),
                Err(_) => missing.push(root_hex.clone()),
            }
        }

        let replies = Filter::new().kinds([1]).event(&root).build();
        let results = cx
            .ndb
            .query(&txn, &[replies], limit as i32)
            .map_err(|e| format!("querying replies: {e}"))?;
        for result in &results {
            thread.push(&txn, cx.ndb, &result.note);
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

    fn render(&self, out: &Self::Output) -> Option<String> {
        let mut text = render_notes(&out.notes);
        for id in &out.missing {
            text.push_str(&format!("(missing {id})\n"));
        }
        Some(text)
    }
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
/// or one hashtag.
struct ReadTimeline;

/// [`ReadTimeline`]'s arguments. At most one of `author` and `hashtag`;
/// neither reads the home timeline.
#[derive(Deserialize)]
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

impl AppTool for ReadTimeline {
    type Args = TimelineArgs;
    type Output = TimelineOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_read_timeline",
            "Read a nostr timeline of kind-1 notes from the local nostrdb, newest first: `{source, of, notes}`, each note `{note_id, pubkey, name, content, created_at, note_kind, reply_to}`. With no `author` or `hashtag`, the user's home timeline: the accounts their contact list follows. Replies are left out unless `replies` is true. Page back with `until` set to the oldest note's timestamp.",
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

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let limit = clamp_limit(args.limit, DEFAULT_TIMELINE_LIMIT);
        let txn = open_txn(cx)?;
        let mut filter = Filter::new().kinds([1]).limit(limit);
        let (source, of) = match (args.author.as_deref(), args.hashtag.as_deref()) {
            (Some(_), Some(_)) => {
                return Err("give `author` or `hashtag`, not both".to_owned());
            }
            (Some(author), None) => {
                let author = parse_pubkey(author)?;
                filter = filter.authors([author.bytes()]);
                ("author", author.npub().unwrap_or_else(|| author.hex()))
            }
            (None, Some(tag)) => {
                let tag = tag.trim().trim_start_matches('#').to_lowercase();
                filter = filter.tags([tag.as_str()], 't');
                ("hashtag", tag)
            }
            (None, None) => {
                let me = *cx.accounts.selected_account_pubkey();
                let npub = me.npub().unwrap_or_else(|| me.hex());
                let followed = follows(cx.ndb, &txn, &me).ok_or_else(|| {
                    format!(
                        "no contact list for {npub} in the local db; give `author` or `hashtag`"
                    )
                })?;
                if followed.is_empty() {
                    return Err(format!("{npub} follows no one; give `author` or `hashtag`"));
                }
                filter = filter.authors(followed.iter());
                ("home", npub)
            }
        };
        if let Some(until) = args.until {
            filter = filter.until(until);
        }
        if let Some(since) = args.since {
            filter = filter.since(since);
        }
        if !args.replies {
            filter = filter.custom(|note| NoteReply::new(note.tags()).reply().is_none());
        }
        let mut results = cx
            .ndb
            .query(&txn, &[filter.build()], limit as i32)
            .map_err(|e| format!("querying the timeline: {e}"))?;
        results.sort_by_key(|r| std::cmp::Reverse(r.note.created_at()));
        Ok(TimelineOut {
            source,
            of,
            notes: results
                .iter()
                .map(|r| note_to_simple(&txn, cx.ndb, &r.note))
                .collect(),
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_notes(&out.notes))
    }
}

#[path = "nostr_tools/open.rs"]
pub mod open;

#[cfg(test)]
#[path = "nostr_tools/tests.rs"]
mod tests;
