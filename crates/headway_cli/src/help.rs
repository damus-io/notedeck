//! The CLI's help table: one entry per command, laid out by
//! [`cli_term::help`].
//!
//! `headway --help` prints the command list grouped by section — a name and a
//! one-line summary each — plus the options that apply to every run. Everything
//! command-specific lives on that command's own page, printed by `headway <cmd>
//! --help` (or `headway help <cmd>`), so a reader looking up one command isn't
//! reading past thirty unrelated flags to find its two.
//!
//! [`COMMANDS`] is also the CLI's list of command *names*: parsing looks a name
//! up here before dispatching, so a command with no entry is rejected rather
//! than shipping undocumented.

use cli_term::help::{Cli, CommandHelp, GlobalOption};

// The sections a command is listed under in `headway --help`, printed in the
// order [`CLI`] lists them.
const READING: &str = "READING";
const CARDS: &str = "CARDS";
const STRUCTURE: &str = "STRUCTURE & DEPENDENCIES";
const BOARDS: &str = "BOARDS";
const KEYS: &str = "KEYS";

/// The whole help surface: the table plus how the overview frames it.
const CLI: Cli = Cli {
    bin: "headway",
    about: "interact with a Headway board over a running notedeck's relay",
    groups: &[READING, CARDS, STRUCTURE, BOARDS, KEYS],
    commands: COMMANDS,
    global_hint: "--board, --relay, ...",
};

/// `--json` on a command that edits the board: the edit's result as an object
/// instead of the `ok (n events)` line. Every such command prints it the same
/// way, so they share one description.
const EDIT_JSON: (&str, &str) = (
    "--json",
    "Machine-readable result: {ok, events}, plus {card, ref} for a card add creates, or {noop} when nothing needed doing",
);

