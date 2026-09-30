//! The edit path: turning a parsed [`Command`] into a [`BoardAction`] by
//! resolving its card, column, container and comment arguments against the
//! folded board, and the [`Collect`] sink that gathers the frames an edit
//! produces.

use nostrdb_net::NoteId;

use headway::event::{self, BoardView, Container, Date, Priority, resolve_card};
use headway::store::{self, BoardAction, Publisher};
use headway::wordid;

use nostrdb_net::relay::sync::Result;

use crate::args::{Command, SeqSpec};

/// Translate a resolved [`Command`] into a [`BoardAction`], resolving card and
/// column arguments against `view`.
pub(crate) fn build_action(view: &BoardView, command: Command) -> Result<BoardAction> {
    Ok(match command {
        Command::Add {
            title,
            col,
            labels,
            parent,
            description,
        } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            let parent = parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?;
            BoardAction::AddCard {
                col,
                title,
                description: description.unwrap_or_default(),
                labels,
                parent,
            }
        }
        Command::Move { card, col, row } => {
            let card = resolve_card(view, &card)?;
            let to_col = resolve_col(view, &col)?;
            let to_row = row.unwrap_or(view.columns[to_col].cards.len());
            BoardAction::MoveCard {
                card,
                to_col,
                to_row,
            }
        }
        Command::Title { card, title } => BoardAction::EditTitle {
            card: resolve_card(view, &card)?,
            title,
        },
        Command::Desc { card, text } => BoardAction::EditDescription {
            card: resolve_card(view, &card)?,
            description: text,
        },
        Command::Label { card, labels } => BoardAction::SetLabels {
            card: resolve_card(view, &card)?,
            labels,
        },
        Command::Priority { card, level } => BoardAction::SetPriority {
            card: resolve_card(view, &card)?,
            priority: Priority::parse(&level),
        },
        Command::Due { card, date } => BoardAction::SetDue {
            card: resolve_card(view, &card)?,
            due: parse_clearable(&date, |s| {
                Date::parse(s).ok_or_else(|| format!("invalid date '{s}' (want YYYY-MM-DD)").into())
            })?,
        },
        Command::Estimate { card, points } => BoardAction::SetEstimate {
            card: resolve_card(view, &card)?,
            estimate: parse_clearable(&points, |s| {
                s.parse::<u32>()
                    .map_err(|_| format!("invalid estimate '{s}' (want a number)").into())
            })?,
        },
        Command::Parent { card, parent } => BoardAction::SetParent {
            card: resolve_card(view, &card)?,
            parent: parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?,
        },
        Command::Block { card, on } => BoardAction::Block {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Unblock { card, on } => BoardAction::Unblock {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Relate { card, other } => BoardAction::Relate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
        },
        Command::Unrelate { card, other } => BoardAction::Unrelate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
        },
        Command::Seq {
            card,
            spec,
            container,
        } => {
            let card = resolve_card(view, &card)?;
            let container = match container.as_deref() {
                Some(sel) => resolve_container(view, sel)?,
                None => Container::BoardRoot(view.id.clone()),
            };
            let position = match spec {
                SeqSpec::First => store::SeqPosition::First,
                SeqSpec::Last => store::SeqPosition::Last,
                SeqSpec::After(a) => store::SeqPosition::After(resolve_card(view, &a)?),
                SeqSpec::Before(a) => store::SeqPosition::Before(resolve_card(view, &a)?),
            };
            let rank = store::seq_rank(view, &container, card, &position)?;
            BoardAction::SetSequence {
                card,
                container,
                rank,
            }
        }
        Command::Comment {
            card,
            body,
            reply_to,
        } => {
            let card = resolve_card(view, &card)?;
            let reply_to = reply_to
                .as_deref()
                .map(|sel| resolve_comment(view, &card, sel))
                .transpose()?;
            BoardAction::AddComment {
                card,
                body,
                reply_to,
            }
        }
        Command::Review { card, review } => BoardAction::AddReview {
            card: resolve_card(view, &card)?,
            review,
        },
        Command::Delete { card } => BoardAction::DeleteCard {
            card: resolve_card(view, &card)?,
        },
        Command::Archive { card } => BoardAction::ArchiveCard {
            card: resolve_card(view, &card)?,
        },
        Command::Restore { card } => BoardAction::RestoreCard {
            card: resolve_card(view, &card)?,
        },
        Command::Rename { title } => BoardAction::RenameBoard { title },
        Command::Terminal { col, terminal } => BoardAction::SetColumnTerminal {
            col: resolve_col(view, &col)?,
            terminal,
        },
        Command::Show { .. }
        | Command::Next { .. }
        | Command::Diff { .. }
        | Command::Seed { .. }
        | Command::Migrate
        | Command::Share { .. }
        | Command::Link { .. }
        | Command::MoveBoard { .. }
        | Command::Board { .. }
        | Command::Login { .. }
        | Command::Logout => {
            unreachable!("handled before build_action")
        }
    })
}

/// Parse a "set or clear" scalar CLI value: `none` (or an empty string) clears
/// the field (`Ok(None)`); any other value is run through `parse`. Shared by the
/// `due`/`estimate` commands so both take `none` to clear.
fn parse_clearable<T>(s: &str, parse: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    parse(s).map(Some)
}

/// Collects the `["EVENT", {...}]` frames an edit produces so they can be
/// forwarded to the relay after `apply` returns.
#[derive(Default)]
pub(crate) struct Collect(pub(crate) Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

fn resolve_col(view: &BoardView, sel: &str) -> Result<usize> {
    view.columns
        .iter()
        .position(|c| c.id == sel || c.name.eq_ignore_ascii_case(sel))
        .ok_or_else(|| {
            let names: Vec<&str> = view.columns.iter().map(|c| c.name.as_str()).collect();
            format!("no column matching '{sel}'; columns: {}", names.join(", ")).into()
        })
}

/// Resolve an `--in` container selector: a card ref names that card's container
/// (its subissues); anything else must be the board's own slug, naming the
/// board-root work-order.
pub(crate) fn resolve_container(view: &BoardView, sel: &str) -> Result<Container> {
    match resolve_card(view, sel) {
        Ok(id) => Ok(Container::Card(*id.bytes())),
        Err(_) if sel.eq_ignore_ascii_case(&view.id) => Ok(Container::BoardRoot(view.id.clone())),
        Err(e) => Err(e.into()),
    }
}

/// Resolve a `--reply-to` selector against the comments on `card`, accepting a
/// full hex id, a unique hex prefix, or a comment word-id. Comments render as a
/// bare word-id in a card's thread (not a `headway:<board>/…` card ref), so the
/// bare word-id is the selector here.
fn resolve_comment(view: &BoardView, card: &NoteId, sel: &str) -> Result<NoteId> {
    let comments = event::all_cards(view)
        .find(|c| c.id == *card)
        .map(|c| c.comments.as_slice())
        .unwrap_or(&[]);

    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();
    if let Some(c) = comments
        .iter()
        .find(|c| wordid::encode(c.id.bytes()) == sel)
    {
        return Ok(c.id);
    }

    let mut hits = comments.iter().filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id),
        (Some(_), Some(_)) => Err(format!("ambiguous comment prefix '{sel}'").into()),
        _ => Err(format!("no comment matching '{sel}' on this card").into()),
    }
}
