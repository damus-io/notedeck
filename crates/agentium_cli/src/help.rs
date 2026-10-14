//! The CLI's help table: one entry per command, laid out by
//! [`cli_term::help`].
//!
//! `agentium --help` prints the command list grouped by section — a name and a
//! one-line summary each — plus the options that apply to every run. Everything
//! command-specific lives on that command's own page, printed by `agentium <cmd>
//! --help` (or `agentium help <cmd>`), so a reader looking up `spawn` isn't
//! reading past every `log` and `approve` flag to find it.
//!
//! [`COMMANDS`] is also the CLI's list of command *names*: parsing looks a name
//! up here before dispatching, so a command with no entry is rejected rather
//! than shipping undocumented.

use cli_term::help::{Cli, CommandHelp};

// The sections a command is listed under in `agentium --help`, printed in the
// order [`CLI`] lists them.
const SESSIONS: &str = "SESSIONS";
const CONTROL: &str = "CONTROL";
const CONFIGS: &str = "RUN CONFIGS";
const KEYS: &str = "KEYS & OFFLINE";

/// The whole help surface: the table plus how the overview frames it.
const CLI: Cli = Cli {
    bin: "agentium",
    about: "query and control Dave agentic sessions over a notedeck relay",
    groups: &[SESSIONS, CONTROL, CONFIGS, KEYS],
    commands: COMMANDS,
    global_hint: "--relay, --nsec, ...",
};

// Options more than one command reads, so their wording can't drift between
// the pages that list them.

const JSON: (&str, &str) = ("--json", "Machine-readable output");
const NO_SYNC: (&str, &str) = (
    "--no-sync",
    "Skip the relay reconcile and read the local cache as it stands — the fast path for repeated reads",
);
const HOST: (&str, &str) = ("--host <h>", "Only sessions whose host contains <h>");
const STATUS: (&str, &str) = (
    "--status <s>",
    "Only sessions with exactly this status (idle|working|needs_input|error|done|pending)",
);
const CWD: (&str, &str) = ("--cwd <c>", "Only sessions whose working dir contains <c>");
const BACKEND: (&str, &str) = ("--backend <b>", "Only sessions whose backend contains <b>");
const DELETED: (&str, &str) = ("--deleted", "Only soft-deleted (tombstoned) sessions");
const ALL: (&str, &str) = ("--all", "Live and deleted sessions together");
const ROLE: (&str, &str) = (
    "--role <r[,r…]>",
    "Only messages with these roles, comma-separated and/or repeatable — user, assistant, tool_call, tool_result, permission_request, subagent, system, error, compaction, todo",
);
const LAST: (&str, &str) = (
    "-n, --last <n>",
    "Only the last <n> messages (after the other filters)",
);
const TOOLS: (&str, &str) = ("--tools", "Also include tool_call/tool_result messages");
const NO_TOOLS: (&str, &str) = (
    "--no-tools",
    "Fold away tool_call/tool_result noise (the default)",
);
const COLOR: (&str, &str) = (
    "--color <when>",
    "auto (default) | always | never. `always` keeps color when piping into your own pager (e.g. `less -SR`)",
);
const PAGER: (&str, &str) = ("--pager", "Page even when stdout isn't a terminal");
const NO_PAGER: (&str, &str) = (
    "--no-pager",
    "Never page; write straight to stdout. (Pager: $AGENTIUM_PAGER, else $PAGER, else `less -R`)",
);
const REQUEST: (&str, &str) = (
    "--request <id>",
    "The pending request to answer, by perm-id prefix (the 8-digit ids `show` prints). Default: the newest",
);
const MESSAGE: (&str, &str) = (
    "--message <text>",
    "Reply text sent with the decision — a deny reason, or a note with an approve — which the agent sees",
);

/// What `approve` and `deny` share: they are two names for one action with
/// opposite answers.
const RESPOND_DETAILS: &str = "\
Answer a live session's pending permission request — by default the
newest; --request picks another. `show` lists what is pending. An
AskUserQuestion request can be denied but not approved (it needs
answers; use Dave).

The host matches the answer to the request it holds in memory: if it
restarted since, the answer is ignored and the request stays pending,
with no error.

--json emits { session, event_id, perm_id, … } on one line.";

