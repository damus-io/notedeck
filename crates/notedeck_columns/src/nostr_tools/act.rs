//! The nostr action tools — post, reply, react, follow, unfollow, zap — so
//! `nd nostr post|reply|react|follow|unfollow|zap` acts as the user, and an
//! agent spec with a `Notedeck { apps: ["nostr"] }` toolset can too, once
//! the user says yes.
//!
//! ```text
//! nd nostr post "gm"     ─┐                          ┌─ sign as the selected account
//! native-loop tool call ──┴─ ActAsUser: Gated ───────┤  ingest locally (shows at once)
//!   (Dave asks first)                                └─ publish to its write relays
//!
//! nostr_zap ─ ActAsUser: Gated ─ UI tool ─ AppAction::Note(Zap) ─ Columns' zap path
//! ```
//!
//! Every one acts *as the user*: it signs with the selected account's key
//! and puts a note on their relays that others will read as theirs. So each
//! declares [`Capability::ActAsUser`], which the approval table gates under
//! every containment and Plan refuses: a native-loop agent asks before each
//! call, whatever the session's permission mode. A watch-only account
//! can't sign, and each says so rather than failing quietly.
//!
//! The writes are [data tools](notedeck::ToolKind::Data): a [`ToolContext`]
//! has the accounts and a publisher, so the model hears what was sent (the
//! new note's id and how many relays it went to), not just that something
//! went somewhere. A zap is a [UI tool](notedeck::ToolKind::Ui) instead: it
//! needs the wallet and Columns' zap state, which only the click's action
//! path holds, so it answers with the [`NoteAction::Zap`] a zap button
//! raises and the model hears `sent_to_window`.

use nostrdb::{IngestMetadata, Note, NoteBuilder, Transaction};
use nostrdb_net::{ClientMessage, NoteId, Pubkey};
use notedeck::{
    note::ZapTargetAmount, tool::Capability, AppAction, AppContext, AppTool, NoteAction,
    NoteZapTargetOwned, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolOutcome, ToolSpec,
    ZapAction,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{follows, open_txn, parse_note_id, parse_pubkey, TOOL_CATEGORY};
use crate::post::NewPost;

/// Zap a note through the user's wallet.
pub const ZAP: &str = "nostr_zap";

/// The longest reaction [`React`] sends: a `+`, a `-` or one emoji (some
/// are several code points), never a sentence.
const MAX_REACTION_CHARS: usize = 8;

/// The sentence every action tool's description ends with, so a model
/// knows the user is asked first and may say no.
macro_rules! asks_first {
    () => {
        " The user is asked to approve each call first and may refuse it."
    };
}

/// Every nostr action that writes a note, ready to register beside the
/// reads.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(Post),
        RegisteredTool::new(Reply),
        RegisteredTool::new(React),
        RegisteredTool::new(Follow),
        RegisteredTool::new(Unfollow),
    ]
}

/// What a write answers: the note it signed and where it went.
#[derive(Debug, Serialize)]
pub struct Sent {
    /// The new note's id, hex.
    pub id: String,
    /// The new note's id, `note1…`.
    pub note: String,
    /// Its kind: 1 for a post or reply, 7 for a reaction, 3 for a contact
    /// list.
    pub kind: u32,
    /// How many of the account's write relays it was sent to. 0 means it
    /// exists only in this notedeck until the account has a write relay.
    pub relays: usize,
}

/// Rows for a person, from a [`Sent`].
fn render_sent(verb: &str, sent: &Sent) -> String {
    format!(
        "{verb} {}\n{} relay{}\n",
        sent.note,
        sent.relays,
        if sent.relays == 1 { "" } else { "s" }
    )
}

/// The selected account's secret key, or the reason it can't sign.
fn signing_secret(cx: &ToolContext) -> Result<[u8; 32], String> {
    cx.accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes())
        .ok_or_else(|| "the selected account is watch-only and can't sign".to_owned())
}

