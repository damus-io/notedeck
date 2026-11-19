//! The nostr navigation tools — open a profile, a thread, a hashtag's
//! timeline — so `nd nostr open-profile|open-thread|open-hashtag` shows the
//! user something in the window, and an agent spec with a
//! `Notedeck { apps: ["nostr"] }` toolset can too.
//!
//! ```text
//! nd nostr open-thread <note> ─┐                       ┌─ Columns::call_ui_tool
//! native-loop tool call ───────┴─ registry: CallTool ──┤    parse → NoteAction
//!                                                      └─ chrome: AppAction::Note
//!                                                           → switch to Columns,
//!                                                             push the route
//! ```
//!
//! Each is a [UI tool](notedeck::ToolKind::Ui) with a command line, so the
//! agent catalog offers it as `Navigate`: auto-approved, Plan included. It
//! answers with the [`NoteAction`] a click on the same profile, note or
//! hashtag raises, so the open takes the click's path through the chrome
//! (an app that owns the note opens it, else Columns' selected column
//! routes). A bad argument is an error here; whether the note or profile is
//! in the db isn't checked, since Columns fetches what it lacks once the
//! route is up.

use notedeck::{AppAction, AppContext, NoteAction, ToolArg, ToolArgType, ToolOutcome, ToolSpec};
use serde_json::Value;

use super::{parse_note_id, parse_pubkey, TOOL_CATEGORY};

/// Open a nostr profile in Columns.
pub const OPEN_PROFILE: &str = "nostr_open_profile";
/// Open a note's thread in Columns.
pub const OPEN_THREAD: &str = "nostr_open_thread";
/// Open a hashtag's timeline in Columns.
pub const OPEN_HASHTAG: &str = "nostr_open_hashtag";

/// The sentence every open tool's description ends with: a caller hears
/// that the window opened the route, or the tool's error, never what the
/// route found to show.
macro_rules! answer_note {
    () => {
        " Answers {\"applied\": true} once the window has opened it, or an error \
         (a malformed id, or no window); it doesn't say whether the window had \
         anything to show."
    };
}

/// The open tools' specs, for [`App::ui_tools`](notedeck::App::ui_tools).
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            OPEN_PROFILE,
            concat!(
                "Show the user a nostr profile in the Columns app, as a click on the profile \
                 would. Use nostr_get_profile to read one without moving the user's view.",
                answer_note!()
            ),
            vec![ToolArg::new(
                "who",
                ToolArgType::String,
                "Whose profile: an npub, an nprofile or a 64-hex pubkey (a `nostr:` prefix is fine). Defaults to the user's own account.",
            )
            .positional()],
        )
        .titled("Open nostr profile")
        .command(&["nostr", "open-profile"]),
        ToolSpec::new(
            OPEN_THREAD,
            concat!(
                "Show the user a note's thread in the Columns app, as a click on the note \
                 would. Use nostr_load_thread to read one without moving the user's view.",
                answer_note!()
            ),
            vec![ToolArg::new(
                "note",
                ToolArgType::String,
                "The note: a note1, an nevent or a 64-hex id (a `nostr:` prefix is fine).",
            )
            .required(true)
            .positional()],
        )
        .titled("Open nostr thread")
        .command(&["nostr", "open-thread"]),
        ToolSpec::new(
            OPEN_HASHTAG,
            concat!(
                "Show the user a hashtag's timeline in the Columns app, as a click on the \
                 hashtag would.",
                answer_note!()
            ),
            vec![ToolArg::new(
                "hashtag",
                ToolArgType::String,
                "The hashtag, with or without its `#`.",
            )
            .required(true)
            .positional()],
        )
        .titled("Open nostr hashtag")
        .command(&["nostr", "open-hashtag"]),
    ]
    .into_iter()
    .map(|spec| spec.ui().filed_under(TOOL_CATEGORY))
    .collect()
}

