//! Rendering: the board listing, `show`'s card detail, `next`'s frontier, the
//! `board` list, and the decline messages an edit reports.

use nostrdb_net::{NoteId, Pubkey};
use serde_json::json;

use headway::event::{
    self, BoardView, CardView, CommentView, Container, LineSide, Priority, ReviewCommentView,
    ReviewLocation, ReviewView, resolve_card,
};
use headway::store::{self, DeclineReason};
use headway::traversal::{self, NotReady};
use headway::wordid;

use nostrdb_net::relay::sync::Result;

/// Live (non-archived) card count across a board's columns.
pub(crate) fn card_count(view: &BoardView) -> usize {
    view.columns.iter().map(|c| c.cards.len()).sum()
}

/// Render the board list: our own boards, then the ones `shared` with us under
/// their own heading, each owner shown so two same-slug boards tell apart.
/// `current` is marked with a `*` — on our own board by that slug, else on the
/// one shared board it resolves to (see `shared_owner` in `main.rs`). Falls
/// back to a hint when the cache holds no boards at all.
pub(crate) fn print_boards(own: &[BoardView], shared: &[BoardView], current: &str) {
    if own.is_empty() && shared.is_empty() {
        println!(
            "no boards yet — current selection is '{current}'. Run `headway seed` to create it."
        );
        return;
    }
    let own_current = own.iter().any(|v| v.id == current);
    for view in own {
        let mark = if view.id == current { "*" } else { " " };
        let detail =
            nostrdb_net::relay::sync::dim(&format!("{} · {} cards", view.title, card_count(view)));
        println!("{mark} {}  {detail}", view.id);
    }
    // The shared board `--board <current>` resolves to, if exactly one does;
    // several are ambiguous, so none is marked.
    let shared_matches = shared.iter().filter(|v| v.id == current).count();
    let shared_current = !own_current && shared_matches == 1;
    let unresolved = !own_current && shared_matches == 0;
    if unresolved {
        println!(
            "* {current}  {}",
            nostrdb_net::relay::sync::dim("(not created yet — run `headway seed`)")
        );
    }
    if shared.is_empty() {
        return;
    }
    if !own.is_empty() || unresolved {
        println!();
    }
    println!("shared with me");
    for view in shared {
        let mark = if shared_current && view.id == current {
            "*"
        } else {
            " "
        };
        let detail = nostrdb_net::relay::sync::dim(&format!(
            "{} · {} cards · owner {}",
            view.title,
            card_count(view),
            short_owner(&view.author)
        ));
        println!("{mark} {}  {detail}", view.id);
    }
}

/// The tail of `headway board`: the deleted boards, by slug, so a restore can
/// name them. Silent when there are none.
pub(crate) fn print_deleted_boards(deleted: &[BoardView]) {
    if deleted.is_empty() {
        return;
    }
    println!(
        "\ndeleted {}",
        nostrdb_net::relay::sync::dim("(`headway restore-board <board>` brings one back)")
    );
    for view in deleted {
        let detail =
            nostrdb_net::relay::sync::dim(&format!("{} · {} cards", view.title, card_count(view)));
        println!("  {}  {detail}", view.id);
    }
}

/// Why an edit to a deleted board is refused, and how to undo the delete.
pub(crate) fn deleted_board_error(board: &str) -> String {
    format!("board '{board}' is deleted — restore it with `headway restore-board {board}` first")
}

/// A board owner as a short npub (`npub1pjn83hs2…`) — enough to tell owners
/// apart in a listing. `--author` wants the whole key, which the ambiguity error
/// prints in full.
fn short_owner(owner: &[u8; 32]) -> String {
    let pk = Pubkey::new(*owner);
    let full = pk.npub().unwrap_or_else(|| pk.hex());
    format!("{}…", &full[..full.len().min(14)])
}