/// Ingest the signed `note` locally, so it shows in the columns at once,
/// then publish it to the selected account's write relays: the path the
/// post button takes, through `cx`'s publisher. Its id goes in
/// [`written`](ToolContext::written), so `nd` answers once it reads back.
fn send(cx: &mut ToolContext, note: &Note) -> Result<Sent, String> {
    let json = ClientMessage::event(note)
        .and_then(|event| event.to_json())
        .map_err(|e| format!("serializing the note: {e}"))?;
    let relays = cx.accounts.selected_account_write_relays();
    let count = relays.len();
    let publish = cx
        .publish
        .as_mut()
        .ok_or("publishing is unavailable in this context")?;
    cx.ndb
        .process_event_with(&json, IngestMetadata::new().client(true))
        .map_err(|e| format!("storing the note: {e}"))?;
    publish.publish_note(note, relays);

    let id = NoteId::new(*note.id());
    cx.written.push(id);
    Ok(Sent {
        id: id.hex(),
        note: id.to_bech().unwrap_or_default(),
        kind: note.kind(),
        relays: count,
    })
}

/// `content` trimmed, or an error naming the tool when nothing is left.
fn non_empty<'a>(tool: &str, content: &'a str) -> Result<&'a str, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err(format!("'{tool}' needs some content to send"));
    }
    Ok(content)
}

// ---------------------------------------------------------------------------
// post / reply
// ---------------------------------------------------------------------------

/// `nd nostr post <content>`: a kind-1 note from the user.
struct Post;

/// [`Post`]'s arguments.
#[derive(Deserialize)]
struct PostArgs {
    /// The note's text. `#hashtags` in it become `t` tags.
    content: String,
}

impl AppTool for Post {
    type Args = PostArgs;
    type Output = Sent;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_post",
            concat!(
                "Publish a new public nostr note (kind 1) as the user, to their write relays: \
                 `{id, note, kind, relays}`. Everyone who follows them will see it. \
                 `#hashtags` in the text are tagged. To answer someone, use nostr_reply.",
                asks_first!()
            ),
            vec![ToolArg::new(
                "content",
                ToolArgType::String,
                "The note's text, exactly as it should appear.",
            )
            .required(true)
            .positional()],
        )
        .titled("Post nostr note")
        .capability(Capability::ActAsUser)
        .command(&["nostr", "post"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let content = non_empty("nostr_post", &args.content)?.to_owned();
        let account = cx
            .accounts
            .selected_filled()
            .ok_or("the selected account is watch-only and can't sign")?
            .to_full();
        let secret = account.secret_key.secret_bytes();
        let post = NewPost::new(content, account, Vec::new(), Vec::new());
        let note = post.to_note(&secret);
        send(cx, &note)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_sent("posted", out))
    }
}

/// `nd nostr reply <note> <content>`: a kind-1 reply, threaded by NIP-10.
struct Reply;

/// [`Reply`]'s arguments.
#[derive(Deserialize)]
struct ReplyArgs {
    /// The note answered: note1, nevent or hex.
    note: String,
    /// The reply's text.
    content: String,
}

impl AppTool for Reply {
    type Args = ReplyArgs;
    type Output = Sent;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_reply",
            concat!(
                "Publish a public reply to a nostr note as the user, threaded under it (NIP-10 \
                 root and reply tags, the author and everyone they tagged): \
                 `{id, note, kind, relays}`. The note must be in the local db; read it with \
                 nostr_load_thread first.",
                asks_first!()
            ),
            vec![
                ToolArg::new(
                    "note",
                    ToolArgType::String,
                    "The note to reply to: a note1, an nevent or a 64-hex id (a `nostr:` prefix is fine).",
                )
                .required(true)
                .positional(),
                ToolArg::new("content", ToolArgType::String, "The reply's text.")
                    .required(true)
                    .positional(),
            ],
        )
        .titled("Reply to nostr note")
        .capability(Capability::ActAsUser)
        .command(&["nostr", "reply"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let id = parse_note_id(&args.note)?;
        let content = non_empty("nostr_reply", &args.content)?.to_owned();
        let account = cx
            .accounts
            .selected_filled()
            .ok_or("the selected account is watch-only and can't sign")?
            .to_full();
        let secret = account.secret_key.secret_bytes();
        let txn = open_txn(cx)?;
        let parent = known_note(cx, &txn, &id)?;
        let post = NewPost::new(content, account, Vec::new(), Vec::new());
        let note = post.to_reply(&secret, &parent);
        send(cx, &note)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_sent("replied", out))
    }
}

/// The note `id` from the db, or an error saying the agent has to see it
/// first: a reply or reaction tags the note's author and thread, which
/// only the note itself names.
fn known_note<'t>(cx: &ToolContext, txn: &'t Transaction, id: &NoteId) -> Result<Note<'t>, String> {
    cx.ndb.get_note_by_id(txn, id.bytes()).map_err(|_| {
        format!(
            "note {} isn't in this notedeck's db yet; open its thread so it's fetched, then try again",
            id.to_bech().unwrap_or_else(|| id.hex())
        )
    })
}

