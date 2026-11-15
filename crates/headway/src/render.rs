//! The human text view of a board: the board listing, a card's detail, the
//! `next` frontier, the board list, and the decline messages an edit reports.
//!
//! Every function returns its text rather than printing it, so the `headway`
//! CLI and the app's tools (which `nd` prints) show one rendering. `color`
//! says whether the muted parts (refs, ages, counts) carry an ANSI
//! bright-black: the CLI passes whether its stdout is a terminal, and a tool
//! answering over the socket renders with color on and leaves stripping it to
//! the client, which is the one that knows where the text is going.

use nostrdb_net::{NoteId, Pubkey};

use crate::event::{
    BoardView, CardView, CommentView, Container, EdgeRef, LineSide, Priority, ReviewCommentView,
    ReviewLocation, ReviewView,
};
use crate::store::{self, DeclineReason};
use crate::traversal::{self, NotReady};
use crate::wordid;

/// Text being built, with whether its muted parts are colored.
struct Text {
    out: String,
    color: bool,
}

impl Text {
    fn new(color: bool) -> Self {
        Self {
            out: String::new(),
            color,
        }
    }

    /// `s` muted, see [`dim`].
    fn dim(&self, s: &str) -> String {
        dim(self.color, s)
    }

    /// Append `s` and a newline.
    fn line(&mut self, s: impl AsRef<str>) {
        self.out.push_str(s.as_ref());
        self.out.push('\n');
    }

    /// Append an empty line.
    fn blank(&mut self) {
        self.out.push('\n');
    }
}