/// Run the open tool `name` with `args`: the click's [`NoteAction`] as an
/// intent for the chrome to route, or `None` when `name` isn't one.
pub fn call(ctx: &AppContext<'_>, name: &str, args: &Value) -> Option<ToolOutcome> {
    let action = match name {
        OPEN_PROFILE => open_profile(ctx, args),
        OPEN_THREAD => open_thread(args),
        OPEN_HASHTAG => open_hashtag(args),
        _ => return None,
    };
    Some(match action {
        Ok(action) => ToolOutcome::Intent(AppAction::Note(action)),
        Err(err) => ToolOutcome::Error(err),
    })
}

/// [`OPEN_PROFILE`]: `who`'s profile, or the account's own.
fn open_profile(ctx: &AppContext<'_>, args: &Value) -> Result<NoteAction, String> {
    let pubkey = match args.get("who").and_then(Value::as_str) {
        Some(who) => parse_pubkey(who)?,
        None => *ctx.accounts.selected_account_pubkey(),
    };
    Ok(NoteAction::Profile(pubkey))
}

/// [`OPEN_THREAD`]: the thread `note` is in.
fn open_thread(args: &Value) -> Result<NoteAction, String> {
    let note = required_str(args, OPEN_THREAD, "note")?;
    Ok(NoteAction::note(parse_note_id(note)?))
}

/// [`OPEN_HASHTAG`]: one hashtag's timeline, its `#` dropped.
fn open_hashtag(args: &Value) -> Result<NoteAction, String> {
    let raw = required_str(args, OPEN_HASHTAG, "hashtag")?.trim();
    let tag = raw.strip_prefix('#').unwrap_or(raw);
    if tag.is_empty() || tag.contains(|c: char| c.is_whitespace() || c == '#') {
        return Err(format!("{raw:?} is not a hashtag: give one word"));
    }
    Ok(NoteAction::Hashtag(tag.to_owned()))
}

/// The string argument `arg` of a `tool` call, or an error naming it.
fn required_str<'a>(args: &'a Value, tool: &str, arg: &str) -> Result<&'a str, String> {
    args.get(arg)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{tool}' needs a {arg:?} string"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::tool::Capability;
    use notedeck::ToolKind;
    use serde_json::json;

    /// Each is a UI tool under `nd nostr`, so the catalog offers it to a
    /// nostr toolset as Navigate (auto, allowed in Plan).
    #[test]
    fn the_open_tools_are_navigate_ui_tools_under_nd_nostr() {
        let specs = specs();
        let names: Vec<_> = specs.iter().map(ToolSpec::name).collect();
        assert_eq!(names, [OPEN_PROFILE, OPEN_THREAD, OPEN_HASHTAG]);
        for spec in &specs {
            assert_eq!(spec.kind(), ToolKind::Ui, "{}", spec.name());
            assert_eq!(spec.command_path().first(), Some(&"nostr"));
            assert_eq!(spec.capability_class(), Capability::Navigate);
            assert!(spec.description().contains("\"applied\": true"));
        }
    }

    /// A note id in any of its spellings opens its thread.
    #[test]
    fn open_thread_takes_any_note_id_spelling() {
        let id = nostrdb_net::NoteId::new([7; 32]);
        let bech = id.to_bech().expect("bech");
        for note in [id.hex(), bech.clone(), format!("nostr:{bech}")] {
            let action = open_thread(&json!({ "note": note })).expect("parses");
            assert!(
                matches!(action, NoteAction::Note { note_id, .. } if note_id == id),
                "{note}"
            );
        }
        assert!(open_thread(&json!({})).unwrap_err().contains("\"note\""));
        assert!(open_thread(&json!({ "note": "nope" }))
            .unwrap_err()
            .contains("not a note id"));
    }

    /// A hashtag opens with or without its `#`; more than one word doesn't.
    #[test]
    fn open_hashtag_drops_the_hash_and_wants_one_word() {
        for tag in ["nostr", "#nostr", " #nostr "] {
            let action = open_hashtag(&json!({ "hashtag": tag })).expect("parses");
            assert!(
                matches!(action, NoteAction::Hashtag(t) if t == "nostr"),
                "{tag}"
            );
        }
        for bad in ["", "#", "two words", "a#b"] {
            assert!(open_hashtag(&json!({ "hashtag": bad })).is_err(), "{bad:?}");
        }
    }
}