// ---------------------------------------------------------------------------
// react
// ---------------------------------------------------------------------------

/// `nd nostr react <note> [reaction]`: a NIP-25 reaction, a like by default.
struct React;

/// [`React`]'s arguments.
#[derive(Deserialize)]
struct ReactArgs {
    /// The note reacted to.
    note: String,
    /// `+` (like, the default), `-` or one emoji.
    #[serde(default)]
    reaction: Option<String>,
}

impl AppTool for React {
    type Args = ReactArgs;
    type Output = Sent;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_react",
            concat!(
                "React to a nostr note as the user (NIP-25, kind 7), as the like button does: \
                 `{id, note, kind, relays}`. The note must be in the local db.",
                asks_first!()
            ),
            vec![
                ToolArg::new(
                    "note",
                    ToolArgType::String,
                    "The note to react to: a note1, an nevent or a 64-hex id (a `nostr:` prefix is fine).",
                )
                .required(true)
                .positional(),
                ToolArg::new(
                    "reaction",
                    ToolArgType::String,
                    "`+` for a like (the default), `-` for a dislike, or a single emoji.",
                )
                .positional(),
            ],
        )
        .titled("React to nostr note")
        .capability(Capability::ActAsUser)
        .command(&["nostr", "react"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let id = parse_note_id(&args.note)?;
        let reaction = reaction_content(args.reaction.as_deref())?;
        let secret = signing_secret(cx)?;
        let txn = open_txn(cx)?;
        let target = known_note(cx, &txn, &id)?;
        let note = crate::actionbar::reaction_note(&txn, &target, &secret, reaction)?;
        send(cx, &note)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_sent("reacted", out))
    }
}

/// A reaction's content: `+` when none is given, else the one given if it
/// is a single short token (`+`, `-`, an emoji), never a sentence.
fn reaction_content(reaction: Option<&str>) -> Result<&str, String> {
    let reaction = reaction.map(str::trim).unwrap_or("+");
    let reaction = if reaction.is_empty() { "+" } else { reaction };
    if reaction.chars().count() > MAX_REACTION_CHARS || reaction.contains(char::is_whitespace) {
        return Err(format!(
            "{reaction:?} is not a reaction: give `+`, `-` or one emoji (reply with nostr_reply to say more)"
        ));
    }
    Ok(reaction)
}

// ---------------------------------------------------------------------------
// follow / unfollow
// ---------------------------------------------------------------------------

/// `nd nostr follow <who>`: add a key to the user's contact list.
struct Follow;

/// `nd nostr unfollow <who>`: drop a key from the user's contact list.
struct Unfollow;

/// [`Follow`]'s and [`Unfollow`]'s arguments.
#[derive(Deserialize)]
struct WhoArgs {
    /// Whom: npub, nprofile or hex.
    who: String,
}

/// What a follow or unfollow answers.
#[derive(Debug, Serialize)]
pub struct FollowOut {
    /// The key followed or unfollowed, hex.
    pub pubkey: String,
    /// The same key, `npub1…`.
    pub npub: String,
    /// Whether the user follows them now.
    pub following: bool,
    /// The new contact list, or `None` when nothing changed (they were
    /// already followed, or already not) and nothing was sent.
    pub sent: Option<Sent>,
}

/// The `who` argument of [`Follow`] and [`Unfollow`].
fn who_arg(description: &'static str) -> Vec<ToolArg> {
    vec![ToolArg::new("who", ToolArgType::String, description)
        .required(true)
        .positional()]
}

impl AppTool for Follow {
    type Args = WhoArgs;
    type Output = FollowOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_follow",
            concat!(
                "Follow a nostr user as the user: publish their contact list (kind 3) with this \
                 key added, every other entry kept: `{pubkey, npub, following, sent}`. `sent` is \
                 null when they were already followed. Refuses when this notedeck doesn't have \
                 the user's contact list yet, since publishing one would drop their other follows.",
                asks_first!()
            ),
            who_arg("Whom to follow: an npub, an nprofile or a 64-hex pubkey (a `nostr:` prefix is fine)."),
        )
        .titled("Follow nostr user")
        .capability(Capability::ActAsUser)
        .command(&["nostr", "follow"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        set_following(cx, &parse_pubkey(&args.who)?, true)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_follow(out))
    }
}

