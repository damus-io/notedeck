//! Argument parsing: the command line into a [`Cli`] (or a help page), each
//! subcommand into a [`Command`], and self-routing a command to the board its
//! card refs name.

use std::env;
use std::ffi::OsString;

use nostrdb_net::Pubkey;

use headway::event::ReviewFields;
use headway::store;

use nostrdb_net::relay::sync::Result;

use crate::review::{self, ReviewFlags};
use crate::{APP, help};

/// A parsed command. Card arguments are still raw strings here; they're resolved
/// against the board once it's folded.
pub(crate) enum Command {
    Show {
        /// Optional card selectors. When non-empty, `show` prints these cards
        /// in full `git show`-style detail rather than the whole board.
        cards: Vec<String>,
    },
    /// Create the target board as a born team-of-one SNS channel (sealed from note
    /// #1). Optional `--title` sets its display name; when unset it defaults to the
    /// board's slug (so a non-default board is never accidentally titled "Headway").
    Seed {
        title: Option<String>,
    },
    /// Migrate the current board to SNS: seal it under a fresh per-board team key
    /// so it can be shared, re-sealing existing notes in place (no data loss). See
    /// [`store::migrate_board_to_sns`].
    Migrate,
    /// Gift-wrap the target board's channel root to `recipient` as a kind-1082
    /// key-share, making them a member. Owner-only, sealed boards only, and it
    /// needs a live relay: a key-share is never re-sent by a later run.
    Share {
        recipient: Pubkey,
    },
    Add {
        title: String,
        col: Option<String>,
        labels: Vec<String>,
        /// A card to parent the new one under (created as a subissue).
        parent: Option<String>,
        /// Initial description (cover note), already resolved from `--desc` or
        /// `--desc-file`. `None` means no description event is emitted.
        description: Option<String>,
    },
    Move {
        card: String,
        col: String,
        row: Option<usize>,
    },
    Title {
        card: String,
        title: String,
    },
    Desc {
        card: String,
        text: String,
    },
    Label {
        card: String,
        labels: Vec<String>,
    },
    /// Set a card's priority (none/low/medium/high/urgent).
    Priority {
        card: String,
        level: String,
    },
    /// Set a card's due date (`YYYY-MM-DD`, or `none` to clear).
    Due {
        card: String,
        date: String,
    },
    /// Set a card's estimate (a number, or `none` to clear).
    Estimate {
        card: String,
        points: String,
    },
    /// Position a card in a container's work-order (see [`SeqSpec`]). `container`
    /// is a card ref (its subissues) or omitted/board-slug (the board root).
    Seq {
        card: String,
        spec: SeqSpec,
        container: Option<String>,
    },
    /// Make a card a subissue of another card, or detach it (no parent given).
    Parent {
        card: String,
        parent: Option<String>,
    },
    /// Record that `card` is blocked by `on` (a dependency edge, independent of
    /// the parent axis and allowed to cross boards).
    Block {
        card: String,
        on: String,
    },
    /// Remove the `card`-blocked-by-`on` dependency edge.
    Unblock {
        card: String,
        on: String,
    },
    /// Relate `card` to `other` (an undirected "see also" edge — symmetric,
    /// carries no ordering or readiness meaning, and allowed to cross boards).
    Relate {
        card: String,
        other: String,
    },
    /// Remove the `card`-relates-`other` edge (symmetric: from either endpoint).
    Unrelate {
        card: String,
        other: String,
    },
    /// Print what to work on next: walk a container's work-order and show the
    /// ready frontier (see [`traversal`](headway::traversal)). A read command — it never signs.
    Next {
        /// `--in`: the container. A card ref (its subissues) or the board slug
        /// (board root); omitted means the board root. Shares the `seq --in`
        /// grammar via [`resolve_container`](crate::edit::resolve_container).
        container: Option<String>,
        /// `--ready`: print the whole ready set (parallel-dispatch frontier),
        /// not just the single next card.
        ready: bool,
        /// `-n <k>`: cap the number of cards printed.
        limit: Option<usize>,
    },
    /// Comment on a card, or — with `--path`/`--line`/`--record`, or a
    /// `--reply-to` naming a review comment — on one of its review records'
    /// commits (see [`build_action`](crate::edit::build_action)).
    Comment {
        card: String,
        body: String,
        /// A comment on the same card, or a review comment on one of its
        /// records, to thread this reply under.
        reply_to: Option<String>,
        /// Where on a review record's commit a new inline comment points.
        review: ReviewCommentFlags,
    },
    /// Record a review record on a card: a commit plus where it lives (host,
    /// path, repo) and the session and explainer behind it. The fields are
    /// already gathered from git and the environment (see [`review::gather`]).
    Review {
        card: String,
        review: ReviewFields,
    },
    /// Print the commit a card's review record names, fetching it from the
    /// recording host if this one lacks it. A read command — it never signs.
    Diff {
        card: String,
        /// `--record`: pick the record whose commit starts with this prefix
        /// instead of the newest.
        record: Option<String>,
    },
    Delete {
        card: String,
    },
    Archive {
        card: String,
    },
    Restore {
        card: String,
    },
    /// Place a card from the current board onto another board too, keeping it on
    /// both (placement-driven membership — it's the same card, not a copy).
    Link {
        card: String,
        to_board: String,
    },
    /// Move a card from the current board to another board: link it there and
    /// remove it from the current one.
    MoveBoard {
        card: String,
        to_board: String,
    },
    /// With an `id`, switch the persisted current board; without one, list the
    /// boards in the cache and mark the current selection.
    Board {
        id: Option<String>,
    },
    /// Rename the current board's display title (its slug is unchanged, so
    /// existing card refs keep working).
    Rename {
        title: String,
    },
    /// Mark a column terminal (a "done" column) or clear the flag. A card in a
    /// terminal column clears its dependents and drops out of the `next`
    /// frontier. Republishes the board definition.
    Terminal {
        col: String,
        terminal: bool,
    },
    Login {
        nsec: String,
    },
    Logout,
}