/// `s` in ANSI bright-black when `color`, else as is. Bright-black rather than
/// the SGR dim attribute because many terminals (urxvt among them) ignore dim,
/// while the 16-color palette is honored everywhere.
pub fn dim(color: bool, s: &str) -> String {
    if color {
        format!("\x1b[90m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Live (non-archived) card count across a board's columns.
pub fn card_count(view: &BoardView) -> usize {
    view.columns.iter().map(|c| c.cards.len()).sum()
}

/// One board as the board list shows it: its slug, title and live card count.
#[derive(Clone, Copy)]
pub struct BoardRow<'a> {
    /// `*` for the current board, else a space.
    pub mark: char,
    pub id: &'a str,
    pub title: &'a str,
    pub cards: usize,
    /// The owner's key, for a board shared with us.
    pub owner: Option<&'a [u8; 32]>,
}

impl<'a> BoardRow<'a> {
    /// `view` as a row, unmarked and without its owner.
    pub fn of(view: &'a BoardView) -> Self {
        Self {
            mark: ' ',
            id: &view.id,
            title: &view.title,
            cards: card_count(view),
            owner: None,
        }
    }
}

/// One board-list line: `<mark> <slug>  <title · n cards[ · owner …]>`.
pub fn board_row(row: BoardRow, color: bool) -> String {
    let mut detail = format!("{} · {} cards", row.title, row.cards);
    if let Some(owner) = row.owner {
        detail.push_str(&format!(" · owner {}", short_owner(owner)));
    }
    format!("{} {}  {}\n", row.mark, row.id, dim(color, &detail))
}

/// The board list: our own boards, then the ones `shared` with us under their
/// own heading, each owner shown so two same-slug boards tell apart. `current`
/// is marked with a `*`: on our own board by that slug, else on the one shared
/// board it resolves to. Falls back to a hint when there are no boards at all.
pub fn boards(own: &[BoardView], shared: &[BoardView], current: &str, color: bool) -> String {
    let mut t = Text::new(color);
    if own.is_empty() && shared.is_empty() {
        t.line(format!(
            "no boards yet — current selection is '{current}'. Run `nd headway seed` to create it."
        ));
        return t.out;
    }
    let own_current = own.iter().any(|v| v.id == current);
    for view in own {
        let mark = if view.id == current { '*' } else { ' ' };
        t.out.push_str(&board_row(
            BoardRow {
                mark,
                ..BoardRow::of(view)
            },
            color,
        ));
    }
    // The shared board `--board <current>` resolves to, if exactly one does;
    // several are ambiguous, so none is marked.
    let shared_matches = shared.iter().filter(|v| v.id == current).count();
    let shared_current = !own_current && shared_matches == 1;
    let unresolved = !own_current && shared_matches == 0;
    if unresolved {
        let hint = t.dim("(not created yet — run `nd headway seed`)");
        t.line(format!("* {current}  {hint}"));
    }
    if shared.is_empty() {
        return t.out;
    }
    if !own.is_empty() || unresolved {
        t.blank();
    }
    t.line("shared with me");
    for view in shared {
        let mark = if shared_current && view.id == current {
            '*'
        } else {
            ' '
        };
        t.out.push_str(&board_row(
            BoardRow {
                mark,
                owner: Some(&view.author),
                ..BoardRow::of(view)
            },
            color,
        ));
    }
    t.out
}

/// The tail of `nd headway board`: the deleted boards, by slug, so a restore can
/// name them. Empty when there are none.
pub fn deleted_boards(deleted: &[BoardView], color: bool) -> String {
    let mut t = Text::new(color);
    if deleted.is_empty() {
        return t.out;
    }
    let hint = t.dim("(`nd headway restore-board <board>` brings one back)");
    t.line(format!("\ndeleted {hint}"));
    for view in deleted {
        let detail = t.dim(&format!("{} · {} cards", view.title, card_count(view)));
        t.line(format!("  {}  {detail}", view.id));
    }
    t.out
}

/// Why an edit to a deleted board is refused, and how to undo the delete.
pub fn deleted_board_error(board: &str) -> String {
    format!("board '{board}' is deleted — restore it with `nd headway restore-board {board}` first")
}

/// A board owner as a short npub (`npub1pjn83hs2…`), enough to tell owners
/// apart in a listing.
fn short_owner(owner: &[u8; 32]) -> String {
    let pk = Pubkey::new(*owner);
    let full = pk.npub().unwrap_or_else(|| pk.hex());
    format!("{}…", &full[..full.len().min(14)])
}

/// A board as `nd headway show` prints it: its title, description, then each
/// column with its cards, one line a card. Archived cards are counted, and
/// listed when `show_archived`.
pub fn board(view: &BoardView, show_archived: bool, color: bool) -> String {
    let mut t = Text::new(color);
    t.line(&view.title);
    if view.deleted {
        let hint = t.dim(&format!(
            "(deleted — `nd headway restore-board {}` brings it back)",
            view.id
        ));
        t.line(hint);
    }
    if !view.description.is_empty() {
        t.line(&view.description);
    }
    for col in &view.columns {
        t.line(format!("\n{} ({})", col.name, col.cards.len()));
        for c in &col.cards {
            let line = format!(
                "  {}{}{}{}{}  {}",
                blocked_prefix(c, color),
                priority_prefix(c.priority, color),
                c.title,
                progress_suffix(c, color),
                labels_suffix(&c.labels),
                card_ref(view, &c.id, color),
            );
            t.line(line);
        }
    }
    if !view.archived.is_empty() {
        if show_archived {
            t.line(format!("\nArchived ({})", view.archived.len()));
            for a in &view.archived {
                let line = format!("  {}  {}", a.card.title, card_ref(view, &a.card.id, color));
                t.line(line);
            }
        } else {
            t.line(format!(
                "\nArchived ({}) — use `show --archived` to list",
                view.archived.len()
            ));
        }
    }
    t.out
}

/// Every board in `boards`, each as [`board`] renders it under its slug,
/// separated by a blank line. A board someone else owns (one shared with `me`)
/// is headed with its owner too, since slugs are only unique per owner.
pub fn all_boards(boards: &[BoardView], me: &[u8; 32], show_archived: bool, color: bool) -> String {
    let mut t = Text::new(color);
    if boards.is_empty() {
        t.line("no boards yet — run `nd headway seed` to create one");
        return t.out;
    }
    for (i, view) in boards.iter().enumerate() {
        if i > 0 {
            t.blank();
        }
        // Lead with the addressable slug: board titles can collide (two boards
        // both titled "Headway"), and the slug is what `--board`/`board <id>`
        // take, so it's the anchor for the human-readable title that follows.
        let heading = if &view.author == me {
            view.id.clone()
        } else {
            format!("{} · shared by {}", view.id, short_owner(&view.author))
        };
        let heading = t.dim(&heading);
        t.line(heading);
        t.out.push_str(&board(view, show_archived, color));
    }
    t.out
}

/// A card on a board with where it sits, as [`placed_cards`] yields it.
#[derive(Clone, Copy)]
pub struct PlacedCard<'a> {
    pub card: &'a CardView,
    /// Its column's name, or `archived` (the word `show <card>` uses) for an
    /// archived card.
    pub column: &'a str,
}

/// Every card on `view`, live then archived, each with where it sits.
pub fn placed_cards(view: &BoardView) -> impl Iterator<Item = PlacedCard<'_>> {
    let live = view.columns.iter().flat_map(|col| {
        col.cards.iter().map(move |card| PlacedCard {
            card,
            column: col.name.as_str(),
        })
    });
    let archived = view.archived.iter().map(|a| PlacedCard {
        card: &a.card,
        column: "archived",
    });
    live.chain(archived)
}

/// Find a card by id anywhere on the board, with where it sits.
pub fn find_card<'a>(view: &'a BoardView, id: &NoteId) -> Option<PlacedCard<'a>> {
    placed_cards(view).find(|p| p.card.id == *id)
}

/// An array of `cards` as
/// [`card_detail_json`](crate::event::card_detail_json), each with the
/// `column` it sits in: what the app's `headway_show_card` tool answers with.
pub fn cards_json(view: &BoardView, cards: &[PlacedCard]) -> serde_json::Value {
    cards
        .iter()
        .map(|p| {
            let mut j = crate::event::card_detail_json(&view.id, p.card);
            j["column"] = serde_json::json!(p.column);
            j
        })
        .collect()
}