impl AppTool for Unfollow {
    type Args = WhoArgs;
    type Output = FollowOut;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "nostr_unfollow",
            concat!(
                "Unfollow a nostr user as the user: publish their contact list (kind 3) with this \
                 key removed, every other entry kept: `{pubkey, npub, following, sent}`. `sent` \
                 is null when they weren't followed.",
                asks_first!()
            ),
            who_arg("Whom to unfollow: an npub, an nprofile or a 64-hex pubkey (a `nostr:` prefix is fine)."),
        )
        .titled("Unfollow nostr user")
        .capability(Capability::ActAsUser)
        .command(&["nostr", "unfollow"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        set_following(cx, &parse_pubkey(&args.who)?, false)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_follow(out))
    }
}

/// Rows for a person, from a [`FollowOut`].
fn render_follow(out: &FollowOut) -> String {
    let state = if out.following {
        "following"
    } else {
        "not following"
    };
    match &out.sent {
        Some(sent) => format!("{state} {}\n{}", out.npub, render_sent("sent", sent)),
        None => format!("already {state} {}\n", out.npub),
    }
}

/// Make the user follow `target` (or not), by publishing their newest
/// contact list with `target`'s `p` tag added or removed and every other
/// tag kept, as the profile's follow button does. Nothing is sent when it
/// is already so. With no contact list in the db it refuses: a new one
/// holding only `target` would replace the follows on their relays.
fn set_following(cx: &mut ToolContext, target: &Pubkey, follow: bool) -> Result<FollowOut, String> {
    let secret = signing_secret(cx)?;
    let me = *cx.accounts.selected_account_pubkey();
    let txn = open_txn(cx)?;
    let following = follows(cx.ndb, &txn, &me)
        .ok_or("this notedeck doesn't have your contact list yet, so it can't change it without dropping your other follows; try again once it has synced")?
        .contains(target.bytes());
    let out = |sent| FollowOut {
        pubkey: target.hex(),
        npub: target.npub().unwrap_or_default(),
        following: follow,
        sent,
    };
    if following == follow {
        return Ok(out(None));
    }

    let filter = notedeck::contacts::contacts_filter(me.bytes());
    let contacts = cx
        .ndb
        .query(&txn, &[filter], 1)
        .ok()
        .and_then(|r| r.into_iter().next())
        .ok_or("your contact list vanished from the db")?
        .note;
    let note = contact_list_note(&contacts, target, follow, &secret)?;
    let sent = send(cx, &note)?;
    Ok(out(Some(sent)))
}

/// `contacts` re-signed with `secret`, `target`'s `p` tag appended when
/// `follow`, dropped when not; every other tag and the content (some
/// clients keep relays there) as they were.
fn contact_list_note<'a>(
    contacts: &'a Note<'_>,
    target: &Pubkey,
    follow: bool,
    secret: &[u8; 32],
) -> Result<Note<'a>, String> {
    let is_target = |tag: &nostrdb::Tag<'_>| {
        tag.count() >= 2 && tag.get_str(0) == Some("p") && tag.get_id(1) == Some(target.bytes())
    };
    let mut builder = NoteBuilder::new().kind(3).content(contacts.content());
    for tag in contacts.tags() {
        if !follow && is_target(&tag) {
            continue;
        }
        builder = builder.start_tag();
        for item in tag {
            builder = match item.variant() {
                nostrdb::NdbStrVariant::Id(id) => builder.tag_id(id),
                nostrdb::NdbStrVariant::Str(s) => builder.tag_str(s),
            };
        }
    }
    if follow {
        builder = builder.start_tag().tag_str("p").tag_id(target.bytes());
    }
    builder
        .sign(secret)
        .build()
        .ok_or_else(|| "failed to build the contact list".to_owned())
}

// ---------------------------------------------------------------------------
// zap (a UI tool)
// ---------------------------------------------------------------------------