/// `comment`'s review flags as typed: where on which review record a new
/// inline comment goes. All unset means an ordinary card comment.
#[derive(Default)]
pub(crate) struct ReviewCommentFlags {
    /// `--path`: the file, as the diff names it.
    pub(crate) path: Option<String>,
    /// `--line`: `a` or `a-b`, parsed from the flag.
    pub(crate) lines: Option<(u32, u32)>,
    /// `--old`: the lines are on the old (deleted) side.
    pub(crate) old: bool,
    /// `--record`: the record whose commit starts with this prefix, rather
    /// than the newest.
    pub(crate) record: Option<String>,
}

impl ReviewCommentFlags {
    /// Any review flag was given, so the comment goes on a record.
    pub(crate) fn any(&self) -> bool {
        self.path.is_some() || self.lines.is_some() || self.old || self.record.is_some()
    }
}

/// Parse a `--line` value: `42` for one line or `42-48` for a range, 1-based
/// and inclusive, so `0` and a backwards range are refused.
fn parse_lines(value: &str) -> Result<(u32, u32)> {
    let bad = || format!("--line wants <a> or <a-b> (1-based), got '{value}'");
    let num = |s: &str| s.trim().parse::<u32>().ok().filter(|&n| n > 0);
    let (start, end) = match value.split_once('-') {
        Some((a, b)) => (num(a).ok_or_else(bad)?, num(b).ok_or_else(bad)?),
        None => {
            let a = num(value).ok_or_else(bad)?;
            (a, a)
        }
    };
    if end < start {
        return Err(bad().into());
    }
    Ok((start, end))
}

/// Where `seq` should place the card, parsed from `--after/--before/--first/--last`
/// (any card refs are resolved later, in [`build_action`](crate::edit::build_action)).
pub(crate) enum SeqSpec {
    First,
    Last,
    After(String),
    Before(String),
}

/// The `seq` position flags, collected during arg parsing and validated into a
/// [`SeqSpec`] by [`seq_spec`].
#[derive(Default)]
struct SeqFlags {
    after: Option<String>,
    before: Option<String>,
    first: bool,
    last: bool,
    /// `--in`: the container selector (card ref or board slug).
    container: Option<String>,
}

/// Validate that exactly one position flag was given and turn it into a [`SeqSpec`].
fn seq_spec(flags: &SeqFlags) -> Result<SeqSpec> {
    let count = flags.after.is_some() as u8
        + flags.before.is_some() as u8
        + flags.first as u8
        + flags.last as u8;
    if count != 1 {
        return Err(
            "seq needs exactly one of --after <card> / --before <card> / --first / --last".into(),
        );
    }
    Ok(if let Some(a) = &flags.after {
        SeqSpec::After(a.clone())
    } else if let Some(b) = &flags.before {
        SeqSpec::Before(b.clone())
    } else if flags.first {
        SeqSpec::First
    } else {
        SeqSpec::Last
    })
}

impl Command {
    /// The board named by this command's card selectors, when any of them is a
    /// `headway:<board>/<word-id>` reference (or the scheme-less shorthand — see
    /// [`ref_board`]). Used to self-route the command to that board. Errors when
    /// two selectors name different boards — cards are resolved against a single
    /// board per run.
    fn selector_board(&self) -> Result<Option<String>> {
        let mut selectors: Vec<&str> = Vec::new();
        match self {
            Command::Show { cards } => selectors.extend(cards.iter().map(String::as_str)),
            Command::Add { parent, .. } => selectors.extend(parent.as_deref()),
            Command::Parent { card, parent } => {
                selectors.push(card);
                selectors.extend(parent.as_deref());
            }
            Command::Block { card, on } | Command::Unblock { card, on } => {
                selectors.push(card);
                selectors.push(on);
            }
            Command::Relate { card, other } | Command::Unrelate { card, other } => {
                selectors.push(card);
                selectors.push(other);
            }
            Command::Seq {
                card,
                spec,
                container,
            } => {
                selectors.push(card);
                if let SeqSpec::After(a) | SeqSpec::Before(a) = spec {
                    selectors.push(a);
                }
                selectors.extend(container.as_deref());
            }
            Command::Next { container, .. } => selectors.extend(container.as_deref()),
            Command::Move { card, .. }
            | Command::Title { card, .. }
            | Command::Desc { card, .. }
            | Command::Label { card, .. }
            | Command::Priority { card, .. }
            | Command::Due { card, .. }
            | Command::Estimate { card, .. }
            | Command::Comment { card, .. }
            | Command::Review { card, .. }
            | Command::Diff { card, .. }
            | Command::Delete { card }
            | Command::Archive { card }
            | Command::Restore { card }
            | Command::Link { card, .. }
            | Command::MoveBoard { card, .. } => selectors.push(card),
            Command::Seed { .. }
            | Command::Migrate
            | Command::Share { .. }
            | Command::Rename { .. }
            | Command::Terminal { .. }
            | Command::Board { .. }
            | Command::Login { .. }
            | Command::Logout => {}
        }

        let mut found: Option<String> = None;
        for slug in selectors.into_iter().filter_map(ref_board) {
            match &found {
                Some(cur) if cur != &slug => {
                    return Err(
                        format!("card refs name different boards ('{cur}' and '{slug}')").into(),
                    );
                }
                _ => found = Some(slug),
            }
        }
        Ok(found)
    }
}

/// The `<board>` segment of a card reference — `headway:<board>/<word-id>` or its
/// scheme-less `<board>/<word-id>` shorthand — lowercased (board slugs are
/// lowercase). `None` for anything that isn't a reference: plain word ids, hex
/// ids/prefixes, and segments that aren't slug-shaped (see [`headway::wordid::parse_ref`]).
fn ref_board(sel: &str) -> Option<String> {
    headway::wordid::parse_ref(sel).map(|(board, _)| board.to_lowercase())
}