/// One card in `git show` style: a header block of metadata, then the title
/// and description indented underneath, then its subissues, edges, review
/// records and comments. `column` is where it sits.
pub fn card_detail(view: &BoardView, card: &CardView, column: &str, color: bool) -> String {
    let mut t = Text::new(color);
    t.line(format!("card    {}", card_ref(view, &card.id, color)));
    t.line(format!("id      {}", card.id.hex()));
    t.line(format!("column  {column}"));
    if !card.labels.is_empty() {
        t.line(format!("labels  {}", card.labels.join(", ")));
    }
    if card.priority != Priority::None {
        t.line(format!("priority {}", card.priority.as_str()));
    }
    if let Some(due) = card.due {
        t.line(format!("due     {due}"));
    }
    if let Some(estimate) = card.estimate {
        t.line(format!("estimate {estimate}"));
    }
    if let Some(parent) = &card.parent {
        t.line(format!("parent  {}", card_ref(view, parent, color)));
    }
    t.line(format!("created {}", crate::fmt::rel_time(card.created_at)));
    if card.updated_at > card.created_at {
        t.line(format!("updated {}", crate::fmt::rel_time(card.updated_at)));
    }

    t.line(format!("\n    {}", card.title));
    if !card.description.is_empty() {
        t.blank();
        for line in card.description.lines() {
            if line.is_empty() {
                t.blank();
            } else {
                t.line(format!("    {line}"));
            }
        }
    }

    if !card.subissues.is_empty() {
        let done = card.subissues.iter().filter(|s| s.done).count();
        t.line(format!(
            "\nsubissues ({done}/{} done)",
            card.subissues.len()
        ));
        for s in &card.subissues {
            let mark = if s.done { "x" } else { " " };
            // Where the child sits, when we know: its column, or "archived".
            let place = if s.archived {
                Some("archived".to_string())
            } else {
                s.column.clone()
            };
            let place = place.map_or(String::new(), |p| format!("  {}", t.dim(&format!("({p})"))));
            let line = format!(
                "    [{mark}] {}{place}  {}",
                s.title,
                card_ref(view, &s.id, color)
            );
            t.line(line);
        }
    }

    edges(&mut t, view, "blocked by", &card.blocked_by);
    edges(&mut t, view, "blocks", &card.blocks);
    related(&mut t, view, &card.related);

    if !card.reviews.is_empty() {
        t.line(format!("\nreview ({})", card.reviews.len()));
        for r in &card.reviews {
            review(&mut t, r);
        }
    }

    if !card.comments.is_empty() {
        t.line(format!("\ncomments ({})", card.comments.len()));
        for c in &card.comments {
            comment(&mut t, c);
        }
    }
    t.out
}

/// One comment in the card-detail thread: an author/time header (with the
/// comment's own word-id so it can be `--reply-to`'d), then its body indented
/// beneath. Replies are flagged inline but still rendered flat.
fn comment(t: &mut Text, c: &CommentView) {
    let mut header = format!(
        "    {}  {}  {}",
        crate::fmt::short_author(&c.author),
        t.dim(&crate::fmt::rel_time(c.created_at)),
        t.dim(&wordid::encode(c.id.bytes())),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&t.dim(&format!("  ↳ reply to {}", wordid::encode(parent.bytes()))));
    }
    t.line(format!("\n{header}"));
    for line in c.body.lines() {
        if line.is_empty() {
            t.blank();
        } else {
            t.line(format!("        {line}"));
        }
    }
}