/// The UI action tools' specs, for [`App::ui_tools`](notedeck::App::ui_tools)
/// beside the opens.
pub fn ui_specs() -> Vec<ToolSpec> {
    vec![ToolSpec::new(
        ZAP,
        concat!(
            "Zap a nostr note's author from the user's wallet, as the zap button does: \
             sats leave the user's wallet. With no `sats`, the wallet's default zap amount. \
             The note must be in the local db. Answers {\"sent_to_window\": true} once the zap \
             is on its way; it doesn't say whether the wallet paid. Needs a window and a \
             connected wallet.",
            asks_first!()
        ),
        vec![
            ToolArg::new(
                "note",
                ToolArgType::String,
                "The note to zap: a note1, an nevent or a 64-hex id (a `nostr:` prefix is fine).",
            )
            .required(true)
            .positional(),
            ToolArg::new(
                "sats",
                ToolArgType::Integer,
                "How many sats. Defaults to the wallet's default zap amount.",
            )
            .positional(),
        ],
    )
    .titled("Zap nostr note")
    .capability(Capability::ActAsUser)
    .command(&["nostr", "zap"])
    .ui()
    .filed_under(TOOL_CATEGORY)]
}

/// Run the UI action tool `name` with `args`: the click's
/// [`NoteAction::Zap`] as an intent for the chrome to route to Columns, or
/// `None` when `name` isn't one.
pub fn call_ui(ctx: &AppContext<'_>, name: &str, args: &Value) -> Option<ToolOutcome> {
    if name != ZAP {
        return None;
    }
    Some(match zap(ctx, args) {
        Ok(action) => ToolOutcome::Intent(AppAction::Note(action)),
        Err(err) => ToolOutcome::Error(err),
    })
}

/// [`ZAP`]: send `sats` (or the wallet's default) to `note`'s author.
fn zap(ctx: &AppContext<'_>, args: &Value) -> Result<NoteAction, String> {
    let note = args
        .get("note")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{ZAP}' needs a \"note\" string"))?;
    let note_id = parse_note_id(note)?;
    let specified_msats = zap_msats(args.get("sats"))?;
    let txn = Transaction::new(ctx.ndb).map_err(|e| format!("opening a db transaction: {e}"))?;
    let recipient = ctx
        .ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|n| Pubkey::new(*n.pubkey()))
        .map_err(|_| {
            format!("note {note} isn't in this notedeck's db yet, so its author isn't known")
        })?;
    Ok(NoteAction::Zap(ZapAction::Send(ZapTargetAmount {
        target: NoteZapTargetOwned {
            note_id,
            zap_recipient: recipient,
        },
        specified_msats,
    })))
}

/// The millisats of a zap's `sats` argument: `None` (the wallet's default)
/// when absent, an error when it isn't a whole number of sats above zero.
fn zap_msats(sats: Option<&Value>) -> Result<Option<u64>, String> {
    let Some(sats) = sats.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let n = sats
        .as_u64()
        .or_else(|| sats.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("{sats} is not a number of sats: give a whole number above 0"))?;
    n.checked_mul(1000)
        .map(Some)
        .ok_or_else(|| format!("{n} sats is too many"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A reaction is a like unless one short token is given.
    #[test]
    fn reaction_content_is_one_short_token() {
        assert_eq!(reaction_content(None).unwrap(), "+");
        assert_eq!(reaction_content(Some(" ")).unwrap(), "+");
        assert_eq!(reaction_content(Some("-")).unwrap(), "-");
        assert_eq!(reaction_content(Some("🤙")).unwrap(), "🤙");
        assert!(reaction_content(Some("great post")).is_err());
        assert!(reaction_content(Some("aaaaaaaaaaaa")).is_err());
    }

    /// Sats become millisats; none is the wallet's default; zero, negative
    /// and junk are refused.
    #[test]
    fn zap_sats_are_whole_and_positive() {
        assert_eq!(zap_msats(None).unwrap(), None);
        assert_eq!(zap_msats(Some(&Value::Null)).unwrap(), None);
        assert_eq!(zap_msats(Some(&json!(21))).unwrap(), Some(21_000));
        assert_eq!(zap_msats(Some(&json!("21"))).unwrap(), Some(21_000));
        for bad in [json!(0), json!(-5), json!(1.5), json!("lots")] {
            assert!(zap_msats(Some(&bad)).is_err(), "{bad}");
        }
    }

    /// The zap is a UI tool under `nd nostr` that declares ActAsUser, so
    /// the catalog gates it rather than taking the UI default (Navigate).
    #[test]
    fn the_zap_is_a_gated_ui_tool() {
        let specs = ui_specs();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.name(), ZAP);
        assert_eq!(spec.kind(), notedeck::ToolKind::Ui);
        assert_eq!(spec.command_path(), ["nostr", "zap"]);
        assert_eq!(spec.capability_class(), Capability::ActAsUser);
    }
}