pub(crate) struct Cli {
    pub(crate) secret: Option<([u8; 32], Pubkey)>,
    /// A second key that signs `comment` and nothing else (`--comment-nsec`,
    /// `$HEADWAY_COMMENT_NSEC`, or the file `$HEADWAY_COMMENT_NSEC_FILE` names;
    /// see [`comment_key_text`]). Everything else, including which boards can be
    /// read and which channel the comment seals into, stays with [`secret`].
    /// An agent running as the account's owner uses this to have its comments
    /// attributed to itself, without holding any board key of its own
    /// (headway:headway/lava-number-clap).
    ///
    /// [`secret`]: Self::secret
    pub(crate) comment_secret: Option<([u8; 32], Pubkey)>,
    pub(crate) author: Option<Pubkey>,
    pub(crate) relay: String,
    pub(crate) db: Option<String>,
    pub(crate) board: String,
    /// Whether `board` was named explicitly — by `--board` or a self-routing
    /// `<board>#<word-id>` card ref — rather than falling back to
    /// `$HEADWAY_BOARD`, the persisted current board, or the default. `next`
    /// requires this so an autonomous agent can't be raced by another session
    /// flipping the persisted board between commands.
    pub(crate) board_explicit: bool,
    pub(crate) json: bool,
    pub(crate) archived: bool,
    /// `show` renders every board in the cache instead of just the current one.
    pub(crate) all: bool,
    /// `migrate` reports what it would re-seal and publishes nothing. The seal is
    /// irreversible once it reaches a relay, so the dry run is how you look first.
    pub(crate) dry_run: bool,
    /// `migrate` may create a channel for a board that has none. Off by default:
    /// a board whose channel this cache merely can't *see* would be split in two.
    pub(crate) new_channel: bool,
    pub(crate) command: Command,
}

/// What a command line asks for: a run, or one of the two help pages.
pub(crate) enum Invocation {
    Run(Box<Cli>),
    /// `headway` with no command, or `headway --help`: the grouped overview.
    Usage,
    /// `headway <cmd> --help` or `headway help <cmd>`: one command's own page.
    CommandHelp(&'static help::Command),
}

impl Cli {
    /// Parse args (without the program name), deciding between a run and a help
    /// page (see [`Invocation`]).
    pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Invocation> {
        // Precedence: `--nsec` (set below) overrides the `HEADWAY_NSEC` env var,
        // which overrides the key stored by `login`.
        let mut nsec = env::var("HEADWAY_NSEC")
            .ok()
            .or_else(|| nostrdb_net::relay::sync::stored_nsec(APP));
        // `--comment-nsec` (set below) overrides `HEADWAY_COMMENT_NSEC`, which
        // overrides the key file `HEADWAY_COMMENT_NSEC_FILE` names (read only
        // when neither is set, in `comment_key_text`).
        let mut comment_nsec = env::var("HEADWAY_COMMENT_NSEC").ok();
        let mut relay = env::var("HEADWAY_RELAY")
            .ok()
            .unwrap_or_else(|| nostrdb_net::relay::sync::DEFAULT_RELAY.to_string());
        let mut db = None;
        // Resolved after the command is parsed: `--board` overrides the board
        // named by a `<board>#<word-id>` card ref, which overrides
        // `$HEADWAY_BOARD`, which overrides the board stored by `headway board
        // <id>`, which overrides the default board.
        let mut board: Option<String> = None;
        let mut author = None;
        let mut json = false;
        let mut archived = false;
        let mut all = false;
        let mut dry_run = false;
        let mut new_channel = false;
        let mut col = None;
        let mut row = None;
        let mut to = None;
        let mut reply_to = None;
        let mut parent = None;
        // `add` description, from either the inline `--desc` or the escaping-free
        // `--desc-file` (a path, or `-` for stdin); folded into one value below.
        let mut desc = None;
        let mut desc_file = None;
        let mut on = None;
        // `seed` display title; when unset the title defaults to the board slug
        // (see `Command::Seed`).
        let mut title = None;
        let mut labels: Vec<String> = Vec::new();
        let mut seq = SeqFlags::default();
        // `next` flags.
        let mut ready = false;
        let mut count: Option<usize> = None;
        let mut review = ReviewFlags::default();
        // `diff --record`: which review record to show. `comment` shares it,
        // with its other review flags.
        let mut record = None;
        let mut review_comment = ReviewCommentFlags::default();
        let mut positionals: Vec<String> = Vec::new();
        // `-h`/`--help` is answered after the loop, once the positionals say
        // *which* help — the overview, or one command's page.
        let mut want_help = false;

        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value").into())
                    as Result<String>
            };
            match arg.as_str() {
                "-h" | "--help" => want_help = true,
                "--nsec" => nsec = Some(value("--nsec")?),
                "--comment-nsec" => comment_nsec = Some(value("--comment-nsec")?),
                "--relay" => relay = value("--relay")?,
                "--db" => db = Some(value("--db")?),
                "--board" => board = Some(value("--board")?),
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--col" => col = Some(value("--col")?),
                "--to" => to = Some(value("--to")?),
                "--reply-to" => reply_to = Some(value("--reply-to")?),
                "--parent" => parent = Some(value("--parent")?),
                "--desc" => desc = Some(value("--desc")?),
                "--desc-file" => desc_file = Some(value("--desc-file")?),
                "--on" => on = Some(value("--on")?),
                "--title" => title = Some(value("--title")?),
                "--after" => seq.after = Some(value("--after")?),
                "--before" => seq.before = Some(value("--before")?),
                "--first" => seq.first = true,
                "--last" => seq.last = true,
                "--in" => seq.container = Some(value("--in")?),
                "--ready" => ready = true,
                "--commit" => review.commit = Some(value("--commit")?),
                "--explainer" => review.explainer = Some(value("--explainer")?),
                "--agentium" => review.agentium = Some(value("--agentium")?),
                "--remote" => review.remote = Some(value("--remote")?),
                "--repo-dir" => review.repo_dir = Some(value("--repo-dir")?),
                "--record" => record = Some(value("--record")?),
                "--path" => review_comment.path = Some(value("--path")?),
                "--line" => review_comment.lines = Some(parse_lines(&value("--line")?)?),
                "--old" => review_comment.old = true,
                "-n" | "--count" => {
                    count = Some(value("-n")?.parse().map_err(|_| "-n must be a number")?)
                }
                "-l" | "--label" | "--labels" => {
                    // Repeatable, and each value may be a comma-separated list,
                    // so `-l a,b --label c` and `-l a -l b -l c` are equivalent.
                    labels.extend(split_labels(&value("--label")?));
                }
                "--row" => {
                    row = Some(
                        value("--row")?
                            .parse()
                            .map_err(|_| "--row must be a number")?,
                    )
                }
                "--json" => json = true,
                "--archived" => archived = true,
                "--all" => all = true,
                "--dry-run" => dry_run = true,
                "--new-channel" => new_channel = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        // `headway help [cmd]` is the same two pages spelled as a command, so
        // fold it into the `--help` path rather than giving it its own dispatch.
        let mut positionals = positionals.as_slice();
        if let Some(first) = positionals.first()
            && first == "help"
        {
            want_help = true;
            positionals = &positionals[1..];
        }
        let Some((name, rest)) = positionals.split_first() else {
            return Ok(Invocation::Usage);
        };
        // The help table is the CLI's list of commands: an unknown name is
        // rejected here, so `parse_command` never sees one and a command added
        // without a help entry can't ship undocumented.
        let Some(spec) = help::lookup(name) else {
            return Err(format!("unknown command '{name}' (try `headway --help`)").into());
        };
        if want_help {
            return Ok(Invocation::CommandHelp(spec));
        }
        // Fold `--desc`/`--desc-file` into one description: they name the same
        // thing (the new card's cover note), so passing both is a contradiction. A
        // file value of `-` means stdin, which lets a heredoc pipe a long markdown
        // description in with no shell-escaping.
        let description = match (desc, desc_file) {
            (Some(_), Some(_)) => {
                return Err("pass either --desc or --desc-file, not both".into());
            }
            (Some(text), None) => Some(text),
            (None, Some(path)) => Some(read_desc_source(&path)?),
            (None, None) => None,
        };
        let command = parse_command(
            name,
            rest,
            col,
            row,
            to,
            reply_to,
            parent,
            description,
            on,
            title,
            labels,
            seq,
            ready,
            count,
            review,
            record,
            review_comment,
        )?;

        // A card selector like `headway:commerce/purse-metal-toilet` already names
        // its board, so the command self-routes there — the display id `show`
        // prints is a working address wherever it's pasted, with no `--board`
        // needed. An explicit `--board` must agree with it rather than being
        // silently ignored (or silently winning and then failing "no card
        // matching" on the wrong board).
        if let Some(named) = command.selector_board()? {
            match &board {
                Some(flag) if flag != &named => {
                    return Err(
                        format!("--board {flag} conflicts with card ref board '{named}'").into(),
                    );
                }
                _ => board = Some(named),
            }
        }
        // Explicit iff `--board` or a self-routing ref set it above; the env /
        // persisted / default fallbacks below are implicit (see `board_explicit`).
        let board_explicit = board.is_some();
        let board = board
            .or_else(|| env::var("HEADWAY_BOARD").ok())
            .or_else(|| nostrdb_net::relay::sync::read_config(APP, "board"))
            .unwrap_or_else(|| store::BOARD_ID.to_string());

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured — that would
        // keep `login` from replacing a stale or malformed stored key.
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `headway` store/event layer) speaks `nostrdb_net::Pubkey`. Both are
        // `[u8; 32]` newtypes, so bridge at this boundary and keep everything
        // downstream in enostr terms.
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout, _) => None,
            (_, Some(nsec)) => {
                let (sk, pk) = parse_secret_key(&nsec)?;
                Some((sk, Pubkey::new(*pk.bytes())))
            }
            (_, None) => None,
        };
        let comment_secret = match &command {
            Command::Login { .. } | Command::Logout => None,
            _ => match comment_key_text(comment_nsec, env::var_os(COMMENT_NSEC_FILE_VAR))? {
                Some(key) => {
                    let (sk, pk) = parse_secret_key(&key).map_err(|e| {
                        format!(
                            "--comment-nsec / $HEADWAY_COMMENT_NSEC / ${COMMENT_NSEC_FILE_VAR}: {e}"
                        )
                    })?;
                    Some((sk, Pubkey::new(*pk.bytes())))
                }
                None => None,
            },
        };

        Ok(Invocation::Run(Box::new(Cli {
            secret,
            comment_secret,
            author,
            relay,
            db,
            board,
            board_explicit,
            json,
            archived,
            all,
            dry_run,
            new_channel,
            command,
        })))
    }
}