/// One review record in the card-detail view, newest first as
/// [`CardView::reviews`] holds them: the short sha and subject, then where the
/// commit lives (`host:path (branch)`), the session, the explainer and the
/// deploy, each on its own line and only when recorded.
fn review(t: &mut Text, r: &ReviewView) {
    let f = &r.fields;
    let sha = f
        .commit
        .as_deref()
        .map_or("-------", |c| c.get(..7).unwrap_or(c));
    let age = t.dim(&crate::fmt::rel_time(r.created_at));
    t.line(format!(
        "    {sha}  {}  {age}",
        f.title.as_deref().unwrap_or("")
    ));
    let place = match (f.host.as_deref(), f.path.as_deref()) {
        (Some(host), Some(path)) => Some(format!("{host}:{path}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_string()),
        (None, None) => None,
    };
    if let Some(place) = place {
        let branch = f
            .branch
            .as_deref()
            .map_or(String::new(), |b| format!(" ({b})"));
        t.line(format!("        {place}{branch}"));
    }
    for line in [
        f.agentium.as_deref(),
        f.explainer.as_deref(),
        f.deploy.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        t.line(format!("        {line}"));
    }
    review_comments(t, r);
}

/// A review record's inline comments under it: a count, then each top-level
/// comment with its replies nested beneath (see [`review_comment`]). A reply
/// whose parent isn't on this record shows as top-level rather than vanishing.
fn review_comments(t: &mut Text, r: &ReviewView) {
    let n = r.comments.len();
    if n == 0 {
        return;
    }
    let noun = if n == 1 { "comment" } else { "comments" };
    let count = t.dim(&format!("{n} review {noun}"));
    t.line(format!("        {count}"));
    let on_record = |id: &NoteId| r.comments.iter().any(|c| c.id == *id);
    for c in r
        .comments
        .iter()
        .filter(|c| !c.parent.as_ref().is_some_and(on_record))
    {
        t.blank();
        review_comment(t, r, c, None, 0);
    }
}

/// Review comment `c` at `depth` levels of nesting, then its replies one level
/// deeper, oldest first as [`ReviewView::comments`] holds them. The header
/// carries the author, age and the comment's own word-id (what `comment
/// --reply-to` takes), then where it points ([`review_place`]) unless that's
/// where its parent points too.
fn review_comment(
    t: &mut Text,
    r: &ReviewView,
    c: &ReviewCommentView,
    parent: Option<&ReviewCommentView>,
    depth: usize,
) {
    let indent = " ".repeat(8 + 4 * depth);
    let mut header = format!(
        "{indent}{}{}  {}  {}",
        if depth > 0 { "↳ " } else { "" },
        crate::fmt::short_author(&c.author),
        t.dim(&crate::fmt::rel_time(c.created_at)),
        t.dim(&wordid::encode(c.id.bytes())),
    );
    if let Some(loc) = &c.location
        && parent.and_then(|p| p.location.as_ref()) != Some(loc)
    {
        header.push_str("  ");
        header.push_str(&review_place(loc, r.fields.commit.as_deref()));
    }
    t.line(header);
    for line in c.body.lines() {
        if line.is_empty() {
            t.blank();
        } else {
            t.line(format!("{indent}    {line}"));
        }
    }
    for reply in r.comments.iter().filter(|x| x.parent == Some(c.id)) {
        review_comment(t, r, reply, Some(c), depth + 1);
    }
}

/// Where a review comment points, as `show` prints it: `path:42` or
/// `path:42-48` on the new side, `path:old 42-48` on the old (deleted) side,
/// plus `@<short sha>` when the lines are on a commit other than the record's
/// own `record_commit`.
fn review_place(loc: &ReviewLocation, record_commit: Option<&str>) -> String {
    let side = match loc.side {
        LineSide::New => "",
        LineSide::Old => "old ",
    };
    let mut place = format!("{}:{side}{}", loc.path, loc.line_value());
    if record_commit != Some(loc.commit.as_str()) {
        let short = loc.commit.get(..7).unwrap_or(&loc.commit);
        place.push_str(&format!(" @{short}"));
    }
    place
}

/// A card-detail dependency section (`blocked by` / `blocks`): one line per
/// edge, an `x` marking a cleared (done/archived) blocker so an open one stands
/// out. Nothing when there are no edges.
fn edges(t: &mut Text, view: &BoardView, label: &str, edges: &[EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    t.line(format!("\n{label} ({})", edges.len()));
    for e in edges {
        let mark = if e.done { "x" } else { " " };
        let line = format!(
            "    [{mark}] {}  {}",
            e.title,
            card_ref(view, &e.id, t.color)
        );
        t.line(line);
    }
}

/// A card-detail `related` section: one `<title>  <ref>` line per edge. Unlike
/// [`edges`] there is no cleared marker: the relation is undirected and
/// semantics-free ("see also"), so a partner's doneness is irrelevant.
fn related(t: &mut Text, view: &BoardView, edges: &[EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    t.line(format!("\nrelated ({})", edges.len()));
    for e in edges {
        let line = format!("    {}  {}", e.title, card_ref(view, &e.id, t.color));
        t.line(line);
    }
}

/// One line of `next`'s frontier: the card's ref, plain so an agent can copy
/// it, then its title muted.
pub fn ready_line(card_ref: &str, title: &str, color: bool) -> String {
    format!("{card_ref}  {}\n", dim(color, title))
}

/// The text of `card`'s review source named `name`, exactly as stored, for
/// `show <card> --source <name>`. The newest record that carries a file of
/// that name wins, so a later record without sources doesn't hide the
/// explainer an earlier one stored.
pub fn review_source<'c>(card: &'c CardView, name: &str) -> Result<&'c str, String> {
    card.reviews
        .iter()
        .find_map(|r| r.sources.iter().find(|s| s.name == name))
        .map(|s| s.text.as_str())
        .ok_or_else(|| format!("no review source named '{name}' on '{}'", card.title))
}

/// One card `next` hands out, as `next --json` prints each: its ref, hex id
/// and title.
#[derive(serde::Serialize, Debug)]
pub struct ReadyCard {
    /// The card's `headway:<board>/<word-id>` ref, ready to paste into other
    /// commands.
    #[serde(rename = "ref")]
    pub card_ref: String,
    /// The card's hex event id.
    pub id: String,
    pub title: String,
}

/// What `next` hands out from `container`'s ready frontier.
///
/// Which: `labels` keeps only cards carrying every one of them, applied
/// before the count, so `-l x -n 1` is the first ready `x` card rather than
/// the first ready card if it happens to be `x`. How many: `limit` caps it;
/// otherwise `all` (`--ready`) is the whole frontier, the parallel-dispatch
/// set, and the default is just the single next card.
pub fn next_cards(
    view: &BoardView,
    container: &Container,
    labels: &[String],
    all: bool,
    limit: Option<usize>,
) -> Vec<ReadyCard> {
    let frontier = traversal::ready_labelled(view, container, labels);
    let count = match (all, limit) {
        (_, Some(k)) => k,
        (true, None) => frontier.len(),
        (false, None) => 1,
    };
    frontier
        .into_iter()
        .take(count)
        .map(|c| ReadyCard {
            card_ref: plain_ref(view, &c.id),
            id: c.id.hex(),
            title: c.title.clone(),
        })
        .collect()
}

/// [`next_cards`] as text: one [`ready_line`] each, and nothing at all for an
/// empty frontier, so `$(headway next)` is empty when nothing is ready.
pub fn next_lines(cards: &[ReadyCard], color: bool) -> String {
    cards
        .iter()
        .map(|c| ready_line(&c.card_ref, &c.title, color))
        .collect()
}

/// A neighbour in [`SeqPlace`]: the card just before or after.
#[derive(serde::Serialize, Debug)]
pub struct SeqNeighbour {
    #[serde(rename = "ref")]
    pub card_ref: String,
    pub title: String,
}

/// Where a card sits in a container's work-order: `seq <card>` with no
/// position flag. It serializes as `seq --json` prints it and renders with
/// [`SeqPlace::text`].
///
/// Both counts are kept because they answer different questions: "31 of 40"
/// is the place in the order, "22 ready ahead" is how many cards `next` would
/// hand out first.
#[derive(serde::Serialize, Debug)]
pub struct SeqPlace {
    #[serde(rename = "ref")]
    pub card_ref: String,
    #[serde(skip)]
    pub title: String,
    /// The container: a card ref, or the board slug for the board root.
    pub container: String,
    /// The container as the text names it: `<slug> (board root)` or a ref.
    #[serde(skip)]
    pub in_name: String,
    pub index: usize,
    pub of: usize,
    pub sequenced: bool,
    pub before: Option<SeqNeighbour>,
    pub after: Option<SeqNeighbour>,
    pub ready_ahead: usize,
    pub ready: bool,
    pub not_ready_why: Option<String>,
}

/// Where `card` sits in `container`'s work-order; an error when the walk
/// never reaches it.
pub fn seq_place(
    view: &BoardView,
    container: &Container,
    card: NoteId,
) -> Result<SeqPlace, String> {
    let container_ref = match container {
        Container::BoardRoot(slug) => slug.clone(),
        Container::Card(id) => plain_ref(view, &NoteId::new(*id)),
    };
    let in_name = match container {
        Container::BoardRoot(_) => format!("{container_ref} (board root)"),
        Container::Card(_) => container_ref.clone(),
    };
    let place = traversal::place(view, container, card).ok_or_else(|| {
        format!(
            "{} is not in the work order of {in_name}",
            plain_ref(view, &card)
        )
    })?;
    let title = |id: NoteId| view.card(id).map_or("", |c| c.title.as_str()).to_owned();
    let neighbour = |id: Option<NoteId>| {
        id.map(|id| SeqNeighbour {
            card_ref: plain_ref(view, &id),
            title: title(id),
        })
    };
    let not_ready_why = place.not_ready.map(|r| match r {
        NotReady::Done => "done".to_string(),
        NotReady::InProgress => "In Progress".to_string(),
        NotReady::Blocked { by } => format!("blocked by {}", plain_ref(view, &by)),
        NotReady::OpenSubissues => "has open subissues".to_string(),
    });
    Ok(SeqPlace {
        card_ref: plain_ref(view, &card),
        title: title(card),
        container: container_ref,
        in_name,
        index: place.index,
        of: place.of,
        sequenced: place.sequenced,
        before: neighbour(place.before),
        after: neighbour(place.after),
        ready_ahead: place.ready_ahead,
        ready: place.ready(),
        not_ready_why,
    })
}

impl SeqPlace {
    /// The place as `seq <card>` prints it: the card, then its order, rank,
    /// neighbours and readiness, one labelled line each.
    pub fn text(&self, color: bool) -> String {
        let neighbour = |n: &Option<SeqNeighbour>, end: &str| match n {
            Some(n) => format!("{}  {}", n.card_ref, dim(color, &n.title)),
            None => dim(color, end),
        };
        let mut t = Text::new(color);
        t.line(format!("{}  {}", self.card_ref, t.dim(&self.title)));
        t.line(format!(
            "  order   {} of {} in {}",
            self.index, self.of, self.in_name
        ));
        t.line(format!(
            "  rank    {}",
            if self.sequenced {
                "sequenced"
            } else {
                "unsequenced: placed by when it was made"
            }
        ));
        t.line(format!("  before  {}", neighbour(&self.before, "(first)")));
        t.line(format!("  after   {}", neighbour(&self.after, "(last)")));
        t.line(format!(
            "  ready   {} ready ahead; {}",
            self.ready_ahead,
            match &self.not_ready_why {
                None => "this one is ready".to_string(),
                Some(why) => format!("not ready: {why}"),
            }
        ));
        t.out
    }
}

/// A card's `headway:<board>/<word-id>` ref, undimmed: unlike the muted ref a
/// listing draws beside a title, this is the thing an agent copies.
pub fn plain_ref(view: &BoardView, id: &NoteId) -> String {
    wordid::card_ref(&view.id, id.bytes())
}

/// A card's `headway:<board>/<word-id>` ref, muted so the title stays the
/// eye's anchor. Just a rendering of the event id; see [`wordid`].
fn card_ref(view: &BoardView, id: &NoteId, color: bool) -> String {
    dim(color, &plain_ref(view, id))
}

/// A card's labels as `  [a, b]` after its title, empty when it has none.
pub fn labels_suffix(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", labels.join(", "))
    }
}

/// The at-a-glance priority marker before a card's title on the board
/// listing: muted for the lower priorities, plain for urgent, empty for
/// [`Priority::None`] so unprioritised cards stay unadorned.
fn priority_prefix(priority: Priority, color: bool) -> String {
    match priority {
        Priority::None => String::new(),
        Priority::Urgent => priority_glyph(priority).to_string(),
        _ => dim(color, priority_glyph(priority)),
    }
}

/// The marker [`priority_prefix`] draws for `priority`, unstyled, trailing
/// space included: `↓ `, `= `, `↑ `, `! `, or empty for [`Priority::None`].
pub fn priority_glyph(priority: Priority) -> &'static str {
    match priority {
        Priority::None => "",
        Priority::Low => "↓ ",
        Priority::Medium => "= ",
        Priority::High => "↑ ",
        Priority::Urgent => "! ",
    }
}