/// Every command the CLI accepts. Parsing rejects a name that isn't here, so an
/// added command that's missing its entry fails loudly instead of silently
/// shipping with no help.
pub const COMMANDS: &[CommandHelp] = &[
    // -- reading ----------------------------------------------------------
    CommandHelp {
        name: "show",
        group: READING,
        summary: "Print the board, or given cards in full detail",
        usage: &["show [cards...]", "show <card> --source <name>"],
        details: "\
With no arguments, print the whole board: every column and the cards on
it. With card selectors, print those cards `git show`-style instead —
title, description, metadata, relationships and comment thread.

Each review record in --json lists its sources (the files `review
--explainer-source`/`--code-source` stored) as {id, role, name, bytes,
created_at}. Only a card asked for by name also carries each source's
`text`: a board's JSON would otherwise hold every explainer on it.
--source prints one file's text exactly as stored, from the newest record
that has a file of that name, so a script can redirect it to disk.",
        options: &[
            (
                "--source <name>",
                "Print the named review source of the one card given",
            ),
            ("--archived", "List archived cards in full"),
            ("--all", "Print every board in the cache, not just this one"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "headway show",
            "headway show --board dave",
            "headway show headway:headway/report-raven-expand",
            "headway show headway:headway/report-raven-expand --source report-raven-expand.md > book/report-raven-expand.md",
        ],
    },
    CommandHelp {
        name: "next",
        group: READING,
        summary: "Print what to work on next (the ready frontier)",
        usage: &["next [--in <container>] [--ready] [-n <k>] [-l <label>...]"],
        details: "\
Walk a container's work-order and print the cards that are not done, not
already In Progress, and whose blockers are all cleared — what can be
picked up right now. Each is printed as a headway:<board>/<word-id> ref,
ready to paste into another command.

A card in the In Progress column (`--col in-progress`, matched by id or
name) is someone's already, so `next` never hands it out, with or without
`--ready`; a session resuming its own card should take it by its ref.
`--label` narrows what is printed to cards carrying every given label;
it never changes what counts as blocked or done.

A card is done when it sits in a terminal column, or when it has at
least one live (non-archived) subissue and every one of them is done —
recursively, so an epic of finished sub-epics is done too. A finished
epic stays in its column, but `next` never hands it out and nothing
stays blocked on it. A card with no live subissues is done only by its
column. An empty result means everything left is done, already In
Progress, blocked, or (with `--label`) unlabelled.

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
            (
                "-l, --label <l>",
                "Keep only cards carrying this label. Repeatable, and each value may be a comma-separated list; a card must carry all of them",
            ),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "headway next --board headway",
            "headway next --in headway:notedeck/saddle-because-liquid --ready",
            "headway next --in <milestone> --label role:model -n 1",
        ],
    },
    CommandHelp {
        name: "grep",
        group: READING,
        summary: "Search card text across every board",
        usage: &["grep <pattern> [--board <b>] [--in <c>] [-i|-s] [--color <when>] [--[no-]pager]"],
        details: "\
Print every line of card text that matches <pattern>, grouped under the
card's headway:<board>/<word-id> ref and title, with the field it came
from (the title, the description, a comment, or a review comment on one
of the card's commits) and how long ago it was written. Cards are listed
most recent first, by their newest matching line. Labels and review
records' commit subjects are not searched.

<pattern> is a regex. Case is smart by default: an all-lowercase pattern
matches any case, and a pattern with an uppercase letter matches exactly.
-i and -s force one or the other.

Searches every board you can read, archived cards included (their column
shows as `archived`). --board <b> narrows it to that board, and --in to a
card's subtree (on the board its ref names) or a board slug (the current
board). One fold of each board covers every card, so it stays one relay
sync however many cards match. A read-only command: it never signs or publishes.

On a terminal the matches are colored and paged, like `git grep`; piped,
they are neither. The pager is $HEADWAY_PAGER, else $PAGER, else
`less -R`.",
        options: &[
            (
                "--in <c>",
                "Search only this card and everything under it (a card ref), or the board slug for the whole board",
            ),
            (
                "-i, --ignore-case",
                "Match any case, even with uppercase in the pattern",
            ),
            (
                "-s, --case-sensitive",
                "Match case exactly, even for a lowercase pattern",
            ),
            ("--board <b>", "Search only this board"),
            (
                "--color <when>",
                "auto (default) | always | never. `always` keeps color when piping into your own pager (e.g. `less -SR`)",
            ),
            (
                "--pager",
                "Page the matches even when stdout isn't a terminal",
            ),
            ("--no-pager", "Never page; write straight to stdout"),
            (
                "--json",
                "Machine-readable output: one object per card, its matches grouped under it, each with a unix created_at",
            ),
            (
                "--all, --archived",
                "Accepted for older scripts, and change nothing: grep already searches every board, archived cards included",
            ),
        ],
        examples: &[
            "headway grep 'relay reconnect' --board notedeck",
            "headway grep -i nostrdb",
            "headway grep sealed --in headway:notedeck/saddle-because-liquid",
        ],
    },
    CommandHelp {
        name: "list",
        group: READING,
        summary: "List the cards a query matches, across every board",
        usage: &[
            "list [query...] [--board <b>] [--in <c>] [--all] [--show <props>] [--hide <props>] [--group <by>] [--sub-group <by>] [--empty] [--sort <by>] [--descending|--ascending] [--completed-by-recency] [--view <name>] [--save-view <name>] [-n <k>] [--json]",
            "list --views [--json]",
            "list --delete-view <name>",
        ],
        details: "\
Print one row per card the query keeps: its blocked and priority
markers, full ref, column and title, then its labels, subissue rollup,
estimate, due date and when it last changed. Rows come in board order
(board, then column, then rank) unless --sort says otherwise.

Which of those a row prints are its display properties: --show <props>
prints just these, --hide <props> drops these from the default (or from
--show's). Each is a comma-separated list of: id, status, priority,
labels, subissues, blockers, estimate, due, created, updated, board.
The title is always printed; --json always prints the whole card.

--group <by> gathers the rows under `## <group> (<count>)` headers, and
--sub-group <by> splits each group again, indented. <by> is one of:
status (same-named columns across boards are one group; in progress,
in review, then todo before backlog, done last), priority (urgent first), label (a card with two labels is in
both groups), board, parent, none. --empty also prints the groups no
card is in: every column, priority or board. A row leaves out what its
header already says. With --json, a grouped list is an array of
{group, count, rows, subgroups}.

--sort <by> orders the rows, within each group when grouped: manual
(board order), priority (urgent first, none last), updated and created
(newest first), due (soonest first, undated last), or work-order (each
board's work order, what `next` walks, with the cards `next --ready`
would hand out marked ▶; --json adds \"ready\"). --descending and
--ascending flip the natural direction; cards with no priority or due
date stay last either way. --completed-by-recency puts done cards after
the open ones, most recently updated first. -n keeps the first rows of
the sorted list.

The query is the same one the app's board filter reads: whitespace-
separated terms, all of which must hold.

  label:x  l:x          a label containing x
  priority:x  p:x       urgent|high|medium|low|none; p:high,urgent for either
  is:open|done|progress|any|archived
  is:blocked|unblocked
  status:x  col:x       the column, by id or name; col:todo,backlog for either
  due:any|none|<date>   dated, undated, or due on or before YYYY-MM-DD
  <ref>                 a pasted headway:<board>/<word-id>: that card
  anything else         text in the title, description, labels or word-id

With no is: term only open cards are listed (not in a done column);
--all lists done ones too. Archived cards are listed only for is:archived.

Lists every board you can read. --board <b> narrows it to that board, and
--in to a card's subtree (on the board its ref names).

A view is a named query and layout. --view <name> lists through one: its
query, with the run's query words added to it, and its grouping, ordering
and display properties, unless the run passes its own. Three are built in:
Active (is:open), Backlog (col:backlog) and All issues (is:any).
--save-view <name> saves the run's query and layout (and --board, if
given) as a view instead of listing, replacing the view of that name;
--views lists the views and --delete-view deletes one. Saved views are
yours alone: signed with your key, encrypted to it and kept in this
cache, never sent to a relay, so the app's list tabs keep their own.

Saving or deleting a view signs; otherwise the command never signs or
publishes.",
        options: &[
            ("--all", "Done cards too, when the query has no is: term"),
            (
                "--in <c>",
                "Only this card and everything under it (a card ref), or the board slug for the whole board",
            ),
            ("--board <b>", "List only this board"),
            (
                "--show <props>",
                "Print only these properties beside the title, e.g. id,status,due",
            ),
            (
                "--hide <props>",
                "Leave these properties out, e.g. labels,updated",
            ),
            (
                "--group <by>",
                "Gather rows under headers: status, priority, label, board, parent",
            ),
            (
                "--sub-group <by>",
                "Split each group again by another grouping",
            ),
            (
                "--empty",
                "Print empty groups too (every column, priority, board)",
            ),
            (
                "--sort <by>",
                "Order rows: manual, priority, updated, created, due, work-order",
            ),
            (
                "--descending, --ascending",
                "Run the ordering high-to-low or low-to-high instead of its natural way",
            ),
            (
                "--completed-by-recency",
                "Done cards after the open ones, most recently updated first",
            ),
            (
                "--view <name>",
                "List through a saved view, or Active, Backlog, All issues",
            ),
            (
                "--save-view <name>",
                "Save this run's query and layout as a view instead of listing",
            ),
            ("--views", "List the views instead of cards"),
            ("--delete-view <name>", "Delete a saved view"),
            ("-n, --count <k>", "Print at most k rows"),
            ("--color <when>", "auto (default) | always | never"),
            ("--pager", "Page the rows even when stdout isn't a terminal"),
            ("--no-pager", "Never page; write straight to stdout"),
            (
                "--json",
                "Machine-readable output: an array of cards as show --json renders them, plus board and column",
            ),
        ],
        examples: &[
            "headway list p:high label:ui",
            "headway list is:progress",
            "headway list is:done --board notedeck -n 10",
            "headway list --in headway:headway/acoustic-canvas-diesel --all",
            "headway list --group status --sub-group priority",
        ],
    },
    CommandHelp {
        name: "diff",
        group: READING,
        summary: "Print the commit a card's review record names",
        usage: &["diff <card> [--record <sha-prefix>] [--note explain|code]"],
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
to stdout, so the output pipes into `git apply`.

--note prints one of the commit's explain notes instead of its patch:
`explain` (refs/notes/explain, the page) or `code` (refs/notes/explain-code,
the code walk). The note is read from the repo the commit was found in,
else fetched from the same place the commit would be, into FETCH_HEAD only:
your checkout gains no refs and your own notes are never touched.",
        options: &[
            (
                "--record <sha>",
                "Show the record whose commit starts with this prefix instead of the newest",
            ),
            (
                "--note <explain|code>",
                "Print that explain note of the commit instead of its patch",
            ),
        ],
        examples: &[
            "headway diff headway:headway/report-raven-expand",
            "headway diff headway:headway/report-raven-expand --record 3d718bc",
            "headway diff headway:headway/report-raven-expand --note explain",
        ],
    },
    // -- cards ------------------------------------------------------------
    CommandHelp {
        name: "add",
        group: CARDS,
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
            (
                "--priority <p>",
                "Starting priority (none/low/medium/high/urgent), as `priority` takes it",
            ),
            (
                "--estimate <n>",
                "Starting estimate, as `estimate` takes it",
            ),
            (
                "--due <date>",
                "Starting due date (YYYY-MM-DD), as `due` takes it",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway add fix the flaky selfshare test --col todo -l test,flake",
            "headway add ship the beta --priority high --estimate 3 --due 2026-11-02",
            "headway add design the graph view --parent headway:headway/layer-ticket-vintage",
            "headway add write up the spike --desc-file - <<'EOF'\n    ## Context\n    ...\n    EOF",
        ],
    },
    CommandHelp {
        name: "move",
        group: CARDS,
        summary: "Move a card to a column",
        usage: &["move <card> --col <column>"],
        details: "",
        options: &[
            ("--col <c>", "Destination column (id or name). Required"),
            ("--row <n>", "Position within the column (0 is the top)"),
            EDIT_JSON,
        ],
        examples: &["headway move headway:headway/report-raven-expand --col in-review"],
    },
    CommandHelp {
        name: "title",
        group: CARDS,
        summary: "Edit a card's title",
        usage: &["title <card> <title...>"],
        details: "The new title is every remaining positional joined with spaces.",
        options: &[EDIT_JSON],
        examples: &["headway title headway:headway/report-raven-expand a clearer title"],
    },
    CommandHelp {
        name: "desc",
        group: CARDS,
        summary: "Edit a card's description",
        usage: &["desc <card> <text...>"],
        details: "\
Replaces the card's description with every remaining positional joined
with spaces. For long markdown, `add --desc-file` sets it at creation
time with no shell-escaping.",
        options: &[EDIT_JSON],
        examples: &["headway desc headway:headway/report-raven-expand the details"],
    },
    CommandHelp {
        name: "label",
        group: CARDS,
        summary: "Set a card's labels",
        usage: &["label <card> [labels...]"],
        details: "\
Sets the card's labels to exactly those given — this replaces the label
set rather than adding to it, and naming none clears them. Labels may be
separate positionals, one comma-separated positional, or `-l` flags; the
three spellings are equivalent.",
        options: &[
            (
                "-l, --label <l>",
                "Label(s), as an alternative to the positionals",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway label headway:headway/report-raven-expand cli,ux",
            "headway label headway:headway/report-raven-expand",
        ],
    },
    CommandHelp {
        name: "priority",
        group: CARDS,
        summary: "Set a card's priority",
        usage: &["priority <card> <none|low|medium|high|urgent>"],
        details: "",
        options: &[EDIT_JSON],
        examples: &["headway priority headway:headway/report-raven-expand high"],
    },
    CommandHelp {
        name: "due",
        group: CARDS,
        summary: "Set a card's due date",
        usage: &["due <card> <YYYY-MM-DD|none>"],
        details: "`none` clears the date.",
        options: &[EDIT_JSON],
        examples: &["headway due headway:headway/report-raven-expand 2026-10-01"],
    },
    CommandHelp {
        name: "estimate",
        group: CARDS,
        summary: "Set a card's estimate",
        usage: &["estimate <card> <points|none>"],
        details: "`none` clears the estimate.",
        options: &[EDIT_JSON],
        examples: &["headway estimate headway:headway/report-raven-expand 3"],
    },
    CommandHelp {
        name: "comment",
        group: CARDS,
        summary: "Comment on a card",
        usage: &[
            "comment <card> <text...>",
            "comment <card> --reply-to <comment> <text...>",
            "comment <card> --path <file> --line <a[-b]> [--old] [--record <sha>] <text...>",
            "comment <card> --record <sha> <text...>",
        ],
        details: "\
The comment body is every remaining positional joined with spaces.

Review comments: `show <card>` lists each review record's inline comments
under it, each with its word-id. --reply-to takes a card comment or a
review comment; a reply to a review comment goes on that comment's record,
under the same lines, not into the card's thread. --path and --line leave a
new inline comment on a review record's commit (the newest record, or the
one --record picks); --record alone comments on the whole commit.

With --comment-nsec <key> (or $HEADWAY_COMMENT_NSEC, or a key file named
by $HEADWAY_COMMENT_NSEC_FILE, in that order) the comment is signed by that
key instead of yours, so it shows as that key's comment. It is still sealed
into the board's channel with your access, so the key needs no membership.
Sealed boards only: a plaintext board shows just its owner's events, so
this is refused there.",
        options: &[
            (
                "--reply-to <c>",
                "Thread this reply under a comment or review comment on the same card (its id, a prefix, or its word-id)",
            ),
            (
                "--path <file>",
                "Comment on this file in a review record's commit, as the diff names it (needs --line)",
            ),
            (
                "--line <a[-b]>",
                "The line, or inclusive range, the comment points at (needs --path)",
            ),
            ("--old", "The lines are on the old (deleted) side"),
            (
                "--record <sha>",
                "The review record whose commit starts with this [default: the newest]",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway comment headway:headway/report-raven-expand committed abc123 (\"...\")",
            "headway comment headway:headway/report-raven-expand --path src/lib.rs --line 40-44 why a clone here?",
            "headway comment headway:headway/report-raven-expand --reply-to <word-id> fixed in def456",
        ],
    },
    CommandHelp {
        name: "review",
        group: CARDS,
        summary: "Record a commit for review on a card",
        usage: &[
            "review <card> [--commit <rev>] [--explainer <url>] [--deploy <url>] [--agentium <ref>]",
            "review <card> [--explainer-source <file>...] [--code-source <file>...]",
        ],
        details: "\
Appends a review record to the card: the commit's full sha and subject,
the branch, this host's name, the repo toplevel and the repo identity
(its root commit), all read from git in --repo-dir. The agentium ref
comes from --agentium, else $AGENTIUM_SESSION when it is one. A card
can hold several records, one per commit and host. Refuses outside a
git repo or when the rev doesn't resolve; a dirty tree is fine, since
the commit is what's recorded. When the commit's build was deployed
somewhere it can be opened and tried, --deploy records that URL so a
reviewer can go straight to it. --explainer-source and --code-source
store files on the record (the explainer's Markdown and its walk through
the code), each under its file name and read relative to the current
directory; repeatable, and refused whole when a file is unreadable, over
32 KiB, or shares a name with another. Prints the recorded fields.",
        options: &[
            ("--commit <rev>", "The commit to record [default: HEAD]"),
            (
                "--explainer <url>",
                "URL of the explainer page for the work",
            ),
            (
                "--deploy <url>",
                "URL where this commit's build is live, to open and try it",
            ),
            (
                "--explainer-source <file>",
                "Store the explainer page's source on the record (repeatable)",
            ),
            (
                "--code-source <file>",
                "Store a walk through the commit's code on the record (repeatable)",
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
            EDIT_JSON,
        ],
        examples: &[
            "headway review headway:headway/report-raven-expand --explainer https://claude.ai/artifact/...",
            "headway review headway:headway/report-raven-expand --commit abc123 --repo-dir ~/src/notedeck",
            "headway review headway:gamedev/blur-affair-manage --deploy https://example.com/builds/2866273e28a5/",
            "headway review headway:headway/report-raven-expand --explainer https://claude.ai/artifact/... --explainer-source book/report-raven-expand.md --code-source book/report-raven-expand.code.md",
        ],
    },
    CommandHelp {
        name: "delete",
        group: CARDS,
        summary: "Remove a card (a reversible tombstone)",
        usage: &["delete <card>"],
        details: "",
        options: &[EDIT_JSON],
        examples: &["headway delete headway:headway/report-raven-expand"],
    },
    CommandHelp {
        name: "archive",
        group: CARDS,
        summary: "Archive a card off the board",
        usage: &["archive <card>"],
        details: "`show --archived` lists what's been archived; `restore` puts one back.",
        options: &[EDIT_JSON],
        examples: &["headway archive headway:headway/report-raven-expand"],
    },
    CommandHelp {
        name: "restore",
        group: CARDS,
        summary: "Restore an archived card",
        usage: &["restore <card>"],
        details: "",
        options: &[EDIT_JSON],
        examples: &["headway restore headway:headway/report-raven-expand"],
    },
    // -- structure --------------------------------------------------------
    CommandHelp {
        name: "parent",
        group: STRUCTURE,
        summary: "Make a card a subissue of another, or detach it",
        usage: &["parent <card> [parent]"],
        details: "\
With a parent, <card> becomes its subissue; with the parent omitted,
<card> is detached back to the board root. Cycles are refused.",
        options: &[EDIT_JSON],
        examples: &[
            "headway parent headway:headway/report-raven-expand headway:headway/layer-ticket-vintage",
            "headway parent headway:headway/report-raven-expand",
        ],
    },
    CommandHelp {
        name: "block",
        group: STRUCTURE,
        summary: "Mark a card as blocked by another",
        usage: &["block <card> --on <blocker>", "block <card> <blocker>"],
        details: "\
A dependency edge, independent of the parent axis: a blocked card drops
out of the `next` frontier until its blocker is done — in a terminal
column, or an epic whose live subissues are all done.
The edge may cross boards. Cycles are refused.",
        options: &[
            (
                "--on <card>",
                "The blocker. May also be given as a second positional",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway block headway:headway/report-raven-expand --on headway:headway/fatal-girl-note",
        ],
    },
    CommandHelp {
        name: "unblock",
        group: STRUCTURE,
        summary: "Remove a blocked-by edge",
        usage: &["unblock <card> --on <blocker>", "unblock <card> <blocker>"],
        details: "",
        options: &[
            (
                "--on <card>",
                "The blocker to detach. May also be given as a second positional",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway unblock headway:headway/report-raven-expand --on headway:headway/fatal-girl-note",
        ],
    },
    CommandHelp {
        name: "relate",
        group: STRUCTURE,
        summary: "Relate two cards (an undirected \"see also\")",
        usage: &["relate <card> --to <other>", "relate <card> <other>"],
        details: "\
Symmetric and shown on both cards: it carries no ordering or readiness
meaning and never affects `next`. May cross boards.",
        options: &[
            (
                "--to <card>",
                "The other card. May also be given as a second positional",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway relate headway:headway/report-raven-expand --to headway:headway/fatal-girl-note",
        ],
    },
    CommandHelp {
        name: "unrelate",
        group: STRUCTURE,
        summary: "Remove a relates edge",
        usage: &["unrelate <card> --to <other>", "unrelate <card> <other>"],
        details: "The edge is symmetric, so either endpoint can remove it.",
        options: &[
            (
                "--to <card>",
                "The other card. May also be given as a second positional",
            ),
            EDIT_JSON,
        ],
        examples: &[
            "headway unrelate headway:headway/report-raven-expand --to headway:headway/fatal-girl-note",
        ],
    },
    CommandHelp {
        name: "seq",
        group: STRUCTURE,
        summary: "Show or set where a card sits in a container's work-order",
        usage: &[
            "seq <card> [--in <container>]",
            "seq <card> <--first|--last|--after <c>|--before <c>> [--in <container>]",
        ],
        details: "\
The work-order is what `next` walks. With no position flag, seq only
reads: it prints the card's place in the order (\"31 of 40\"), the
cards just before and after it, whether it has a rank of its own, how
many ready cards `next` would hand out ahead of it, and why it isn't
ready itself, if it isn't. --json gives the same as an object. A card
the container's walk never reaches is an error.

With one position flag, seq moves the card instead.",
        options: &[
            ("--first", "Put it at the front of the order"),
            ("--last", "Put it at the back"),
            ("--after <card>", "Put it immediately after <card>"),
            ("--before <card>", "Put it immediately before <card>"),
            (
                "--in <c>",
                "The container: a card ref (its subissues) or the board slug (the board root). Default: the board root",
            ),
            (
                "--json",
                "Machine-readable output: the card's place as an object, or a move's {ok, events}",
            ),
        ],
        examples: &[
            "headway seq headway:headway/report-raven-expand",
            "headway seq headway:headway/report-raven-expand --in headway:headway/layer-ticket-vintage --json",
            "headway seq headway:headway/report-raven-expand --first",
            "headway seq headway:headway/report-raven-expand --after headway:headway/fatal-girl-note --in headway:headway/layer-ticket-vintage",
        ],
    },
    // -- boards -----------------------------------------------------------
    CommandHelp {
        name: "board",
        group: BOARDS,
        summary: "List boards, or switch the persisted current board",
        usage: &["board [id]"],
        details: "\
With no argument, list the boards in the cache and mark the current
selection, then — under \"shared with me\" — the boards other people
shared with you, each with its owner. With an id, switch to it
persistently.

A shared board's slug is enough to address it: with no `--author`,
`--board <slug>` (or a headway:<slug>/<word-id> ref) that isn't one of
your own boards resolves to the one board shared with you under that
slug. If two owners shared one, pass `--author` to pick.

Scripts and agents should prefer naming the board per command —
`--board <id>`, or a self-routing headway:<board>/<word-id> card ref —
so a concurrent session switching the current board can't misroute an
edit.",
        options: &[],
        examples: &["headway board", "headway board dave"],
    },
    CommandHelp {
        name: "seed",
        group: BOARDS,
        summary: "Create the target board (born sealed)",
        usage: &["seed --board <slug> [--title <t>]", "seed --title <t>"],
        details: "\
Creates a board as a team-of-one SNS channel, sealed from note #1, so it
can later be shared without re-sealing anything.

The board is the one `--board` (or a self-routing card ref) names. With
no `--board`, the slug is derived from `--title`: lowercased, with every
run of other characters collapsed to `-`, so \"Tune Assistant\" seeds
`tune-assistant`. Seed never uses the persisted current board or
$HEADWAY_BOARD; with neither `--board` nor `--title` it refuses rather
than seed whichever board happens to be current.",
        options: &[(
            "-t, --title <t>",
            "Display title. Defaults to the board's slug, so a non-default board is never accidentally titled \"Headway\". Without --board, also the source of the slug",
        )],
        examples: &[
            "headway seed --board ios-port --title \"iOS Port\"",
            "headway seed -t \"Tune Assistant\"    # seeds tune-assistant",
        ],
    },
    CommandHelp {
        name: "rename",
        group: BOARDS,
        summary: "Rename the current board's display title",
        usage: &["rename <title...>"],
        details: "The board's slug is unchanged, so existing card refs keep working.",
        options: &[EDIT_JSON],
        examples: &["headway rename --board ios-port iOS Port"],
    },
    CommandHelp {
        name: "delete-board",
        group: BOARDS,
        summary: "Delete a board you own (a reversible tombstone)",
        usage: &["delete-board <board>"],
        details: "\
Republishes the board's definition marked deleted. The board drops out of
`headway board`, `show --all`, `grep` and the app's switcher, and refuses
further edits, but nothing else is touched: its cards, columns and title
stay, card refs into it still resolve, and `restore-board` brings it back
exactly as it was. Takes the board as an argument rather than using the
current one, so a stale selection can't delete the wrong board.",
        options: &[(
            "--json",
            "Machine-readable result: {ok, events, board}, or {noop} when it already was",
        )],
        examples: &["headway delete-board scratch"],
    },
    CommandHelp {
        name: "restore-board",
        group: BOARDS,
        summary: "Bring back a deleted board",
        usage: &["restore-board <board>"],
        details: "\
Republishes the board's definition without the deletion mark. `headway
board` lists deleted boards at the end so their slugs can be found.",
        options: &[(
            "--json",
            "Machine-readable result: {ok, events, board}, or {noop} when it already was",
        )],
        examples: &["headway restore-board scratch"],
    },
    CommandHelp {
        name: "terminal",
        group: BOARDS,
        summary: "Mark a column \"done\", or clear the flag",
        usage: &["terminal <col> [on|off]"],
        details: "\
A card in a terminal column clears its dependents and drops out of the
`next` frontier. The state defaults to `on`. Republishes the board
definition.",
        options: &[EDIT_JSON],
        examples: &[
            "headway terminal in-review --board headway",
            "headway terminal in-review off --board headway",
        ],
    },
    CommandHelp {
        name: "link",
        group: BOARDS,
        summary: "Also place a card on another board (keep both)",
        usage: &["link <card> --to <board>"],
        details: "\
Membership follows placement, so the card is genuinely on both boards —
it isn't copied, and edits from either side are the same card's.",
        options: &[("--to <board>", "The board to place it on too. Required")],
        examples: &["headway link headway:headway/report-raven-expand --to dave"],
    },
    CommandHelp {
        name: "move-board",
        group: BOARDS,
        summary: "Move a card to another board",
        usage: &["move-board <card> --to <board>"],
        details: "Links the card onto <board> and removes it from the current one.",
        options: &[("--to <board>", "The destination board. Required")],
        examples: &["headway move-board headway:headway/report-raven-expand --to dave"],
    },
    CommandHelp {
        name: "migrate",
        group: BOARDS,
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
    CommandHelp {
        name: "share",
        group: BOARDS,
        summary: "Make someone a member of a sealed board you own",
        usage: &["share <npub|hex> --board <id>"],
        details: "\
Gift-wraps the board's channel key to <npub> as a key-share (kind-1082
in a kind-1059). Their next run pulls it, joins the board, and can read
its whole history and edit it with `--author <you> --board <id>`.

Only the board's owner can share it, and only a sealed board (`headway
migrate` a plaintext one first). There is no revocation: the key can't
be taken back, so the board must be named explicitly.

Needs a reachable relay — a key-share is sent once and never re-sent by
a later run, so an offline share would be lost. Each run wraps with a
fresh throwaway key, so re-running publishes another copy; that's
harmless, since the member's roster dedups by channel and board.",
        options: &[(
            "--json",
            "Machine-readable output: {ok, board, recipient, team_pubkey}",
        )],
        examples: &[
            "headway share --board ios-port npub1...",
            "headway share --board ios-port --json <64-hex-pubkey>",
        ],
    },
    // -- keys -------------------------------------------------------------
    CommandHelp {
        name: "login",
        group: KEYS,
        summary: "Store a signing key for later runs",
        usage: &["login <nsec>"],
        details: "Touches neither the cache nor a relay.",
        options: &[],
        examples: &["headway login nsec1..."],
    },
    CommandHelp {
        name: "logout",
        group: KEYS,
        summary: "Forget the stored signing key",
        usage: &["logout"],
        details: "",
        options: &[],
        examples: &["headway logout"],
    },
];

/// The help entry for a command name, or `None` when the CLI has no such
/// command.
pub fn lookup(name: &str) -> Option<&'static CommandHelp> {
    CLI.lookup(name)
}

/// Hand `f` the options every command takes, as the overview and `headway
/// help options` print them. Built in one place so the pages and
/// [`check_flags`] read the same list: a global the help documents is one
/// every command accepts. (`-h` is the printer's own, [`cli_term::help::HELP`].)
fn with_globals<R>(
    default_relay: &str,
    default_board: &str,
    f: impl FnOnce(&[GlobalOption]) -> R,
) -> R {
    let board_summary = format!("Board for this run [default: {default_board}]");
    let board = format!(
        "Board for this run (or $HEADWAY_BOARD). Normally unnecessary — a headway:<board>/<word-id> card ref routes itself, and `headway board <id>` sets it persistently [default: {default_board}]"
    );
    let relay_summary = format!("Relay URL [default: {default_relay}]");
    let relay = format!("Relay URL (or $HEADWAY_RELAY) [default: {default_relay}]");
    f(&[
        GlobalOption {
            flag: "--board <id>",
            summary: &board_summary,
            details: &board,
        },
        GlobalOption {
            flag: "--nsec <key>",
            summary: "Signing key for this run [default: `headway login`]",
            details: "Signing key for this run, as nsec1… or a 64-char hex secret. Normally unnecessary — run `headway login` once and it's reused. $HEADWAY_NSEC, if set, takes precedence over the stored key",
        },
        GlobalOption {
            flag: "--comment-nsec <key>",
            summary: "A second key that signs only `comment`",
            details: "A second key (nsec1… or hex) that signs `comment` and nothing else, so an agent running as you has its comments attributed to itself. $HEADWAY_COMMENT_NSEC works too, and so does $HEADWAY_COMMENT_NSEC_FILE naming a file that holds it (read only when neither is set; an unreadable file is an error). The comment still seals into the board's channel with your key's access, and only sealed boards show it",
        },
        GlobalOption {
            flag: "--author <pk>",
            summary: "Board owner to read and edit [default: the signer]",
            details: "Board owner whose board to read and edit (defaults to the signer, or to whoever shared a board under --board's slug with you when you have none by that name). The signer may be a member of it rather than its owner",
        },
        GlobalOption {
            flag: "--relay <url>",
            summary: &relay_summary,
            details: &relay,
        },
        GlobalOption {
            flag: "--db <path>",
            summary: "nostrdb cache dir [default: <data-dir>/headway-cli]",
            details: "nostrdb cache dir [default: <data-dir>/headway-cli]",
        },
    ])
}

/// Hold a run of `cmd` to the flags its page documents plus the global ones:
/// `Err` naming the first flag in `passed` (each as typed) it doesn't take.
pub fn check_flags(cmd: &CommandHelp, passed: &[String]) -> Result<(), String> {
    with_globals(
        nostrdb_net::relay::sync::DEFAULT_RELAY,
        headway::store::BOARD_ID,
        |globals| CLI.check_flags(cmd, globals, passed),
    )
}

/// The grouped command list, `headway --help`, as printed.
fn overview(default_relay: &str, default_board: &str) -> String {
    with_globals(default_relay, default_board, |globals| {
        CLI.overview(globals)
    })
}

/// The global options in full, `headway help options`, as printed: they and
/// how a `<card>` or `<col>` argument is read, which every command shares.
fn options_page(default_relay: &str, default_board: &str) -> String {
    with_globals(default_relay, default_board, |globals| {
        CLI.options_page(
            globals,
            "\
CARD AND COLUMN ARGUMENTS:
    <card> is a word-id, a card id, or a unique short prefix (see `show`). A
    full headway:<board>/<word-id> ref also routes the command to that board,
    so no --board is needed. <col> is a column id or name (case-insensitive).
",
        )
    })
}

/// Print the grouped command list: `headway --help`.
pub fn print_usage(default_relay: &str, default_board: &str) {
    eprint!("{}", overview(default_relay, default_board));
}

/// Print the global options in full: `headway help options`.
pub fn print_options(default_relay: &str, default_board: &str) {
    eprint!("{}", options_page(default_relay, default_board));
}

/// Print one command's page: `headway <cmd> --help`.
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

    /// `headway --help` fits one screen: at most 60 lines, none past 80
    /// columns. A command added past that has to make room — tighter
    /// summaries, or a regrouping — rather than push the list off the top.
    #[test]
    fn overview_fits_one_screen() {
        let out = overview(
            nostrdb_net::relay::sync::DEFAULT_RELAY,
            headway::store::BOARD_ID,
        );
        let lines = out.lines().count();
        assert!(lines <= 60, "`headway --help` is {lines} lines:\n{out}");
        for line in out.lines() {
            let width = line.chars().count();
            assert!(width <= 80, "{width} columns: {line:?}");
        }
    }

    /// `headway help options` carries every global's full text, word for
    /// word, plus how card and column arguments are read.
    #[test]
    fn options_page_has_every_global_in_full() {
        let relay = nostrdb_net::relay::sync::DEFAULT_RELAY;
        let board = headway::store::BOARD_ID;
        let out = options_page(relay, board);
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        with_globals(relay, board, |globals| {
            for global in globals {
                assert!(flat.contains(global.details), "{}", global.flag);
            }
        });
        assert!(out.contains("CARD AND COLUMN ARGUMENTS:"));
    }
}