/// Every command the CLI accepts. Parsing rejects a name that isn't here, so an
/// added command that's missing its entry fails loudly instead of silently
/// shipping with no help.
pub const COMMANDS: &[CommandHelp] = &[
    // -- sessions ---------------------------------------------------------
    CommandHelp {
        name: "list",
        group: SESSIONS,
        summary: "List your sessions, newest first, grouped by host",
        usage: &["list [<filters>] [--json]"],
        details: "\
<filters> are --host, --status, --cwd and --backend, case-insensitive,
and --deleted/--all: deleted sessions are hidden unless one of those is
passed. --json emits the raw session set.",
        options: &[HOST, STATUS, CWD, BACKEND, DELETED, ALL, NO_SYNC, JSON],
        examples: &[
            "agentium list",
            "agentium list --cwd notedeck --status working",
            "agentium list --deleted --json",
        ],
    },
    CommandHelp {
        name: "watch",
        group: SESSIONS,
        summary: "Live dashboard of every session, waiting-on-you first",
        usage: &["watch [<filters>] [--once] [--color <when>]"],
        details: "\
One row per session: status, title, host, cwd, backend, mode and last
activity (which counts streamed messages, not just status changes),
sessions waiting on you first. Redraws in place on a terminal until
Ctrl-C; into a pipe, prints each changed frame. Lines clip to $COLUMNS,
else the terminal width; piped frames are never clipped.

Takes the same <filters> as `list`. --once prints one frame and exits,
and is the only form --no-sync allows.",
        options: &[
            HOST,
            STATUS,
            CWD,
            BACKEND,
            DELETED,
            ALL,
            ("--once", "Print one frame and exit instead of following"),
            COLOR,
            NO_SYNC,
        ],
        examples: &["agentium watch", "agentium watch --host mbp --once"],
    },
    CommandHelp {
        name: "show",
        group: SESSIONS,
        summary: "One session's detail: state, configs, usage, requests",
        usage: &["show [session] [--json]"],
        details: "\
Print one session's state, the run-configs on its host+cwd, its latest
usage, a conversation summary (message count and each pending
permission request, with the id `approve`/`deny --request` takes), and
a subagent rollup (counts by status and one line each; sessions recorded
before subagents were published show none).

[session] is any selector `list` accepts: a d-tag, a cli-session id, or
an agentium: ref. It defaults to $AGENTIUM_SESSION, so a running Dave
session can just run `agentium show`. --json emits the structured detail
object.",
        options: &[NO_SYNC, JSON],
        examples: &[
            "agentium show",
            "agentium show agentium:oil-merge-push --json",
        ],
    },
    CommandHelp {
        name: "log",
        group: SESSIONS,
        summary: "Print one session's conversation, one entry per message",
        usage: &["log [session] [--role <r>] [-n <n>] [--tools] [-f] [--json|--jsonl]"],
        details: "\
Messages come in order (millisecond wall-clock, not seq). [session] is
any selector `list` accepts and defaults to $AGENTIUM_SESSION. Tool
call/result noise is folded away by default; --tools includes it.

--json emits structured message objects; --jsonl the reconstructed
claude-code JSONL from the source archive.

--follow keeps streaming new messages (and status changes) after the
tail, like `tail -f`, until Ctrl-C. It conflicts with --pager and
--jsonl (a live stream can't be paged or reconstructed); with --json it
streams newline-delimited objects.",
        options: &[
            ROLE,
            LAST,
            TOOLS,
            NO_TOOLS,
            (
                "--jsonl",
                "Emit reconstructed claude-code JSONL (from the source archive)",
            ),
            COLOR,
            PAGER,
            NO_PAGER,
            (
                "-f, --follow",
                "After the tail, stream new messages as they land until Ctrl-C",
            ),
            NO_SYNC,
            JSON,
        ],
        examples: &[
            "agentium log",
            "agentium log agentium:oil-merge-push --role user,assistant -n 20",
            "agentium log agentium:oil-merge-push -f",
        ],
    },
    CommandHelp {
        name: "grep",
        group: SESSIONS,
        summary: "Search message text across every session",
        usage: &["grep <pattern> [-i|-s] [<filters>] [--role <r>] [-n <n>] [--tools] [--json]"],
        details: "\
Print each matching line under its session's agentium: ref and status.
<pattern> is a regex. Case is smart by default: an all-lowercase
pattern matches any case, one carrying an uppercase letter matches
exactly; -i and -s force one or the other.

Searches every session the `list` <filters> select, deleted ones included
(--deleted narrows it to only those). One sync and one cache read cover
every session, so it is flat in session count where a per-session
`log | grep` loop is not. The `log` filters --role/--last/--tools narrow
which messages are searched (tool noise is folded away by default).
--json groups the matches under each session.",
        options: &[
            (
                "-i, --ignore-case",
                "Match any case, even with uppercase in the pattern",
            ),
            (
                "-s, --case-sensitive",
                "Match case exactly, even for a lowercase pattern",
            ),
            HOST,
            STATUS,
            CWD,
            BACKEND,
            DELETED,
            (
                "--all",
                "Live and deleted sessions together (grep's default)",
            ),
            ROLE,
            LAST,
            TOOLS,
            NO_TOOLS,
            COLOR,
            PAGER,
            NO_PAGER,
            NO_SYNC,
            JSON,
        ],
        examples: &[
            "agentium grep 'relay reconnect'",
            "agentium grep -i nostrdb --cwd notedeck --role assistant",
        ],
    },
    // -- control ----------------------------------------------------------
    CommandHelp {
        name: "spawn",
        group: CONTROL,
        summary: "Tell a (local or remote) Dave host to start a session",
        usage: &[
            "spawn [--host <h>] [--cwd <c>] [--title <t>] [--prompt <text>] [--wait] [--json]",
        ],
        details: "\
Publish a spawn command for a Dave host, then print the new session's
agentium: ref. --host/--cwd/--backend pick the target (exact values,
not filters); omitted, they default to the current session's
($AGENTIUM_SESSION), so a bare `spawn` starts a sibling in the same
worktree on the same host.

--prompt rides the command, so the host delivers it as the session's
first message when the session comes up — even if a slow host outlasts
--wait (which --prompt still implies, only to report the ref).

A spawn that looks like a retry of a recent one (same host+cwd+title)
is refused, naming the session it would have duplicated; the host also
answers a retried spawn with the session it already made, so re-running
after a timeout is safe. --allow-duplicate opts out of both.

--json emits { spawn_id, host, session } on one line.",
        options: &[
            (
                "--host <h>",
                "The host to spawn on (default: the current session's)",
            ),
            (
                "--cwd <c>",
                "The working dir to spawn in (default: the current session's)",
            ),
            (
                "--backend <b>",
                "The backend to run, e.g. claude or codex (default: the current session's)",
            ),
            (
                "--title <text>",
                "Explicit, sticky session title (else it derives from — and churns with — the first message)",
            ),
            (
                "--prompt <text>",
                "The session's first user message, delivered by the host when the session comes up. Implies --wait",
            ),
            (
                "--prompt-file <p>",
                "Like --prompt, but read the message from file <p> (or stdin when <p> is `-`) — a long or multi-line prompt with no shell-escaping. Exclusive with --prompt",
            ),
            (
                "--wait",
                "Block (bounded) until the host answers with the new session's state, then print its agentium: ref",
            ),
            (
                "--wait-timeout <secs>",
                "Override the --wait bound for this run (default 30s, else $AGENTIUM_SPAWN_WAIT). A still-waiting note hits stderr after 8s so a slow host doesn't look like a hang",
            ),
            (
                "--permission-mode <m>",
                "The mode the new session's agent starts in, rather than the host's default: default (aka manual) | plan | accept_edits | auto | bypass. Asking for it in --prompt does NOT work — the backend has already started by the time it reads that message. bypass does no safety checking at all",
            ),
            (
                "--model <name>",
                "The model the new session's agent starts on, rather than the backend's default: an alias the backend knows (opus, sonnet, haiku) or a full model id (claude-opus-5-5, gpt-5.3-codex). Passed through as-is, the way /model in Dave is; a backend that doesn't know it says so when it starts",
            ),
            (
                "--issue-url <uri>",
                "The issue the session works, e.g. headway:<board>/<word-id>. Recorded on the session's state (`show` prints it) so pressing s on the session in Dave opens the card",
            ),
            (
                "--child",
                "The new session works for this one and reports its result back: each turn it finishes at rest sends its final text here as a message from it, which wakes this session. Recorded as `relation child` beside `parent-session`, this session's ref ($AGENTIUM_SESSION), on the new session's state; `show` prints both",
            ),
            (
                "--successor",
                "The new session carries this one's work on (a handoff). Recorded as `relation successor`. Exclusive with --child. With neither, a spawn from inside a session still records its `parent-session`, with no relation",
            ),
            (
                "--idempotency-key <k>",
                "Name this *request*, so a retry of it is recognized as the same spawn. Defaults to a digest of the request itself (host+cwd+backend+title+prompt+mode+model+issue), which already makes an identical re-run safe; pass your own when you have a better notion of identity (a job id, say)",
            ),
            (
                "--allow-duplicate",
                "Really spawn a second session the duplicate guard would refuse. Also drops the idempotency key, so the host doesn't re-impose the dedupe",
            ),
            JSON,
        ],
        examples: &[
            "agentium spawn --title 'fix the build' --prompt 'cargo build is red'",
            "agentium spawn --child --prompt 'check the build on mbp' --host mbp --wait",
            "agentium spawn --title headway:notedeck/x --issue-url headway:notedeck/x --wait --json --prompt-file -",
        ],
    },
    CommandHelp {
        name: "send",
        group: CONTROL,
        summary: "Send a user message to a live session",
        usage: &["send [--immediate] <session> <text…> [--json]"],
        details: "\
Publish a user message to a session's conversation so its running agent
(local or remote) picks it up over relay sync, then report the
resulting event id. The selector is required (there is no
$AGENTIUM_SESSION default — it would be ambiguous against the text);
the message is the remaining words joined with spaces (quote to
preserve exact spacing). Reopen a deleted session with `resume` first.

Sent from inside a session ($AGENTIUM_SESSION set), the message names
that session as its sender.",
        options: &[
            (
                "--immediate",
                "Also interrupt the session's running turn, so the agent stops and reads the message now instead of when the turn ends. An idle session isn't interrupted, since the message starts its turn",
            ),
            (
                "--json",
                "Emit the event id (and, with --immediate, `interrupted`)",
            ),
        ],
        examples: &[
            "agentium send agentium:oil-merge-push are you blocked on anything?",
            "agentium send --immediate agentium:oil-merge-push stop, wrong branch",
        ],
    },
    CommandHelp {
        name: "resume",
        group: CONTROL,
        summary: "Reopen a closed (even soft-deleted) session on its host",
        usage: &["resume <session>"],
        details: "\
Reopen the session so a new message drives its backend again. Takes any
selector `list` accepts (a d-tag, cli-session id, or agentium: ref);
revives the session's agentium: reference in place.",
        options: &[],
        examples: &["agentium resume agentium:oil-merge-push"],
    },
    CommandHelp {
        name: "interrupt",
        group: CONTROL,
        summary: "Abort a live session's in-flight turn — Esc in Dave",
        usage: &["interrupt <session>"],
        details: "\
Takes any selector `list` accepts; a deleted session has nothing running
to interrupt.",
        options: &[],
        examples: &["agentium interrupt agentium:oil-merge-push"],
    },
    CommandHelp {
        name: "approve",
        group: CONTROL,
        summary: "Approve a live session's pending permission request",
        usage: &["approve <session> [--request <id>] [--message <text>] [--json]"],
        details: RESPOND_DETAILS,
        options: &[REQUEST, MESSAGE, JSON],
        examples: &[
            "agentium approve agentium:oil-merge-push",
            "agentium approve agentium:oil-merge-push --request 1a2b3c4d",
        ],
    },
    CommandHelp {
        name: "deny",
        group: CONTROL,
        summary: "Deny a live session's pending permission request",
        usage: &["deny <session> [--request <id>] [--message <text>] [--interrupt] [--json]"],
        details: RESPOND_DETAILS,
        options: &[
            REQUEST,
            MESSAGE,
            (
                "--interrupt",
                "Deny and stop the turn, rather than letting the agent carry on without the tool",
            ),
            JSON,
        ],
        examples: &[
            "agentium deny agentium:oil-merge-push --message 'use the test db'",
            "agentium deny agentium:oil-merge-push --interrupt",
        ],
    },
    CommandHelp {
        name: "mode",
        group: CONTROL,
        summary: "Change a live session's permission mode — Ctrl+M in Dave",
        usage: &["mode <session> <mode> [--json]"],
        details: "\
<mode> is one of: default (aka manual) | plan | accept_edits | auto |
bypass. --json emits { session, event_id, mode }.",
        options: &[JSON],
        examples: &["agentium mode agentium:oil-merge-push plan"],
    },
    CommandHelp {
        name: "link",
        group: CONTROL,
        summary: "Link a live session to the issue it works, after the fact",
        usage: &["link <session> <issue-url> [--json]"],
        details: "\
`spawn --issue-url` for a session spawned without it, so `show` names
the card and `s` in Dave opens it. For a headway card the URL is
headway:<board>/<word-id>. The session's host applies it and
republishes the session; link waits up to 10s for that and fails if it
doesn't come (a host built before `link` ignores the command).

--json emits { session, issue_url, event_id, unchanged }.",
        options: &[JSON],
        examples: &[
            "agentium link agentium:oil-merge-push headway:notedeck/amazing-galaxy-impulse",
        ],
    },
    CommandHelp {
        name: "unlink",
        group: CONTROL,
        summary: "Clear a live session's issue link",
        usage: &["unlink <session> [--json]"],
        details: "\
The reverse of `link`, applied the same way: the host republishes the
session and unlink waits up to 10s for it.",
        options: &[JSON],
        examples: &["agentium unlink agentium:oil-merge-push"],
    },
    // -- run configs ------------------------------------------------------
    CommandHelp {
        name: "config",
        group: CONFIGS,
        summary: "List, show, add, edit or remove run configs",
        usage: &[
            "config [list] [--host <h>] [--cwd <c>] [--json]",
            "config show <config> [--host <h>] [--cwd <c>]",
            "config add --name <n> --command <cmd> [--host <h>] [--cwd <c>]",
            "config edit <config> [--name <n>] [--command <cmd>]",
            "config rm <config>",
        ],
        details: "\
Run configs are the named shell commands Dave's run bar launches in a
host+cwd.

list prints them on every host, grouped host → cwd; --host/--cwd narrow
it (substring, like `list`), and --json emits the flat array.

show prints one. <config> is an id prefix (the 8-char ids `config list`
prints) or an exact name; --host/--cwd narrow what it resolves against.

add registers one. It lands on --host/--cwd (exact values here), which
default to the current session's ($AGENTIUM_SESSION).

edit renames a config or changes its command; its id stays. rm deletes
it (Dave kills it if it is running). add, edit and rm each emit
{ action, event_id, config } on one line with --json.",
        options: &[
            (
                "--host <h>",
                "list/show: hosts containing <h>. add: the exact host to register on",
            ),
            (
                "--cwd <c>",
                "list/show: working dirs containing <c>. add: the exact dir to register in",
            ),
            ("--name <n>", "add/edit: the config's name"),
            ("--command <cmd>", "add/edit: the shell command it runs"),
            ("--no-sync", "list/show: read the local cache as it stands"),
            JSON,
        ],
        examples: &[
            "agentium config",
            "agentium config add --name build --command 'cargo build'",
            "agentium config edit build --command 'cargo build --release'",
        ],
    },
    // -- keys & offline ---------------------------------------------------
    CommandHelp {
        name: "id",
        group: KEYS,
        summary: "Print the agentium: ref of each session d-tag",
        usage: &["id <d-tag…>"],
        details: "\
<d-tag> is a session's kind-31988 d-tag, e.g. from `ndb query --kind
31988`. Offline: needs no key, cache or relay.",
        options: &[],
        examples: &["agentium id 0b5c1f0e-6a4d-4c4e-9c1e-2f7d3a8b9c10"],
    },
    CommandHelp {
        name: "login",
        group: KEYS,
        summary: "Store a signing key for later runs",
        usage: &["login <nsec>"],
        details: "",
        options: &[],
        examples: &["agentium login nsec1…"],
    },
    CommandHelp {
        name: "logout",
        group: KEYS,
        summary: "Forget the stored signing key",
        usage: &["logout"],
        details: "",
        options: &[],
        examples: &["agentium logout"],
    },
];

/// The help entry for a command name, or `None` when the CLI has no such
/// command.
pub fn lookup(name: &str) -> Option<&'static CommandHelp> {
    CLI.lookup(name)
}

/// Print the grouped command list: `agentium --help`.
pub fn print_usage(default_relay: &str) {
    let globals: &[(&str, &str)] = &[
        (
            "--nsec <nsec>",
            "Signing key for this run. Normally unnecessary — run `agentium login` once and it's reused. $AGENTIUM_NSEC, if set, takes precedence over the stored key",
        ),
        (
            "--author <pk>",
            "Identity whose sessions to read (defaults to the signer). Sessions are PNS-encrypted to their owner, so a pubkey other than yours lists nothing decryptable",
        ),
        (
            "--relay <url>",
            &format!(
                "Relay URL. Passing it also remembers it as the default for later runs (--relay > $AGENTIUM_RELAY > stored > {default_relay})"
            ),
        ),
        (
            "--db <path>",
            "nostrdb cache dir, remembered like --relay [default: <data-dir>/agentium-cli]",
        ),
        ("-h, --help", "Print this help, or a command's own"),
    ];
    eprint!(
        "{}",
        CLI.overview(
            globals,
            "\
SESSION ARGUMENTS:
    <session> is any selector `list` accepts: a kind-31988 d-tag, a
    cli-session id, or an agentium:<word-id> ref.
",
        )
    );
}

/// Print one command's page: `agentium <cmd> --help`.
pub fn print_command(cmd: &CommandHelp) {
    eprint!("{}", CLI.page(cmd));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names are unique and every command is filed under a printed section.
    #[test]
    fn help_table_is_consistent() {
        CLI.check().unwrap();
    }
}
