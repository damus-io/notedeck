//! The edit path: turning a parsed [`Command`] into a [`BoardAction`] by
//! resolving its card, column, container and comment arguments against the
//! folded board (with [`headway::edit`], which the app's tools share), and the
//! [`Collect`] sink that gathers the frames an edit produces.

use headway::edit::{comment_action, parse_due, parse_estimate, resolve_col};
use headway::event::{BoardView, Container, Priority, resolve_card};
use headway::store::{self, BoardAction, NewCard, Publisher};

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
            fields,
        } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            let parent = parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?;
            BoardAction::AddCard(NewCard {
                col,
                title,
                description: description.unwrap_or_default(),
                labels,
                parent,
                priority: fields
                    .priority
                    .as_deref()
                    .map(Priority::parse)
                    .unwrap_or_default(),
                estimate: fields
                    .estimate
                    .as_deref()
                    .map(parse_estimate)
                    .transpose()?
                    .flatten(),
                due: fields.due.as_deref().map(parse_due).transpose()?.flatten(),
            })
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
            due: parse_due(&date)?,
        },
        Command::Estimate { card, points } => BoardAction::SetEstimate {
            card: resolve_card(view, &card)?,
            estimate: parse_estimate(&points)?,
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
            review,
        } => comment_action(
            view,
            resolve_card(view, &card)?,
            body,
            reply_to.as_deref(),
            &review,
        )?,
        Command::Review {
            card,
            review,
            sources,
        } => BoardAction::AddReview {
            card: resolve_card(view, &card)?,
            review,
            sources,
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
        | Command::SeqQuery { .. }
        | Command::Next { .. }
        | Command::Grep { .. }
        | Command::List { .. }
        | Command::Diff { .. }
        | Command::Seed { .. }
        | Command::DeleteBoard { .. }
        | Command::RestoreBoard { .. }
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

/// Collects the `["EVENT", {...}]` frames an edit produces so they can be
/// forwarded to the relay after `apply` returns.
#[derive(Default)]
pub(crate) struct Collect(pub(crate) Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
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