pub(crate) fn print_board(view: &BoardView, as_json: bool, show_archived: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&event::board_json(view))
                .unwrap_or_else(|_| "null".into())
        );
        return;
    }

    println!("{}", view.title);
    if view.deleted {
        println!(
            "{}",
            nostrdb_net::relay::sync::dim(&format!(
                "(deleted — `headway restore-board {}` brings it back)",
                view.id
            ))
        );
    }
    if !view.description.is_empty() {
        println!("{}", view.description);
    }
    for col in &view.columns {
        println!("\n{} ({})", col.name, col.cards.len());
        for c in &col.cards {
            println!(
                "  {}{}{}{}{}  {}",
                blocked_prefix(c),
                priority_prefix(c.priority),
                c.title,
                progress_suffix(c),
                labels_suffix(&c.labels),
                card_ref(view, &c.id),
            );
        }
    }
    if !view.archived.is_empty() {
        if show_archived {
            println!("\nArchived ({})", view.archived.len());
            for a in &view.archived {
                println!("  {}  {}", a.card.title, card_ref(view, &a.card.id));
            }
        } else {
            println!(
                "\nArchived ({}) — use `show --archived` to list",
                view.archived.len()
            );
        }
    }
}

/// Render every board in the cache. In text mode each board is printed with
/// [`print_board`], the boards separated by a blank line; in JSON mode they
/// become a single array so the combined output stays machine-parseable.
///
/// A board someone else owns (one shared with `me`) is headed with its owner, and
/// every JSON board carries an `owner` hex, since slugs are only unique per owner.
pub(crate) fn print_all_boards(
    boards: &[BoardView],
    me: &Pubkey,
    as_json: bool,
    show_archived: bool,
) {
    if as_json {
        let arr: Vec<_> = boards
            .iter()
            .map(|view| {
                let mut board = event::board_json(view);
                board["owner"] = json!(hex::encode(view.author));
                board
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    if boards.is_empty() {
        println!("no boards yet — run `headway seed` to create one");
        return;
    }
    for (i, view) in boards.iter().enumerate() {
        if i > 0 {
            println!();
        }
        // Lead with the addressable slug: board titles can collide (two boards
        // both titled "Headway"), and the slug is what `--board`/`board <id>`
        // take, so it's the anchor for the human-readable title that follows.
        let heading = if &view.author == me.bytes() {
            view.id.clone()
        } else {
            format!("{} · shared by {}", view.id, short_owner(&view.author))
        };
        println!("{}", nostrdb_net::relay::sync::dim(&heading));
        print_board(view, false, show_archived);
    }
}

/// Print only the cards named by `sels` (each a card id or unique short prefix).
/// In JSON mode this is an array of card objects, each with the `column` it
/// currently sits in; otherwise one card per line.
pub(crate) fn print_cards(view: &BoardView, sels: &[String], as_json: bool) -> Result<()> {
    // Resolve every selector first so a bad id fails the whole command rather
    // than printing a partial result.
    let cards: Vec<PlacedCard> = sels
        .iter()
        .map(|sel| {
            let id = resolve_card(view, sel)?;
            find_card(view, &id).ok_or_else(|| format!("no card matching '{sel}'").into())
        })
        .collect::<Result<_>>()?;

    if as_json {
        let out: Vec<_> = cards
            .iter()
            .map(|p| {
                let mut j = event::card_detail_json(&view.id, p.card);
                j["column"] = json!(p.column);
                j
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
    } else {
        for (i, p) in cards.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_card_detail(view, p.card, p.column);
        }
    }
    Ok(())
}

/// Print the text of the review source named `name` on card `sel`, exactly
/// as stored, so a script can `> book/<word-id>.md`. The newest record that
/// carries a file of that name wins, so a later record without sources doesn't
/// hide the explainer an earlier one stored.
pub(crate) fn print_review_source(view: &BoardView, sel: &str, name: &str) -> Result<()> {
    use std::io::Write;
    let id = resolve_card(view, sel)?;
    let card = find_card(view, &id).ok_or_else(|| format!("no card matching '{sel}'"))?;
    let source = card
        .card
        .reviews
        .iter()
        .find_map(|r| r.sources.iter().find(|s| s.name == name))
        .ok_or_else(|| format!("no review source named '{name}' on {sel}"))?;
    std::io::stdout().write_all(source.text.as_bytes())?;
    Ok(())
}

/// Print the work-order frontier for `next`: the ready cards of `container`, each
/// as a `headway:board/word-id` ref an agent can paste straight into `move`/`show`.
///
/// Which: `labels` (from `--label`) keeps only cards carrying every one of them,
/// applied to the frontier before the count, so `--label x -n 1` is the first
/// ready `x` card rather than the first ready card if it happens to be `x`.
///
/// How many: `-n <k>` caps to `k`; otherwise `--ready` prints the whole ready set
/// (the parallel-dispatch frontier) and the default prints just the single next
/// card. In JSON mode it's an array of `{ref, id, title}` objects.
pub(crate) fn print_next(
    view: &BoardView,
    container: &Container,
    ready: bool,
    limit: Option<usize>,
    labels: &[String],
    as_json: bool,
) {
    let frontier = traversal::ready_labelled(view, container, labels);
    let count = match (ready, limit) {
        (_, Some(k)) => k,
        (true, None) => frontier.len(),
        (false, None) => 1,
    };
    let picked = frontier.into_iter().take(count);

    if as_json {
        let arr: Vec<_> = picked
            .map(|c| {
                json!({
                    "ref": plain_ref(view, &c.id),
                    "id": c.id.hex(),
                    "title": c.title,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }

    let mut any = false;
    for c in picked {
        any = true;
        println!(
            "{}  {}",
            plain_ref(view, &c.id),
            nostrdb_net::relay::sync::dim(&c.title)
        );
    }
    if !any {
        eprintln!(
            "{}",
            nostrdb_net::relay::sync::dim(
                "nothing ready — everything is done, in progress or blocked (or filtered out by --label)"
            )
        );
    }
}

/// Print where `card` sits in `container`'s work-order for `seq <card>` with no
/// position flag: its index in the whole walk, its neighbours, whether it has
/// a rank of its own, and how much of the ready frontier is ahead of it.
///
/// Both counts are printed because they answer different questions: "31 of
/// 40" is the place in the order, "22 ready ahead" is how many cards `next`
/// would hand out first. Errors when the walk never reaches the card.
pub(crate) fn print_seq_place(
    view: &BoardView,
    container: &Container,
    card: NoteId,
    as_json: bool,
) -> Result<()> {
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
    let title = |id: NoteId| view.card(id).map_or("", |c| c.title.as_str());
    let why = place.not_ready.map(|r| match r {
        NotReady::Done => "done".to_string(),
        NotReady::InProgress => "In Progress".to_string(),
        NotReady::Blocked { by } => format!("blocked by {}", plain_ref(view, &by)),
        NotReady::OpenSubissues => "has open subissues".to_string(),
    });

    if as_json {
        let neighbour = |id: Option<NoteId>| {
            id.map(|id| json!({ "ref": plain_ref(view, &id), "title": title(id) }))
        };
        let obj = json!({
            "ref": plain_ref(view, &card),
            "container": container_ref,
            "index": place.index,
            "of": place.of,
            "sequenced": place.sequenced,
            "before": neighbour(place.before),
            "after": neighbour(place.after),
            "ready_ahead": place.ready_ahead,
            "ready": place.ready(),
            "not_ready_why": why,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "null".into())
        );
        return Ok(());
    }

    let dim = nostrdb_net::relay::sync::dim;
    let neighbour = |id: Option<NoteId>, end: &str| match id {
        Some(id) => format!("{}  {}", plain_ref(view, &id), dim(title(id))),
        None => dim(end),
    };
    println!("{}  {}", plain_ref(view, &card), dim(title(card)));
    println!("  order   {} of {} in {in_name}", place.index, place.of);
    println!(
        "  rank    {}",
        if place.sequenced {
            "sequenced"
        } else {
            "unsequenced: placed by when it was made"
        }
    );
    println!("  before  {}", neighbour(place.before, "(first)"));
    println!("  after   {}", neighbour(place.after, "(last)"));
    println!(
        "  ready   {} ready ahead; {}",
        place.ready_ahead,
        match &why {
            None => "this one is ready".to_string(),
            Some(why) => format!("not ready: {why}"),
        }
    );
    Ok(())
}

/// A card's `headway:<board>/<word-id>` ref, undimmed — unlike [`card_ref`],
/// `next` output is the thing an agent copies, so the ref itself must stay plain
/// text.
pub(crate) fn plain_ref(view: &BoardView, id: &NoteId) -> String {
    wordid::card_ref(&view.id, id.bytes())
}

/// Print a single card in `git show` style: a header block of metadata, then the
/// title and description body indented underneath. Used when `show` is given
/// explicit card selectors, where the full card is more useful than the
/// one-line board summary.
fn print_card_detail(view: &BoardView, card: &CardView, col: &str) {
    println!("card    {}", card_ref(view, &card.id));
    println!("id      {}", card.id.hex());
    println!("column  {col}");
    if !card.labels.is_empty() {
        println!("labels  {}", card.labels.join(", "));
    }
    if card.priority != Priority::None {
        println!("priority {}", card.priority.as_str());
    }
    if let Some(due) = card.due {
        println!("due     {due}");
    }
    if let Some(estimate) = card.estimate {
        println!("estimate {estimate}");
    }
    if let Some(parent) = &card.parent {
        println!("parent  {}", card_ref(view, parent));
    }
    println!("created {}", headway::fmt::rel_time(card.created_at));
    if card.updated_at > card.created_at {
        println!("updated {}", headway::fmt::rel_time(card.updated_at));
    }

    println!("\n    {}", card.title);
    if !card.description.is_empty() {
        println!();
        for line in card.description.lines() {
            if line.is_empty() {
                println!();
            } else {
                println!("    {line}");
            }
        }
    }

    if !card.subissues.is_empty() {
        let done = card.subissues.iter().filter(|s| s.done).count();
        println!("\nsubissues ({done}/{} done)", card.subissues.len());
        for s in &card.subissues {
            let mark = if s.done { "x" } else { " " };
            // Where the child sits, when we know: its column, or "archived".
            let place = if s.archived {
                Some("archived".to_string())
            } else {
                s.column.clone()
            };
            let place = place.map_or(String::new(), |p| {
                format!("  {}", nostrdb_net::relay::sync::dim(&format!("({p})")))
            });
            println!("    [{mark}] {}{place}  {}", s.title, card_ref(view, &s.id));
        }
    }

    print_edges(view, "blocked by", &card.blocked_by);
    print_edges(view, "blocks", &card.blocks);
    print_related(view, &card.related);

    if !card.reviews.is_empty() {
        println!("\nreview ({})", card.reviews.len());
        for r in &card.reviews {
            print_review(r);
        }
    }

    if !card.comments.is_empty() {
        println!("\ncomments ({})", card.comments.len());
        for c in &card.comments {
            print_comment(c);
        }
    }
}

/// Print a single comment in the card-detail thread: an author/time header (with
/// the comment's own word-id so it can be `--reply-to`'d), then its body indented
/// beneath. Replies are flagged inline but still rendered flat for now.
fn print_comment(c: &CommentView) {
    let mut header = format!(
        "    {}  {}  {}",
        headway::fmt::short_author(&c.author),
        nostrdb_net::relay::sync::dim(&headway::fmt::rel_time(c.created_at)),
        nostrdb_net::relay::sync::dim(&wordid::encode(c.id.bytes())),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&nostrdb_net::relay::sync::dim(&format!(
            "  ↳ reply to {}",
            wordid::encode(parent.bytes())
        )));
    }
    println!("\n{header}");
    for line in c.body.lines() {
        if line.is_empty() {
            println!();
        } else {
            println!("        {line}");
        }
    }
}

/// Print one review record in the card-detail view, newest first as
/// [`CardView::reviews`] holds them: the short sha and subject, then where the
/// commit lives (`host:path (branch)`), the session, the explainer and the
/// deploy, each on its own line and only when recorded.
fn print_review(r: &ReviewView) {
    let f = &r.fields;
    let sha = f
        .commit
        .as_deref()
        .map_or("-------", |c| c.get(..7).unwrap_or(c));
    println!(
        "    {sha}  {}  {}",
        f.title.as_deref().unwrap_or(""),
        nostrdb_net::relay::sync::dim(&headway::fmt::rel_time(r.created_at)),
    );
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
        println!("        {place}{branch}");
    }
    for line in [
        f.agentium.as_deref(),
        f.explainer.as_deref(),
        f.deploy.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        println!("        {line}");
    }
    print_review_comments(r);
}

/// Print a review record's inline comments under it: a count, then each
/// top-level comment with its replies nested beneath (see
/// [`print_review_comment`]). A reply whose parent isn't on this record shows
/// as top-level rather than vanishing.
fn print_review_comments(r: &ReviewView) {
    let n = r.comments.len();
    if n == 0 {
        return;
    }
    let noun = if n == 1 { "comment" } else { "comments" };
    println!(
        "        {}",
        nostrdb_net::relay::sync::dim(&format!("{n} review {noun}"))
    );
    let on_record = |id: &NoteId| r.comments.iter().any(|c| c.id == *id);
    for c in r
        .comments
        .iter()
        .filter(|c| !c.parent.as_ref().is_some_and(on_record))
    {
        println!();
        print_review_comment(r, c, None, 0);
    }
}

/// Print review comment `c` at `depth` levels of nesting, then its replies one
/// level deeper, oldest first as [`ReviewView::comments`] holds them. The
/// header carries the author, age and the comment's own word-id (what
/// `comment --reply-to` takes), then where it points ([`review_place`]) unless
/// that's where its parent points too.
fn print_review_comment(
    r: &ReviewView,
    c: &ReviewCommentView,
    parent: Option<&ReviewCommentView>,
    depth: usize,
) {
    let indent = " ".repeat(8 + 4 * depth);
    let mut header = format!(
        "{indent}{}{}  {}  {}",
        if depth > 0 { "↳ " } else { "" },
        headway::fmt::short_author(&c.author),
        nostrdb_net::relay::sync::dim(&headway::fmt::rel_time(c.created_at)),
        nostrdb_net::relay::sync::dim(&wordid::encode(c.id.bytes())),
    );
    if let Some(loc) = &c.location
        && parent.and_then(|p| p.location.as_ref()) != Some(loc)
    {
        header.push_str("  ");
        header.push_str(&review_place(loc, r.fields.commit.as_deref()));
    }
    println!("{header}");
    for line in c.body.lines() {
        if line.is_empty() {
            println!();
        } else {
            println!("{indent}    {line}");
        }
    }
    for reply in r.comments.iter().filter(|x| x.parent == Some(c.id)) {
        print_review_comment(r, reply, Some(c), depth + 1);
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

/// A card on a board with where it sits, as [`placed_cards`] yields it.
#[derive(Clone, Copy)]
pub(crate) struct PlacedCard<'a> {
    pub(crate) card: &'a CardView,
    /// Its column's name, or `archived` (the word `show <card>` uses) for an
    /// archived card.
    pub(crate) column: &'a str,
}

/// Every card on `view`, live then archived, each with where it sits.
pub(crate) fn placed_cards(view: &BoardView) -> impl Iterator<Item = PlacedCard<'_>> {
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
fn find_card<'a>(view: &'a BoardView, id: &NoteId) -> Option<PlacedCard<'a>> {
    placed_cards(view).find(|p| p.card.id == *id)
}

/// A card's labels as `  [a, b]` after its title, empty when it has none.
pub(crate) fn labels_suffix(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", labels.join(", "))
    }
}

/// A compact at-a-glance priority marker printed before a card's title on the
/// board listing: a dim glyph for the lower priorities and a bold `!` for urgent,
/// empty for [`Priority::None`] so unprioritised cards stay unadorned.
fn priority_prefix(priority: Priority) -> String {
    match priority {
        Priority::None => String::new(),
        Priority::Urgent => priority_glyph(priority).to_string(),
        _ => nostrdb_net::relay::sync::dim(priority_glyph(priority)),
    }
}

/// The marker [`priority_prefix`] draws for `priority`, unstyled, trailing
/// space included: `↓ `, `= `, `↑ `, `! `, or empty for [`Priority::None`].
/// Shared with `list`, which styles it under `--color` itself.
pub(crate) fn priority_glyph(priority: Priority) -> &'static str {
    match priority {
        Priority::None => "",
        Priority::Low => "↓ ",
        Priority::Medium => "= ",
        Priority::High => "↑ ",
        Priority::Urgent => "! ",
    }
}

/// A dim `n/m` subissue rollup shown after a parent card's title on the board
/// listing; empty for cards with no children.
fn progress_suffix(card: &CardView) -> String {
    if card.subissues.is_empty() {
        return String::new();
    }
    let done = card.subissues.iter().filter(|s| s.done).count();
    format!(
        "  {}",
        nostrdb_net::relay::sync::dim(&format!("{done}/{}", card.subissues.len()))
    )
}

/// Print a card-detail dependency section (`blocked by:` / `blocks:`) — one line
/// per edge, an `x` marking a cleared (done/archived) blocker so an open one
/// stands out. Nothing is printed when there are no edges.
fn print_edges(view: &BoardView, label: &str, edges: &[headway::event::EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    println!("\n{label} ({})", edges.len());
    for e in edges {
        let mark = if e.done { "x" } else { " " };
        println!("    [{mark}] {}  {}", e.title, card_ref(view, &e.id));
    }
}

/// Print a card-detail `related` section — one `<title>  <ref>` line per edge.
/// Unlike [`print_edges`] there is no cleared marker: the relation is undirected
/// and semantics-free ("see also"), so a partner's doneness is irrelevant.
/// Nothing is printed when there are no related edges.
fn print_related(view: &BoardView, edges: &[headway::event::EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    println!("\nrelated ({})", edges.len());
    for e in edges {
        println!("    {}  {}", e.title, card_ref(view, &e.id));
    }
}

/// The marker [`blocked_prefix`] draws for a blocked card, unstyled.
pub(crate) const BLOCKED_GLYPH: &str = "⊘ ";

/// A dim ⊘ glyph flagging a card as blocked (held back by an unfinished blocker)
/// on the board listing; empty when the card is free to work on.
fn blocked_prefix(card: &CardView) -> String {
    if card.is_blocked() {
        nostrdb_net::relay::sync::dim(BLOCKED_GLYPH)
    } else {
        String::new()
    }
}

/// Say what the reducer declined, in words the caller can act on. An idempotent
/// no-op ([`store::DeclineReason::is_noop`]) reads as a statement of the board's
/// current shape — the caller's ask is already true — while a refusal names the
/// invariant it would have broken. Undimmed: unlike a listing, this is the whole
/// message, not a reference beside a title.
pub(crate) fn declined_message(view: &BoardView, declined: &store::Declined) -> String {
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

/// A card's human-friendly reference: `headway:<board>/<word-id>`, e.g.
/// `headway:dave/maple-river-canyon` — a URI scheme so it reads as a reference
/// inline (`Fixes: headway:dave/maple-river-canyon`) and in chat, survives
/// nostrdb tokenization, and needs no shell quoting. Just a rendering of the event
/// id — see [`headway::wordid`]. Rendered muted so the title stays the eye's anchor.
fn card_ref(view: &BoardView, id: &NoteId) -> String {
    nostrdb_net::relay::sync::dim(&wordid::card_ref(&view.id, id.bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declined edge edit reads as its own outcome: the already-done cases
    /// state what's already true (and are reported as success), while a refusal
    /// names the cycle. Neither may fall back to the generic "unknown card"
    /// catch-all, which sends the caller hunting for a longer id.
    #[test]
    fn declined_edits_read_as_themselves() {
        let view = BoardView {
            id: "headway".to_string(),
            author: [0u8; 32],
            title: "Headway".to_string(),
            description: String::new(),
            created_at: 0,
            columns: vec![],
            archived: vec![],
            deleted: false,
        };
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