/// Split one label argument into the labels it names. A single argument may be a
/// comma-separated list, so `-l a,b` and `label <card> a,b` each set two labels;
/// surrounding whitespace is trimmed and empty entries (`a,,b`, a trailing comma)
/// dropped. Shared by the `-l`/`--label` flag and `label`'s positionals so the two
/// spellings can't disagree — a label containing a comma is deliberately
/// unrepresentable rather than settable by only one of them.
fn split_labels(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn parse_command(
    name: &str,
    rest: &[String],
    col: Option<String>,
    row: Option<usize>,
    to: Option<String>,
    reply_to: Option<String>,
    parent: Option<String>,
    description: Option<String>,
    on: Option<String>,
    title: Option<String>,
    labels: Vec<String>,
    seq: SeqFlags,
    ready: bool,
    count: Option<usize>,
    review: ReviewFlags,
    record: Option<String>,
    mut review_comment: ReviewCommentFlags,
) -> Result<Command> {
    let card = || -> Result<String> { arg(rest, 0, name) };
    Ok(match name {
        "show" => Command::Show {
            cards: rest.to_vec(),
        },
        "seed" => Command::Seed { title },
        "migrate" => Command::Migrate,
        "share" => Command::Share {
            recipient: Pubkey::parse(&arg(rest, 0, name)?)
                .map_err(|e| format!("share: not an npub or hex pubkey: {e}"))?,
        },
        "add" => Command::Add {
            title: joined(rest, 0, name)?,
            col,
            labels,
            parent,
            description,
        },
        "move" => Command::Move {
            card: card()?,
            col: col.ok_or("move needs --col <column>")?,
            row,
        },
        "title" => Command::Title {
            card: card()?,
            title: joined(rest, 1, name)?,
        },
        "desc" => Command::Desc {
            card: card()?,
            text: joined(rest, 1, name)?,
        },
        // Each positional may itself be a comma-separated list, so
        // `label <card> a,b` and `label <card> a b` agree — and agree with the
        // `add -l a,b` that may have set them. `-l`/`--label` flags are honoured
        // too: they'd otherwise leave no positionals, silently *clearing* the
        // card's labels (the no-labels-clears case) instead of setting them.
        "label" => Command::Label {
            card: card()?,
            labels: rest
                .get(1..)
                .unwrap_or_default()
                .iter()
                .flat_map(|l| split_labels(l))
                .chain(labels)
                .collect(),
        },
        "priority" => Command::Priority {
            card: card()?,
            level: arg(rest, 1, name)?,
        },
        "due" => Command::Due {
            card: card()?,
            date: arg(rest, 1, name)?,
        },
        "estimate" => Command::Estimate {
            card: card()?,
            points: arg(rest, 1, name)?,
        },
        "seq" => Command::Seq {
            card: card()?,
            spec: seq_spec(&seq)?,
            container: seq.container,
        },
        "next" => Command::Next {
            container: seq.container,
            ready,
            limit: count,
        },
        // `parent <card> <parent>` sets, `parent <card>` detaches — mirrors how
        // `label` with no labels clears.
        "parent" => Command::Parent {
            card: card()?,
            parent: rest.get(1).cloned(),
        },
        // `block <card> --on <blocker>`; the blocker may also be a second
        // positional so `block <card> <blocker>` works too.
        "block" => Command::Block {
            card: card()?,
            on: on
                .or_else(|| rest.get(1).cloned())
                .ok_or("block needs --on <card>")?,
        },
        "unblock" => Command::Unblock {
            card: card()?,
            on: on
                .or_else(|| rest.get(1).cloned())
                .ok_or("unblock needs --on <card>")?,
        },
        // `relate <card> --to <other>`; the partner may also be a second positional
        // so `relate <card> <other>` works too.
        "relate" => Command::Relate {
            card: card()?,
            other: to
                .or_else(|| rest.get(1).cloned())
                .ok_or("relate needs --to <card>")?,
        },
        "unrelate" => Command::Unrelate {
            card: card()?,
            other: to
                .or_else(|| rest.get(1).cloned())
                .ok_or("unrelate needs --to <card>")?,
        },
        "comment" => {
            review_comment.record = record;
            Command::Comment {
                card: card()?,
                body: joined(rest, 1, name)?,
                reply_to,
                review: review_comment,
            }
        }
        // Gathered here, before any relay work, so a bad rev or a directory
        // outside a repo fails fast (see `review::gather`).
        "review" => Command::Review {
            card: card()?,
            review: review::gather(review)?,
        },
        "diff" => Command::Diff {
            card: card()?,
            record,
        },
        "delete" => Command::Delete { card: card()? },
        "archive" => Command::Archive { card: card()? },
        "restore" => Command::Restore { card: card()? },
        "link" => Command::Link {
            card: card()?,
            to_board: to.ok_or("link needs --to <board>")?,
        },
        "move-board" => Command::MoveBoard {
            card: card()?,
            to_board: to.ok_or("move-board needs --to <board>")?,
        },
        "rename" => Command::Rename {
            title: joined(rest, 0, name)?,
        },
        // `terminal <col> [on|off]` — mark a column as a "done" column, or clear
        // it with `off`. Defaults to `on` when the state is omitted.
        "terminal" => Command::Terminal {
            col: arg(rest, 0, name)?,
            terminal: match rest.get(1).map(String::as_str) {
                None | Some("on" | "true" | "yes") => true,
                Some("off" | "false" | "no") => false,
                Some(other) => {
                    return Err(format!("terminal state must be on/off, got '{other}'").into());
                }
            },
        },
        "board" => Command::Board {
            id: rest.first().cloned(),
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        // Unreachable in practice: `Cli::parse` rejects a name the help table
        // doesn't list, so reaching here means a documented command was never
        // given a parser (see `every_documented_command_parses`).
        other => return Err(format!("command '{other}' has no parser").into()),
    })
}

/// The `idx`th positional argument to a command, or an error naming the command.
fn arg(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    rest.get(idx)
        .cloned()
        .ok_or_else(|| format!("`{cmd}` is missing an argument").into())
}

/// Everything from `idx` onward, space-joined — for free-text titles/bodies.
fn joined(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    let parts = rest.get(idx..).unwrap_or_default();
    if parts.is_empty() {
        return Err(format!("`{cmd}` is missing text").into());
    }
    Ok(parts.join(" "))
}

/// Parse a signing key given as bech32 `nsec1…` or as a 64-char hex secret,
/// returning the secret bytes and their pubkey. Surrounding whitespace is
/// trimmed first, so a key file's trailing newline (`--nsec "$(cat key)"`,
/// or `HEADWAY_NSEC` read from one) doesn't make it unparseable.
///
/// Hex is handled here rather than in `nostrdb_net`'s `parse_nsec` so accepting
/// it stays a CLI concern and needs no fork rev-bump; anything that isn't 64 hex
/// characters falls through to the bech32 parser.
fn parse_secret_key(key: &str) -> Result<([u8; 32], nostrdb_net::Pubkey)> {
    let key = key.trim();
    let hex_secret = (key.len() == 64)
        .then(|| hex::decode(key).ok())
        .flatten()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
    let Some(secret) = hex_secret else {
        return nostrdb_net::relay::sync::parse_nsec(key)
            .map_err(|e| format!("{e} (expected an nsec1… or a 64-char hex secret key)").into());
    };
    let sk = nostrdb_net::SecretKey::from_slice(&secret)
        .map_err(|e| format!("invalid hex secret key: {e}"))?;
    Ok((secret, nostrdb_net::Keypair::from_secret(sk).pubkey))
}

/// The environment variable naming a file that holds the comment key.
const COMMENT_NSEC_FILE_VAR: &str = "HEADWAY_COMMENT_NSEC_FILE";

/// The comment key's text, before parsing: `inline` (`--comment-nsec`, else
/// `$HEADWAY_COMMENT_NSEC`) when set, else the contents of the file `key_file`
/// (`$HEADWAY_COMMENT_NSEC_FILE`) names, else `None`.
///
/// The file is only read when nothing inline is set. A file that can't be read
/// is an error naming the variable and the path rather than a silent `None`:
/// falling back would sign the comment as the account, which is the one thing
/// setting the variable was meant to prevent.
fn comment_key_text(inline: Option<String>, key_file: Option<OsString>) -> Result<Option<String>> {
    if inline.is_some() {
        return Ok(inline);
    }
    let Some(path) = key_file else {
        return Ok(None);
    };
    std::fs::read_to_string(&path).map(Some).map_err(|e| {
        format!(
            "${COMMENT_NSEC_FILE_VAR}: can't read {}: {e}",
            path.to_string_lossy()
        )
        .into()
    })
}

/// Read a `--desc-file` value into description text: `-` reads stdin (so a long
/// markdown description can heredoc in with no shell-escaping), anything else is
/// a file path. Trailing whitespace is trimmed so a heredoc's closing newline
/// doesn't ride along.
fn read_desc_source(path: &str) -> Result<String> {
    use std::io::Read;
    let mut text = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(path)?;
    }
    Ok(text.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        match Cli::parse(args.iter().map(|s| s.to_string())).expect("parse ok") {
            Invocation::Run(cli) => *cli,
            _ => panic!("expected a command, got help"),
        }
    }

    /// The help page a command line asks for, or `None` when it asks to run.
    fn help_of(args: &[&str]) -> Option<Option<&'static str>> {
        match Cli::parse(args.iter().map(|s| s.to_string())).expect("parse ok") {
            Invocation::Run(_) => None,
            Invocation::Usage => Some(None),
            Invocation::CommandHelp(cmd) => Some(Some(cmd.name)),
        }
    }

    /// `--help` picks the overview or one command's page depending on whether a
    /// command was named, in either spelling and either order — and `--help`
    /// wins over the command's own argument checking, so `headway move --help`
    /// prints the page rather than erroring about the missing `--col`.
    #[test]
    fn help_resolves_to_a_page() {
        assert_eq!(help_of(&[]), Some(None));
        assert_eq!(help_of(&["--help"]), Some(None));
        assert_eq!(help_of(&["help"]), Some(None));
        assert_eq!(help_of(&["move", "--help"]), Some(Some("move")));
        assert_eq!(help_of(&["-h", "move"]), Some(Some("move")));
        assert_eq!(help_of(&["help", "move"]), Some(Some("move")));
        assert_eq!(help_of(&["help", "move-board"]), Some(Some("move-board")));
        // Without it, the same line runs (and here fails on its own missing flag).
        assert!(help_of(&["show"]).is_none());
    }

    /// `comment`'s review flags parse into where the comment goes: `--line`
    /// takes one line or an inclusive range, `--record` is shared with `diff`,
    /// and a malformed range is refused before any relay work.
    #[test]
    fn comment_review_flags_parse() {
        let cli = parse(&[
            "comment", "card", "--path", "src/a.rs", "--line", "3-5", "--old", "--record", "abc",
            "rename", "it",
        ]);
        let Command::Comment { body, review, .. } = cli.command else {
            panic!("expected a comment");
        };
        assert_eq!(body, "rename it");
        assert_eq!(review.path.as_deref(), Some("src/a.rs"));
        assert_eq!(review.lines, Some((3, 5)));
        assert!(review.old && review.any());
        assert_eq!(review.record.as_deref(), Some("abc"));

        let Command::Comment { review, .. } = parse(&["comment", "card", "hi"]).command else {
            panic!("expected a comment");
        };
        assert!(!review.any(), "no flags is a card comment");

        assert_eq!(parse_lines("42").unwrap(), (42, 42));
        for bad in ["0", "5-3", "a", "3-", "-3", ""] {
            assert!(parse_lines(bad).is_err(), "{bad:?}");
        }
    }

    /// An unrecognised command is rejected by name, whether or not help was
    /// asked for — `headway frobnicate --help` has no page to print.
    #[test]
    fn unknown_commands_are_rejected() {
        for args in [&["frobnicate"][..], &["frobnicate", "--help"][..]] {
            let err = parse_err(args);
            assert!(err.contains("unknown command 'frobnicate'"), "{err}");
        }
    }

    /// Every command the help documents is one the parser builds, so a help
    /// entry can't drift off a command that was renamed or removed. The
    /// converse — a command with no help entry — can't happen: `Cli::parse`
    /// resolves the name through the help table before dispatching.
    #[test]
    fn every_documented_command_parses() {
        for cmd in help::COMMANDS {
            let rest = ["a".to_string(), "b".to_string()];
            let res = parse_command(
                cmd.name,
                &rest,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
                SeqFlags::default(),
                false,
                None,
                ReviewFlags::default(),
                None,
                ReviewCommentFlags::default(),
            );
            // A command that wants a flag we didn't pass errors about *that*;
            // only the fallback arm means the name has no parser at all.
            if let Err(e) = res {
                assert!(
                    !e.to_string().contains("has no parser"),
                    "`{}` is documented but has no parser",
                    cmd.name
                );
            }
        }
    }

    /// A `headway:<board>/<word-id>` selector (or the scheme-less shorthand)
    /// routes the run to that board; a bare word-id or hex prefix doesn't.
    #[test]
    fn card_ref_routes_to_its_board() {
        assert_eq!(
            parse(&["show", "headway:commerce/purse-metal-toilet"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["move", "dave/stage-injury-surprise", "--col", "done"]).board,
            "dave"
        );
        // Case-normalised like the slugs themselves.
        assert_eq!(
            parse(&["show", "headway:Commerce/purse-metal-toilet"]).board,
            "commerce"
        );
    }

    /// `priority <card> <level>` parses into a Priority command, and a full
    /// card ref self-routes it to that card's board.
    #[test]
    fn priority_command_parses_and_routes() {
        let cli = parse(&["priority", "headway:commerce/purse-metal-toilet", "high"]);
        assert_eq!(cli.board, "commerce");
        match cli.command {
            Command::Priority { card, level } => {
                assert_eq!(card, "headway:commerce/purse-metal-toilet");
                assert_eq!(level, "high");
            }
            _ => panic!("expected a Priority command"),
        }
    }

    /// The labels of a `label` command, whatever spelling produced them.
    fn labels_of(cli: Cli) -> Vec<String> {
        match cli.command {
            Command::Label { labels, .. } => labels,
            _ => panic!("expected a Label command"),
        }
    }

    /// `label` takes its labels the same ways `add -l` does: separate
    /// positionals, one comma-separated positional, or the `-l`/`--label` flag —
    /// all equivalent. The flag case used to leave `rest` empty and so *clear*
    /// the card's labels instead of setting them.
    #[test]
    fn label_command_accepts_commas_and_the_label_flag() {
        let expected = ["a".to_string(), "b".to_string()];
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a", "b"])), expected);
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a,b"])), expected);
        assert_eq!(
            labels_of(parse(&["label", "deadbeef", "-l", "a,b"])),
            expected
        );
        assert_eq!(
            labels_of(parse(&["label", "deadbeef", "--label", "a", "-l", "b"])),
            expected
        );
        // Whitespace trimmed, empty entries dropped.
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a, ,b,"])), expected);
        // No labels at all still clears — the one case that must stay empty.
        assert!(labels_of(parse(&["label", "deadbeef"])).is_empty());
    }

    /// `add --desc <text>` folds the inline text into the card's description.
    #[test]
    fn add_desc_inline_sets_description() {
        let cli = parse(&["add", "a card", "--desc", "the details"]);
        match cli.command {
            Command::Add {
                title, description, ..
            } => {
                assert_eq!(title, "a card");
                assert_eq!(description.as_deref(), Some("the details"));
            }
            _ => panic!("expected an Add command"),
        }
    }

    /// `add --desc-file <path>` reads the description from a file, trimming the
    /// trailing whitespace a heredoc's closing newline leaves behind while
    /// keeping interior newlines.
    #[test]
    fn add_desc_file_is_read_and_trimmed() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "## Heading\n\nbody line\n\n").unwrap();
        let cli = parse(&["add", "a card", "--desc-file", f.path().to_str().unwrap()]);
        match cli.command {
            Command::Add { description, .. } => {
                assert_eq!(description.as_deref(), Some("## Heading\n\nbody line"))
            }
            _ => panic!("expected an Add command"),
        }
    }

    /// `--desc` and `--desc-file` name the same thing, so passing both errors
    /// rather than silently letting one win.
    #[test]
    fn add_desc_and_desc_file_conflict() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "from file").unwrap();
        let res = Cli::parse(
            [
                "add",
                "a card",
                "--desc",
                "inline",
                "--desc-file",
                f.path().to_str().unwrap(),
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let err = match res {
            Err(e) => e,
            Ok(_) => panic!("--desc + --desc-file should conflict"),
        };
        assert!(err.to_string().contains("not both"), "{err}");
    }

    /// `--all` is an independent flag on `show`, off unless passed.
    #[test]
    fn all_flag_toggles_show() {
        assert!(!parse(&["show"]).all);
        assert!(parse(&["show", "--all"]).all);
    }

    #[test]
    fn non_ref_selectors_do_not_route() {
        // These fall through to the usual precedence; with no env/config in a
        // test environment that may be the stored board, so only assert the
        // selector itself didn't force one by checking against a routed run.
        let routed = parse(&["show", "headway:commerce/purse-metal-toilet"]).board;
        assert_eq!(routed, "commerce");
        for sel in ["purse-metal-toilet", "2716e5db"] {
            let cli = parse(&["show", sel]);
            // Whatever board was picked, it wasn't derived from the selector.
            assert_eq!(cli.command.selector_board().unwrap(), None);
        }
    }

    /// Parse args expecting an error, returning its message.
    fn parse_err(args: &[&str]) -> String {
        match Cli::parse(args.iter().map(|s| s.to_string())) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected an error for {args:?}"),
        }
    }

    /// A fixed test secret (never a real key), in both accepted spellings.
    const TEST_SECRET: [u8; 32] = [7u8; 32];

    fn test_nsec() -> String {
        let hrp = bech32::Hrp::parse("nsec").expect("hrp");
        bech32::encode::<bech32::Bech32>(hrp, &TEST_SECRET).expect("encode nsec")
    }

    /// Hex and bech32 spellings of one secret yield the same secret and pubkey,
    /// and a key file's trailing newline (or surrounding spaces) is ignored.
    #[test]
    fn secret_key_accepts_hex_and_nsec() {
        let from_nsec = parse_secret_key(&test_nsec()).expect("nsec parses");
        let hex = hex::encode(TEST_SECRET);
        for spelling in [
            hex.clone(),
            format!("{hex}\n"),
            format!("  {}\n", hex.to_uppercase()),
        ] {
            let from_hex = parse_secret_key(&spelling).expect("hex parses");
            assert_eq!(from_hex.0, from_nsec.0, "{spelling:?}");
            assert_eq!(from_hex.1.bytes(), from_nsec.1.bytes(), "{spelling:?}");
        }
        assert_eq!(from_nsec.0, TEST_SECRET);
        let nsec_newline = parse_secret_key(&format!("{}\n", test_nsec())).expect("nsec\\n");
        assert_eq!(nsec_newline.1.bytes(), from_nsec.1.bytes());
    }

    /// `--nsec <hex>` reaches the same signer as `--nsec <nsec1…>` through the
    /// full argument parser, not just the helper.
    #[test]
    fn nsec_flag_accepts_hex() {
        let hex = hex::encode(TEST_SECRET);
        let via_hex = parse(&["--nsec", &hex, "show"]).secret.expect("signer");
        let via_nsec = parse(&["--nsec", &test_nsec(), "show"])
            .secret
            .expect("signer");
        assert_eq!(via_hex.0, via_nsec.0);
        assert_eq!(via_hex.1, via_nsec.1);
    }

    /// `--comment-nsec` takes the same spellings as `--nsec`, is kept apart from
    /// the signing key, and a malformed one is an error naming the flag.
    #[test]
    fn comment_key_parses_beside_the_signing_key() {
        let hex = hex::encode(TEST_SECRET);
        let cli = parse(&["--nsec", &test_nsec(), "--comment-nsec", &hex, "show"]);
        let (signing, _) = cli.secret.expect("signer");
        let (comment, _) = cli.comment_secret.expect("comment key");
        assert_eq!(signing, comment, "same key, two spellings");
        assert!(
            parse(&["--nsec", &test_nsec(), "show"])
                .comment_secret
                .is_none()
        );

        let err = match Cli::parse(
            ["--nsec", &test_nsec(), "--comment-nsec", "nope", "show"]
                .iter()
                .map(|s| s.to_string()),
        ) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a malformed comment key must be refused"),
        };
        assert!(err.contains("--comment-nsec"), "unexpected error: {err}");
    }

    /// A key file named by `$HEADWAY_COMMENT_NSEC_FILE` yields the same key as
    /// the inline spelling, trailing newline and all.
    #[test]
    fn comment_key_file_matches_inline_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nsec");
        let hex = hex::encode(TEST_SECRET);
        std::fs::write(&path, format!("{hex}\n")).expect("write key file");

        let from_file = comment_key_text(None, Some(path.into_os_string()))
            .expect("readable key file")
            .expect("a key");
        let (file_sk, file_pk) = parse_secret_key(&from_file).expect("file key parses");
        let (inline_sk, inline_pk) = parse_secret_key(&test_nsec()).expect("inline key parses");
        assert_eq!(file_sk, inline_sk);
        assert_eq!(file_pk.bytes(), inline_pk.bytes());
    }

    /// An inline key (`--comment-nsec` or `$HEADWAY_COMMENT_NSEC`) wins over the
    /// key file, which isn't even read then, and with neither there is no key.
    #[test]
    fn comment_key_inline_beats_key_file() {
        let missing = OsString::from("/nonexistent/headway-comment-nsec");
        let inline = comment_key_text(Some("inline".to_string()), Some(missing))
            .expect("the file is never read");
        assert_eq!(inline.as_deref(), Some("inline"));
        assert!(comment_key_text(None, None).expect("no key").is_none());
    }

    /// A key file that can't be read is an error naming the variable and the
    /// path, never a quiet fallback to signing as the account.
    #[test]
    fn comment_key_missing_file_names_the_variable() {
        let path = "/nonexistent/headway-comment-nsec";
        let err = comment_key_text(None, Some(OsString::from(path)))
            .expect_err("a missing key file must be refused")
            .to_string();
        assert!(err.contains(COMMENT_NSEC_FILE_VAR), "{err}");
        assert!(err.contains(path), "{err}");
    }

    /// Input that is neither 64 hex chars nor a valid nsec errors, and the
    /// message names both accepted forms so the fix is obvious.
    #[test]
    fn secret_key_rejects_malformed_input() {
        let hex = hex::encode(TEST_SECRET);
        let bad = [
            hex[..63].to_string(),
            format!("{}zz", &hex[..62]),
            "not a key".to_string(),
        ];
        for input in bad {
            let err = parse_secret_key(&input)
                .expect_err("should reject")
                .to_string();
            assert!(
                err.contains("nsec1") && err.contains("64-char hex"),
                "{input:?}: {err}"
            );
        }
        // Well-formed hex that isn't a valid secp256k1 secret (zero) is refused too.
        assert!(parse_secret_key(&"0".repeat(64)).is_err());
    }

    #[test]
    fn conflicting_refs_error() {
        let err = parse_err(&["show", "commerce/a-b-c", "dave/d-e-f"]);
        assert!(err.contains("different boards"), "{err}");

        let err = parse_err(&["--board", "headway", "show", "commerce/a-b-c"]);
        assert!(err.contains("conflicts"), "{err}");
    }

    /// `--board` agreeing with the ref is fine, and two refs naming the same
    /// board are too.
    #[test]
    fn agreeing_refs_are_fine() {
        assert_eq!(
            parse(&["--board", "commerce", "show", "commerce/a-b-c"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["show", "commerce/a-b-c", "commerce/d-e-f"]).board,
            "commerce"
        );
    }

    /// `next --in <headway:board/word-id>` self-routes to the ref's board and marks
    /// the board explicit, and `--ready`/`-n` parse into the command.
    #[test]
    fn next_parses_and_routes() {
        let cli = parse(&[
            "next",
            "--in",
            "headway:notedeck/saddle-because-liquid",
            "--ready",
            "-n",
            "3",
        ]);
        assert_eq!(cli.board, "notedeck");
        assert!(cli.board_explicit);
        match cli.command {
            Command::Next {
                container,
                ready,
                limit,
            } => {
                assert_eq!(
                    container.as_deref(),
                    Some("headway:notedeck/saddle-because-liquid")
                );
                assert!(ready);
                assert_eq!(limit, Some(3));
            }
            _ => panic!("expected a Next command"),
        }
    }

    /// `--board` marks the board explicit for the statelessness guard; a bare
    /// `next` (no ref, no `--board`) does not.
    #[test]
    fn next_board_explicitness() {
        assert!(parse(&["next", "--board", "headway"]).board_explicit);
        assert!(!parse(&["next"]).board_explicit);
        assert!(!parse(&["next", "--in", "headway"]).board_explicit);
    }

    /// `migrate` seals live data under a fresh key, so it must name its board
    /// explicitly — a bare `migrate` is not `board_explicit`, so dispatch rejects
    /// it rather than sealing the persisted current board.
    #[test]
    fn migrate_board_explicitness() {
        assert!(!parse(&["migrate"]).board_explicit);
        assert!(parse(&["migrate", "--board", "commerce"]).board_explicit);
    }

    /// `share` takes its recipient as an npub or as hex, and names the key it
    /// couldn't read rather than failing later at wrap time.
    #[test]
    fn share_parses_npub_and_hex_recipients() {
        let hex = "0ca678de0a151cc2425631f23605c2edee96d4723a3d06582bfff13311e52cb6";
        let npub = Pubkey::from_hex(hex).unwrap().npub().unwrap();
        for given in [hex, npub.as_str()] {
            match parse(&["share", given, "--board", "commerce"]).command {
                Command::Share { recipient } => assert_eq!(recipient.hex(), hex),
                _ => panic!("expected a Share command"),
            }
        }
        let err = parse_err(&["share", "not-a-key"]);
        assert!(err.contains("not an npub or hex pubkey"), "{err}");
        let err = parse_err(&["share"]);
        assert!(err.contains("missing an argument"), "{err}");
    }

    #[test]
    fn ref_board_shapes() {
        // Full scheme form and scheme-less shorthand both name their board.
        assert_eq!(ref_board("headway:commerce/a-b-c"), Some("commerce".into()));
        assert_eq!(ref_board("commerce/a-b-c"), Some("commerce".into()));
        assert_eq!(ref_board("ios-port/a-b-c"), Some("ios-port".into()));
        // A bare word-id, hex, or empty segment is not a reference.
        assert_eq!(ref_board("a-b-c"), None);
        assert_eq!(ref_board("/a-b-c"), None);
        assert_eq!(ref_board("commerce/"), None);
        assert_eq!(ref_board("not a slug/a-b-c"), None);
    }
}
