//! The CLI's help surface: one entry per command, printed two ways.
//!
//! `headway --help` prints the command list grouped by [`Group`] — a name and a
//! one-line summary each — plus the options that apply to every run. Everything
//! command-specific lives on that command's own page, printed by `headway <cmd>
//! --help` (or `headway help <cmd>`), so a reader looking up one command isn't
//! reading past thirty unrelated flags to find its two.
//!
//! [`COMMANDS`] is also the CLI's list of command *names*: parsing looks a name
//! up here before dispatching, so a command with no entry is rejected rather
//! than shipping undocumented.

/// The section a command is listed under in `headway --help`. Commands are
/// printed in the order the groups are declared here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Reading,
    Cards,
    Structure,
    Boards,
    Keys,
}

impl Group {
    /// The groups in the order `headway --help` prints them.
    const ALL: [Group; 5] = [
        Group::Reading,
        Group::Cards,
        Group::Structure,
        Group::Boards,
        Group::Keys,
    ];

    /// The section heading.
    fn title(self) -> &'static str {
        match self {
            Group::Reading => "READING",
            Group::Cards => "CARDS",
            Group::Structure => "STRUCTURE & DEPENDENCIES",
            Group::Boards => "BOARDS",
            Group::Keys => "KEYS",
        }
    }
}

/// One command's help: its line in the grouped list, and its own page.
pub struct Command {
    /// The name as typed, e.g. `move-board`.
    pub name: &'static str,
    pub group: Group,
    /// One line for the command list. No trailing period.
    pub summary: &'static str,
    /// The usage line(s) on the command's page, without the leading `headway`.
    pub usage: &'static [&'static str],
    /// The body of the command's page: what it does and why, pre-wrapped to the
    /// help column. Empty for a command the summary already covers.
    pub details: &'static str,
    /// The flags this command reads — only its own; the global ones stay on
    /// `headway --help`.
    pub options: &'static [(&'static str, &'static str)],
    /// Full command lines, printed verbatim.
    pub examples: &'static [&'static str],
}