/// A muted `n/m` subissue rollup after a parent card's title on the board
/// listing; empty for cards with no children.
fn progress_suffix(card: &CardView, color: bool) -> String {
    if card.subissues.is_empty() {
        return String::new();
    }
    let done = card.subissues.iter().filter(|s| s.done).count();
    format!(
        "  {}",
        dim(color, &format!("{done}/{}", card.subissues.len()))
    )
}

/// The marker [`blocked_prefix`] draws for a blocked card, unstyled.
pub const BLOCKED_GLYPH: &str = "⊘ ";

/// A muted ⊘ flagging a card as blocked (held back by an unfinished blocker) on
/// the board listing; empty when the card is free to work on.
fn blocked_prefix(card: &CardView, color: bool) -> String {
    if card.is_blocked() {
        dim(color, BLOCKED_GLYPH)
    } else {
        String::new()
    }
}

/// Say what the reducer declined, in words the caller can act on. An
/// idempotent no-op ([`store::DeclineReason::is_noop`]) reads as a statement
/// of the board's current shape (the caller's ask is already true), while a
/// refusal names the invariant it would have broken. Never muted: unlike a
/// listing, this is the whole message, not a reference beside a title.
pub fn declined_message(view: &BoardView, declined: &store::Declined) -> String {
    let card = plain_ref(view, &declined.card);
    let other = plain_ref(view, &declined.other);
    match declined.reason {
        DeclineReason::AlreadyBlocked => format!("{card} is already blocked on {other}"),
        DeclineReason::NotBlocked => format!("{card} isn't blocked on {other}"),
        DeclineReason::AlreadyRelated => format!("{card} is already related to {other}"),
        DeclineReason::NotRelated => format!("{card} isn't related to {other}"),
        // A self-block is the degenerate cycle; naming it as one reads as a typo
        // report rather than a graph puzzle.
        DeclineReason::BlockCycle if declined.card == declined.other => {
            format!("{card} can't block itself")
        }
        DeclineReason::BlockCycle => format!(
            "blocking {card} on {other} would create a dependency cycle \
             ({other} is already blocked by {card})"
        ),
        DeclineReason::ParentCycle if declined.card == declined.other => {
            format!("{card} can't be its own parent")
        }
        DeclineReason::ParentCycle => format!(
            "parenting {card} under {other} would create a parent cycle \
             ({other} is a descendant of {card}, or isn't a card on this board)"
        ),
        DeclineReason::SelfRelation => format!("{card} can't be related to itself"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ArchivedCard, ColumnView, ReviewFields, SubissueView};

    /// The instant every fixture is stamped with, and the one [`rel_time`]
    /// is frozen to, so ages render as "just now" or a fixed offset.
    ///
    /// [`rel_time`]: crate::fmt::rel_time
    const NOW: u64 = 1_700_000_000;

    /// A bare card `n` titled `title`, created at [`NOW`].
    fn card(n: u8, title: &str) -> CardView {
        CardView {
            id: NoteId::new([n; 32]),
            author: [7; 32],
            title: title.to_string(),
            description: String::new(),
            labels: vec![],
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: "m".to_string(),
            seq: None,
            placed_at: 0,
            created_at: NOW,
            updated_at: NOW,
            comments: vec![],
            reviews: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
            blocked_by: vec![],
            blocks: vec![],
            related: vec![],
        }
    }

    /// A board `b` with a Todo and a Done column holding `todo` and `done`.
    fn board_of(todo: Vec<CardView>, done: Vec<CardView>) -> BoardView {
        BoardView {
            id: "b".to_string(),
            author: [7; 32],
            title: "The board".to_string(),
            description: String::new(),
            created_at: NOW,
            columns: vec![
                ColumnView {
                    id: "todo".to_string(),
                    name: "Todo".to_string(),
                    terminal: false,
                    cards: todo,
                },
                ColumnView {
                    id: "done".to_string(),
                    name: "Done".to_string(),
                    terminal: true,
                    cards: done,
                },
            ],
            archived: vec![],
            deleted: false,
        }
    }

    fn r(n: u8) -> String {
        wordid::card_ref("b", &[n; 32])
    }

    /// `nd headway show`: the title, each column with its count, one line a
    /// card carrying its blocked/priority markers, rollup, labels and ref,
    /// and the archived count with the hint to list them.
    #[test]
    fn board_lists_columns_and_cards() {
        let mut epic = card(1, "Epic");
        epic.labels = vec!["epic".to_string(), "cli".to_string()];
        epic.priority = Priority::High;
        epic.subissues = vec![SubissueView {
            id: NoteId::new([2; 32]),
            title: "Child".to_string(),
            column: Some("done".to_string()),
            done: true,
            archived: false,
            seq: None,
        }];
        let mut blocked = card(3, "Blocked");
        blocked.blocked_by = vec![EdgeRef {
            id: NoteId::new([1; 32]),
            title: "Epic".to_string(),
            done: false,
        }];
        let mut view = board_of(vec![epic, blocked], vec![card(2, "Child")]);
        view.description = "What it's for".to_string();
        view.archived = vec![ArchivedCard {
            card: card(4, "Old"),
            from: None,
        }];

        let text = board(&view, false, false);
        assert_eq!(
            text,
            format!(
                "The board\nWhat it's for\n\nTodo (2)\n  ↑ Epic  1/1  [epic, cli]  {}\n  ⊘ Blocked  {}\n\nDone (1)\n  Child  {}\n\nArchived (1) — use `show --archived` to list\n",
                r(1),
                r(3),
                r(2)
            )
        );
        let archived = board(&view, true, false);
        assert!(
            archived.ends_with(&format!("\nArchived (1)\n  Old  {}\n", r(4))),
            "{archived}"
        );
    }

    /// With color on, the muted parts are bright-black and the title is not.
    #[test]
    fn color_mutes_refs_only() {
        let view = board_of(vec![card(1, "One")], vec![]);
        let text = board(&view, false, true);
        assert!(
            text.contains(&format!("  One  \x1b[90m{}\x1b[0m\n", r(1))),
            "{text:?}"
        );
        assert!(text.starts_with("The board\n"), "{text:?}");
    }

    /// `nd headway show <card>`: the header block, the indented title and
    /// description, then subissues, edges, review records and comments.
    #[test]
    fn card_detail_reads_like_git_show() {
        crate::fmt::freeze_now(NOW + 120);
        let mut c = card(1, "Epic");
        c.description = "line one\n\nline two".to_string();
        c.labels = vec!["cli".to_string()];
        c.parent = Some(NoteId::new([9; 32]));
        c.subissues = vec![SubissueView {
            id: NoteId::new([2; 32]),
            title: "Child".to_string(),
            column: Some("todo".to_string()),
            done: false,
            archived: false,
            seq: None,
        }];
        c.blocks = vec![EdgeRef {
            id: NoteId::new([3; 32]),
            title: "Later".to_string(),
            done: false,
        }];
        c.related = vec![EdgeRef {
            id: NoteId::new([4; 32]),
            title: "Nearby".to_string(),
            done: true,
        }];
        c.reviews = vec![ReviewView {
            id: NoteId::new([5; 32]),
            author: [7; 32],
            created_at: NOW,
            fields: ReviewFields {
                commit: Some("0123456789abcdef".to_string()),
                title: Some("do the thing".to_string()),
                branch: Some("main".to_string()),
                host: Some("monad".to_string()),
                path: Some("/src".to_string()),
                agentium: Some("agentium:a-b-c".to_string()),
                ..ReviewFields::default()
            },
            comments: vec![],
            sources: vec![],
        }];
        c.comments = vec![CommentView {
            id: NoteId::new([6; 32]),
            author: [7; 32],
            parent: None,
            body: "hello\nthere".to_string(),
            created_at: NOW,
        }];
        let view = board_of(vec![c], vec![]);
        let text = card_detail(&view, &view.columns[0].cards[0], "Todo", false);

        let author = crate::fmt::short_author(&[7; 32]);
        let comment_id = wordid::encode(&[6; 32]);
        let expected = format!(
            "card    {r1}\nid      {hex}\ncolumn  Todo\nlabels  cli\nparent  {r9}\ncreated 2m ago\n\
             \n    Epic\n\n    line one\n\n    line two\n\
             \nsubissues (0/1 done)\n    [ ] Child  (todo)  {r2}\n\
             \nblocks (1)\n    [ ] Later  {r3}\n\
             \nrelated (1)\n    Nearby  {r4}\n\
             \nreview (1)\n    0123456  do the thing  2m ago\n        monad:/src (main)\n        agentium:a-b-c\n\
             \ncomments (1)\n\n    {author}  2m ago  {comment_id}\n        hello\n        there\n",
            r1 = r(1),
            hex = NoteId::new([1; 32]).hex(),
            r9 = r(9),
            r2 = r(2),
            r3 = r(3),
            r4 = r(4),
        );
        assert_eq!(text, expected);
    }

    /// `nd headway next`: one plain ref and its title a line.
    #[test]
    fn ready_line_keeps_the_ref_plain() {
        assert_eq!(ready_line(&r(1), "One", false), format!("{}  One\n", r(1)));
        assert_eq!(
            ready_line(&r(1), "One", true),
            format!("{}  \x1b[90mOne\x1b[0m\n", r(1))
        );
    }

    /// `nd headway board`: own boards with the current one starred, shared ones
    /// under their heading with an owner, and the deleted tail.
    #[test]
    fn boards_marks_current_and_heads_shared() {
        let own = board_of(vec![card(1, "One")], vec![]);
        let mut shared = board_of(vec![], vec![]);
        shared.id = "team".to_string();
        shared.title = "Team".to_string();
        shared.author = [8; 32];
        let text = boards(
            std::slice::from_ref(&own),
            std::slice::from_ref(&shared),
            "b",
            false,
        );
        let owner = short_owner(&[8; 32]);
        assert_eq!(
            text,
            format!(
                "* b  The board · 1 cards\n\nshared with me\n  team  Team · 0 cards · owner {owner}\n"
            )
        );

        let unresolved = boards(std::slice::from_ref(&own), &[], "nope", false);
        assert!(
            unresolved.ends_with("* nope  (not created yet — run `nd headway seed`)\n"),
            "{unresolved}"
        );
        assert!(boards(&[], &[], "x", false).starts_with("no boards yet"));

        let mut gone = own;
        gone.deleted = true;
        assert_eq!(
            deleted_boards(std::slice::from_ref(&gone), false),
            "\ndeleted (`nd headway restore-board <board>` brings one back)\n  b  The board · 1 cards\n"
        );
        assert_eq!(deleted_boards(&[], false), "");
    }

    /// A declined edge edit reads as its own outcome: the already-done cases
    /// state what's already true (and are reported as success), while a refusal
    /// names the cycle. Neither may fall back to the generic "unknown card"
    /// catch-all, which sends the caller hunting for a longer id.
    #[test]
    fn declined_edits_read_as_themselves() {
        let view = board_of(vec![], vec![]);
        let card = NoteId::new([1u8; 32]);
        let other = NoteId::new([2u8; 32]);
        let msg = |reason, card, other| {
            declined_message(
                &view,
                &store::Declined {
                    reason,
                    card,
                    other,
                },
            )
        };
        let card_ref = plain_ref(&view, &card);
        let other_ref = plain_ref(&view, &other);

        assert_eq!(
            msg(DeclineReason::AlreadyBlocked, card, other),
            format!("{card_ref} is already blocked on {other_ref}")
        );
        assert_eq!(
            msg(DeclineReason::NotRelated, card, other),
            format!("{card_ref} isn't related to {other_ref}")
        );
        assert!(
            msg(DeclineReason::BlockCycle, card, other).contains("would create a dependency cycle")
        );
        // The degenerate cycles read as the typos they usually are.
        assert_eq!(
            msg(DeclineReason::BlockCycle, card, card),
            format!("{card_ref} can't block itself")
        );
        assert_eq!(
            msg(DeclineReason::ParentCycle, card, card),
            format!("{card_ref} can't be its own parent")
        );

        // Only the already-done half is success.
        assert!(DeclineReason::AlreadyBlocked.is_noop());
        assert!(!DeclineReason::BlockCycle.is_noop());
    }

    /// A review comment's place names its side, and its commit only when that
    /// isn't the record's own.
    #[test]
    fn review_place_names_side_and_foreign_commit() {
        let loc = |start, end, side| ReviewLocation {
            path: "src/a.rs".to_string(),
            commit: "abcdef0123".to_string(),
            start,
            end,
            side,
        };
        let own = Some("abcdef0123");
        assert_eq!(
            review_place(&loc(42, 42, LineSide::New), own),
            "src/a.rs:42"
        );
        assert_eq!(
            review_place(&loc(3, 5, LineSide::Old), own),
            "src/a.rs:old 3-5"
        );
        assert_eq!(
            review_place(&loc(3, 5, LineSide::New), Some("ffff")),
            "src/a.rs:3-5 @abcdef0"
        );
    }
}