/// Every command the CLI accepts. Parsing rejects a name that isn't here, so an
/// added command that's missing its entry fails loudly instead of silently
/// shipping with no help.
pub const COMMANDS: &[Command] = &[
    // -- reading ----------------------------------------------------------
    Command {
        name: "show",
        group: Group::Reading,
        summary: "Print the board, or given cards in full detail",
        usage: &["show [cards...]"],
        details: "\
With no arguments, print the whole board: every column and the cards on
it. With card selectors, print those cards `git show`-style instead —
title, description, metadata, relationships and comment thread.",
        options: &[
            ("--archived", "List archived cards in full"),
            ("--all", "Print every board in the cache, not just this one"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "headway show",
            "headway show --board dave",
            "headway show headway:headway/report-raven-expand",
        ],
    },
    Command {
        name: "next",
        group: Group::Reading,
        summary: "Print what to work on next (the ready frontier)",
        usage: &["next [--in <container>] [--ready] [-n <k>]"],
        details: "\
Walk a container's work-order and print the cards whose blockers are all
cleared — what can be picked up right now. Each is printed as a
headway:<board>/<word-id> ref, ready to paste into another command.

A read-only command: it never signs or publishes. It needs the board
named explicitly (`--board`, or an `--in` ref that carries one) so an
autonomous agent can't be raced by another session switching the
persisted current board between commands.",
        options: &[
            (
                "--in <c>",
                "The container: a card ref (walks its subissues) or the board slug (the board root). Default: the board root",
            ),
            (
                "--ready",
                "Print the whole ready set — the parallel-dispatch frontier — not just the first card",
            ),
            ("-n, --count <k>", "Cap how many cards are printed"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "headway next --board headway",
            "headway next --in headway:notedeck/saddle-because-liquid --ready",
        ],
    },
    Command {
        name: "diff",
        group: Group::Reading,
        summary: "Print the commit a card's review record names, fetching it if needed",
        usage: &["diff <card> [--record <sha-prefix>]"],
        details: "\
Resolve the card's newest review record (see `review`) to a commit on
this host and print it `git show`-style. The commit is looked for in the
recorded checkout when the record was made here, else in a checkout of
the same repo (matched by its root commit: this one, or any path a record
on this board says was recorded here), else in a headway-owned bare cache
under the data dir. When it isn't there it is fetched — from the record's
--remote, a remote of that repo pointing at the recording host, or
<host>:<path> over ssh — without prompting and without creating refs in
your checkout. If the sha is gone (a rebase), the commit is found by its
`Headway: <card ref>` trailer instead; a card with no record at all is
searched for that way too. Where it was found goes to stderr, the commit
to stdout, so the output pipes into `git apply`.",
        options: &[(
            "--record <sha>",
            "Show the record whose commit starts with this prefix instead of the newest",
        )],
        examples: &[
            "headway diff headway:headway/report-raven-expand",
            "headway diff headway:headway/report-raven-expand --record 3d718bc",
        ],
    },
    // -- cards ------------------------------------------------------------
    Command {
        name: "add",
        group: Group::Cards,
        summary: "Add a card",
        usage: &["add <title...>"],
        details: "\
The title is taken from every remaining positional, joined with spaces,
so it needs no quoting. The card lands in `--col` (the first column when
omitted).",
        options: &[
            ("--col <c>", "Column to add it to (id or name)"),
            (
                "-l, --label <l>",
                "Label(s) to tag it with. Repeatable, and each value may be a comma-separated list",
            ),
            (
                "--parent <card>",
                "Create it as a subissue of <card> rather than at the board root",
            ),
            ("--desc <text>", "Initial description (the cover note)"),
            (
                "--desc-file <p>",
                "Like --desc, but read from file <p> — or stdin when <p> is `-`, so a long markdown description heredocs in with no shell-escaping. Mutually exclusive with --desc",
            ),
        ],
        examples: &[
            "headway add fix the flaky selfshare test --col todo -l test,flake",
            "headway add design the graph view --parent headway:headway/layer-ticket-vintage",
            "headway add write up the spike --desc-file - <<'EOF'\n    ## Context\n    ...\n    EOF",
        ],
    },
    Command {
        name: "move",
        group: Group::Cards,
        summary: "Move a card to a column",
        usage: &["move <card> --col <column>"],
        details: "",
        options: &[
            ("--col <c>", "Destination column (id or name). Required"),
            ("--row <n>", "Position within the column (0 is the top)"),
        ],
        examples: &["headway move headway:headway/report-raven-expand --col in-review"],
    },
    Command {
        name: "title",
        group: Group::Cards,
        summary: "Edit a card's title",
        usage: &["title <card> <title...>"],
        details: "The new title is every remaining positional joined with spaces.",
        options: &[],
        examples: &["headway title headway:headway/report-raven-expand a clearer title"],
    },
    Command {
        name: "desc",
        group: Group::Cards,
        summary: "Edit a card's description",
        usage: &["desc <card> <text...>"],
        details: "\
Replaces the card's description with every remaining positional joined
with spaces. For long markdown, `add --desc-file` sets it at creation
time with no shell-escaping.",
        options: &[],
        examples: &["headway desc headway:headway/report-raven-expand the details"],
    },
    Command {
        name: "label",
        group: Group::Cards,
        summary: "Set a card's labels",
        usage: &["label <card> [labels...]"],
        details: "\
Sets the card's labels to exactly those given — this replaces the label
set rather than adding to it, and naming none clears them. Labels may be
separate positionals, one comma-separated positional, or `-l` flags; the
three spellings are equivalent.",
        options: &[(
            "-l, --label <l>",
            "Label(s), as an alternative to the positionals",
        )],
        examples: &[
            "headway label headway:headway/report-raven-expand cli,ux",
            "headway label headway:headway/report-raven-expand",
        ],
    },
    Command {
        name: "priority",
        group: Group::Cards,
        summary: "Set a card's priority",
        usage: &["priority <card> <none|low|medium|high|urgent>"],
        details: "",
        options: &[],
        examples: &["headway priority headway:headway/report-raven-expand high"],
    },
    Command {
        name: "due",
        group: Group::Cards,
        summary: "Set a card's due date",
        usage: &["due <card> <YYYY-MM-DD|none>"],
        details: "`none` clears the date.",
        options: &[],
        examples: &["headway due headway:headway/report-raven-expand 2026-10-01"],
    },
    Command {
        name: "estimate",
        group: Group::Cards,
        summary: "Set a card's estimate",
        usage: &["estimate <card> <points|none>"],
        details: "`none` clears the estimate.",
        options: &[],
        examples: &["headway estimate headway:headway/report-raven-expand 3"],
    },
    Command {
        name: "comment",
        group: Group::Cards,
        summary: "Comment on a card",
        usage: &["comment <card> <text...>"],
        details: "The comment body is every remaining positional joined with spaces.",
        options: &[(
            "--reply-to <c>",
            "Thread this reply under another comment on the same card (its id, a prefix, or its word-id)",
        )],
        examples: &[
            "headway comment headway:headway/report-raven-expand committed abc123 (\"...\")",
        ],
    },
    Command {
        name: "review",
        group: Group::Cards,
        summary: "Record a commit for review on a card (host, path, session, explainer)",
        usage: &["review <card> [--commit <rev>] [--explainer <url>] [--agentium <ref>]"],
        details: "\
Appends a review record to the card: the commit's full sha and subject,
the branch, this host's name, the repo toplevel and the repo identity
(its root commit), all read from git in --repo-dir. The agentium ref
comes from --agentium, else $AGENTIUM_SESSION when it is one. A card
can hold several records, one per commit and host. Refuses outside a
git repo or when the rev doesn't resolve; a dirty tree is fine, since
the commit is what's recorded. Prints the recorded fields.",
        options: &[
            ("--commit <rev>", "The commit to record [default: HEAD]"),
            (
                "--explainer <url>",
                "URL of the explainer page for the work",
            ),
            (
                "--agentium <ref>",
                "The session behind the work, as agentium:<word-id> [default: $AGENTIUM_SESSION]",
            ),
            (
                "--remote <url>",
                "An explicit fetch URL for the commit, overriding the host-derived one",
            ),
            (
                "--repo-dir <dir>",
                "Where to run git [default: the current directory]",
            ),
        ],
        examples: &[
            "headway review headway:headway/report-raven-expand --explainer https://claude.ai/artifact/...",
            "headway review headway:headway/report-raven-expand --commit abc123 --repo-dir ~/src/notedeck",
        ],
    },
    Command {
        name: "delete",
        group: Group::Cards,
        summary: "Remove a card (a reversible tombstone)",
        usage: &["delete <card>"],
        details: "",
        options: &[],
        examples: &["headway delete headway:headway/report-raven-expand"],
    },
    Command {
        name: "archive",
        group: Group::Cards,
        summary: "Archive a card off the board",
        usage: &["archive <card>"],
        details: "`show --archived` lists what's been archived; `restore` puts one back.",
        options: &[],
        examples: &["headway archive headway:headway/report-raven-expand"],
    },
    Command {
        name: "restore",
        group: Group::Cards,
        summary: "Restore an archived card",
        usage: &["restore <card>"],
        details: "",
        options: &[],
        examples: &["headway restore headway:headway/report-raven-expand"],
    },
    // -- structure --------------------------------------------------------
    Command {
        name: "parent",
        group: Group::Structure,
        summary: "Make a card a subissue of another, or detach it",
        usage: &["parent <card> [parent]"],
        details: "\
With a parent, <card> becomes its subissue; with the parent omitted,
<card> is detached back to the board root. Cycles are refused.",
        options: &[],
        examples: &[
            "headway parent headway:headway/report-raven-expand headway:headway/layer-ticket-vintage",
            "headway parent headway:headway/report-raven-expand",
        ],
    },
    Command {
        name: "block",
        group: Group::Structure,
        summary: "Mark a card as blocked by another",
        usage: &["block <card> --on <blocker>", "block <card> <blocker>"],
        details: "\
A dependency edge, independent of the parent axis: a blocked card drops
out of the `next` frontier until its blocker reaches a terminal column.
The edge may cross boards. Cycles are refused.",
        options: &[(
            "--on <card>",
            "The blocker. May also be given as a second positional",
        )],
        examples: &[
            "headway block headway:headway/report-raven-expand --on headway:headway/fatal-girl-note",
        ],
    },
    Command {
        name: "unblock",
        group: Group::Structure,
        summary: "Remove a blocked-by edge",
        usage: &["unblock <card> --on <blocker>", "unblock <card> <blocker>"],
        details: "",
        options: &[(
            "--on <card>",
            "The blocker to detach. May also be given as a second positional",
        )],
        examples: &[
            "headway unblock headway:headway/report-raven-expand --on headway:headway/fatal-girl-note",
        ],
    },
    Command {
        name: "relate",
        group: Group::Structure,
        summary: "Relate two cards (an undirected \"see also\")",
        usage: &["relate <card> --to <other>", "relate <card> <other>"],
        details: "\
Symmetric and shown on both cards: it carries no ordering or readiness
meaning and never affects `next`. May cross boards.",
        options: &[(
            "--to <card>",
            "The other card. May also be given as a second positional",
        )],
        examples: &[
            "headway relate headway:headway/report-raven-expand --to headway:headway/fatal-girl-note",
        ],
    },
    Command {
        name: "unrelate",
        group: Group::Structure,
        summary: "Remove a relates edge",
        usage: &["unrelate <card> --to <other>", "unrelate <card> <other>"],
        details: "The edge is symmetric, so either endpoint can remove it.",
        options: &[(
            "--to <card>",
            "The other card. May also be given as a second positional",
        )],
        examples: &[
            "headway unrelate headway:headway/report-raven-expand --to headway:headway/fatal-girl-note",
        ],
    },
    Command {
        name: "seq",
        group: Group::Structure,
        summary: "Position a card in a container's work-order",
        usage: &["seq <card> <--first|--last|--after <c>|--before <c>> [--in <container>]"],
        details: "\
The work-order is what `next` walks. Exactly one position flag is
required.",
        options: &[
            ("--first", "Put it at the front of the order"),
            ("--last", "Put it at the back"),
            ("--after <card>", "Put it immediately after <card>"),
            ("--before <card>", "Put it immediately before <card>"),
            (
                "--in <c>",
                "The container: a card ref (its subissues) or the board slug (the board root). Default: the board root",
            ),
        ],
        examples: &[
            "headway seq headway:headway/report-raven-expand --first",
            "headway seq headway:headway/report-raven-expand --after headway:headway/fatal-girl-note --in headway:headway/layer-ticket-vintage",
        ],
    },
    // -- boards -----------------------------------------------------------
    Command {
        name: "board",
        group: Group::Boards,
        summary: "List boards, or switch the persisted current board",
        usage: &["board [id]"],
        details: "\
With no argument, list the boards in the cache and mark the current
selection. With an id, switch to it persistently.

Scripts and agents should prefer naming the board per command —
`--board <id>`, or a self-routing headway:<board>/<word-id> card ref —
so a concurrent session switching the current board can't misroute an
edit.",
        options: &[],
        examples: &["headway board", "headway board dave"],
    },
    Command {
        name: "seed",
        group: Group::Boards,
        summary: "Create the target board (born sealed)",
        usage: &["seed [--title <t>]"],
        details: "\
Creates the board named by `--board` as a team-of-one SNS channel,
sealed from note #1, so it can later be shared without re-sealing
anything.",
        options: &[(
            "--title <t>",
            "Display title. Defaults to the board's slug, so a non-default board is never accidentally titled \"Headway\"",
        )],
        examples: &["headway seed --board ios-port --title \"iOS Port\""],
    },
    Command {
        name: "rename",
        group: Group::Boards,
        summary: "Rename the current board's display title",
        usage: &["rename <title...>"],
        details: "The board's slug is unchanged, so existing card refs keep working.",
        options: &[],
        examples: &["headway rename --board ios-port iOS Port"],
    },
    Command {
        name: "terminal",
        group: Group::Boards,
        summary: "Mark a column \"done\", or clear the flag",
        usage: &["terminal <col> [on|off]"],
        details: "\
A card in a terminal column clears its dependents and drops out of the
`next` frontier. The state defaults to `on`. Republishes the board
definition.",
        options: &[],
        examples: &[
            "headway terminal in-review --board headway",
            "headway terminal in-review off --board headway",
        ],
    },
    Command {
        name: "link",
        group: Group::Boards,
        summary: "Also place a card on another board (keep both)",
        usage: &["link <card> --to <board>"],
        details: "\
Membership follows placement, so the card is genuinely on both boards —
it isn't copied, and edits from either side are the same card's.",
        options: &[("--to <board>", "The board to place it on too. Required")],
        examples: &["headway link headway:headway/report-raven-expand --to dave"],
    },
    Command {
        name: "move-board",
        group: Group::Boards,
        summary: "Move a card to another board",
        usage: &["move-board <card> --to <board>"],
        details: "Links the card onto <board> and removes it from the current one.",
        options: &[("--to <board>", "The destination board. Required")],
        examples: &["headway move-board headway:headway/report-raven-expand --to dave"],
    },
    Command {
        name: "migrate",
        group: Group::Boards,
        summary: "Seal an existing board under a fresh per-board key",
        usage: &["migrate --board <id> [--dry-run]"],
        details: "\
Migrates a plaintext board to SNS so it can be shared: existing notes
are re-sealed in place, with no data loss. Sealing is irreversible once
it reaches a relay, so the board must be named explicitly and `--dry-run`
is how you look first.",
        options: &[
            (
                "--dry-run",
                "Report what would be re-sealed and publish nothing",
            ),
            (
                "--new-channel",
                "Allow creating a channel for a board that appears to have none. Off by default: a board whose channel this cache merely can't see would be split in two",
            ),
        ],
        examples: &[
            "headway migrate --board ios-port --dry-run",
            "headway migrate --board ios-port",
        ],
    },
    // -- keys -------------------------------------------------------------
    Command {
        name: "login",
        group: Group::Keys,
        summary: "Store a signing key for later runs",
        usage: &["login <nsec>"],
        details: "Touches neither the cache nor a relay.",
        options: &[],
        examples: &["headway login nsec1..."],
    },
    Command {
        name: "logout",
        group: Group::Keys,
        summary: "Forget the stored signing key",
        usage: &["logout"],
        details: "",
        options: &[],
        examples: &["headway logout"],
    },
];

/// The help entry for a command name, or `None` when the CLI has no such
/// command.
pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// The column command names and option flags are padded to, in both help
/// layouts. Wide enough for the longest of each (`move-board`, `-n, --count <k>`).
const NAME_COL: usize = 18;

/// The width help text wraps to, so an option's description hangs under itself
/// rather than running off an 80-column terminal.
const WIDTH: usize = 78;

/// Print the grouped command list: `headway --help`.
pub fn print_usage(default_relay: &str, default_board: &str) {
    eprintln!(
        "\
headway — interact with a Headway board over a running notedeck's relay

USAGE:
    headway [OPTIONS] <COMMAND> [ARGS...]
    headway <COMMAND> --help      Options and examples for one command
"
    );

    for group in Group::ALL {
        eprintln!("{}", group.title());
        for cmd in COMMANDS.iter().filter(|c| c.group == group) {
            eprintln!("    {:<width$}{}", cmd.name, cmd.summary, width = NAME_COL);
        }
        eprintln!();
    }

    eprintln!(
        "\
OPTIONS (every command):
{opts}
CARD AND COLUMN ARGUMENTS:
    <card> is a word-id, a card id, or a unique short prefix (see `show`). A
    full headway:<board>/<word-id> ref also routes the command to that board,
    so no --board is needed. <col> is a column id or name (case-insensitive).

Run `headway <command> --help` for that command's own options and examples.",
        opts = options_block(&[
            (
                "--board <id>",
                &format!(
                    "Board for this run (or $HEADWAY_BOARD). Normally unnecessary — a headway:<board>/<word-id> card ref routes itself, and `headway board <id>` sets it persistently [default: {default_board}]"
                ),
            ),
            (
                "--nsec <key>",
                "Signing key for this run, as nsec1… or a 64-char hex secret. Normally unnecessary — run `headway login` once and it's reused. $HEADWAY_NSEC, if set, takes precedence over the stored key",
            ),
            (
                "--author <pk>",
                "Board owner whose board to read and edit (defaults to the signer). The signer may be a member of it rather than its owner",
            ),
            (
                "--relay <url>",
                &format!("Relay URL (or $HEADWAY_RELAY) [default: {default_relay}]"),
            ),
            (
                "--db <path>",
                "nostrdb cache dir [default: <data-dir>/headway-cli]"
            ),
            ("-h, --help", "Print this help, or a command's own"),
        ]),
    );
}

/// Print one command's page: `headway <cmd> --help`.
pub fn print_command(cmd: &Command) {
    eprintln!("headway {} — {}\n", cmd.name, cmd.summary);

    eprintln!("USAGE:");
    for usage in cmd.usage {
        eprintln!("    headway {usage}");
    }

    if !cmd.details.is_empty() {
        eprintln!();
        for line in cmd.details.lines() {
            eprintln!("{line}");
        }
    }

    if !cmd.options.is_empty() {
        eprintln!("\nOPTIONS:");
        eprint!("{}", options_block(cmd.options));
    }

    if !cmd.examples.is_empty() {
        eprintln!("\nEXAMPLES:");
        for example in cmd.examples {
            eprintln!("    {example}");
        }
    }

    eprintln!("\nOptions every command takes (--board, --relay, ...): headway --help");
}

/// Render `(flag, description)` pairs as an aligned, wrapped block — each line
/// already indented and newline-terminated, so a caller can `eprint!` it as-is.
fn options_block(options: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (flag, description) in options {
        // A flag wider than the column gets its description on the next line
        // rather than pushing the whole block out of alignment.
        let body = wrapped(description, NAME_COL + 4);
        if flag.len() >= NAME_COL || body.is_empty() {
            out.push_str(&format!("    {flag}\n"));
            out.push_str(&body);
        } else {
            // The first line's indent is replaced by the padded flag; the
            // continuation lines keep theirs, so they hang under the text.
            out.push_str(&format!("    {flag:<NAME_COL$}{}", body.trim_start()));
        }
    }
    out
}

/// Wrap `text` to [`WIDTH`] columns, indenting every line by `indent` spaces.
/// Words longer than the remaining width simply overflow — a URL is more useful
/// unbroken than wrapped. Width is counted in characters, not bytes: the help
/// text is full of em dashes, and measuring those as three columns each would
/// wrap every line that holds one short.
fn wrapped(text: &str, indent: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    let mut width = 0;
    for word in text.split_whitespace() {
        let word_width = word.chars().count();
        if !line.is_empty() && indent + width + 1 + word_width > WIDTH {
            out.push_str(&format!("{:indent$}{line}\n", ""));
            line.clear();
            width = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            width += 1;
        }
        line.push_str(word);
        width += word_width;
    }
    if !line.is_empty() {
        out.push_str(&format!("{:indent$}{line}\n", ""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Command names are unique — a duplicate would shadow its twin in
    /// [`lookup`] and print twice in the command list.
    #[test]
    fn command_names_are_unique() {
        for (i, cmd) in COMMANDS.iter().enumerate() {
            assert!(
                COMMANDS[..i].iter().all(|c| c.name != cmd.name),
                "duplicate help entry for '{}'",
                cmd.name
            );
        }
    }

    /// Every command belongs to a group that's actually printed, so no entry can
    /// go missing from `headway --help` by being filed under a section the list
    /// doesn't walk.
    #[test]
    fn every_command_is_printed() {
        let listed: usize = Group::ALL
            .iter()
            .map(|g| COMMANDS.iter().filter(|c| c.group == *g).count())
            .sum();
        assert_eq!(listed, COMMANDS.len());
    }

    /// Wrapping breaks at the help width and indents continuation lines.
    #[test]
    fn wrapping_indents_and_fits() {
        let text = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen";
        let out = wrapped(text, 8);
        for line in out.lines() {
            assert!(line.starts_with("        "), "not indented: {line:?}");
            let width = line.chars().count();
            assert!(width <= WIDTH, "too wide ({width}): {line:?}");
        }
        // Em dashes are one column each, not three: a line full of them must
        // still fill out to the wrap width rather than breaking early.
        let dashes = "— ".repeat(40);
        let dashed = wrapped(dashes.trim(), 4);
        let first = dashed.lines().next().unwrap();
        assert!(
            first.chars().count() > WIDTH - 2,
            "wrapped short on em dashes: {first:?}"
        );

        // No word is lost or duplicated.
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    /// An option block lines descriptions up under the same column, and a flag
    /// too wide for it drops its description to the next line instead.
    #[test]
    fn option_block_alignment() {
        let out = options_block(&[("--col <c>", "a column"), ("-n, --count <k>", "a cap")]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        for (line, (flag, text)) in lines
            .iter()
            .zip([("--col <c>", "a column"), ("-n, --count <k>", "a cap")])
        {
            assert!(line.starts_with(&format!("    {flag}")), "{line:?}");
            assert_eq!(
                &line[NAME_COL + 4..],
                text,
                "description off column: {line:?}"
            );
        }

        // A flag wider than the column keeps the block aligned by dropping its
        // description to the next line.
        let wide = options_block(&[("--a-very-long-flag-name <value>", "a description")]);
        let lines: Vec<&str> = wide.lines().collect();
        assert_eq!(lines[0], "    --a-very-long-flag-name <value>");
        assert_eq!(lines[1], format!("{:NAME_COL$}    a description", ""));
    }
}
