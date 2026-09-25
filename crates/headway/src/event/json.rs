//! JSON renderings of the view model, the CLI's `--json` output.

use nostrdb_net::Pubkey;

use super::view::{ActivityKind, ActivityView, BoardView, CardView, CommentView, EdgeRef};

/// Render `view` as a stable, machine-readable JSON value: a curated schema for
/// external tooling (e.g. the CLI's `--json`) with hex ids plus the full
/// `headway:<board>/<word-id>` `ref` used to address cards/comments, independent
/// of the internal view types.
pub fn board_json(view: &BoardView) -> serde_json::Value {
    serde_json::json!({
        "id": view.id,
        "title": view.title,
        "description": view.description,
        "columns": view.columns.iter().map(|c| serde_json::json!({
            "id": c.id,
            "name": c.name,
            "terminal": c.terminal,
            "cards": c.cards.iter().map(|card| card_json(&view.id, card)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "archived": view.archived.iter().map(|a| {
            let mut card = card_json(&view.id, &a.card);
            card["from"] = serde_json::json!(a.from);
            card
        }).collect::<Vec<_>>(),
    })
}

/// Render a single card on `board` as JSON. The `ref` fields are full
/// `headway:<board>/<word-id>` references (same-board children and comments share
/// `board`). See [`board_json`].
pub fn card_json(board: &str, card: &CardView) -> serde_json::Value {
    serde_json::json!({
        "id": card.id.hex(),
        "ref": crate::wordid::card_ref(board, card.id.bytes()),
        "author": Pubkey::new(card.author).hex(),
        "title": card.title,
        "description": card.description,
        "labels": card.labels,
        "priority": card.priority.as_str(),
        "due": card.due.map(|d| d.to_string()),
        "estimate": card.estimate,
        "rank": card.rank,
        "seq": card.seq,
        "created_at": card.created_at,
        "updated_at": card.updated_at,
        "parent": card.parent.map(|p| p.hex()),
        "parent_ref": card.parent.map(|p| crate::wordid::card_ref(board, p.bytes())),
        "blocked": card.is_blocked(),
        "blocked_by": card.blocked_by.iter().map(|e| edge_json(board, e)).collect::<Vec<_>>(),
        "blocks": card.blocks.iter().map(|e| edge_json(board, e)).collect::<Vec<_>>(),
        "related": card.related.iter().map(|e| edge_json(board, e)).collect::<Vec<_>>(),
        "subissues": card.subissues.iter().map(|s| serde_json::json!({
            "id": s.id.hex(),
            "ref": crate::wordid::card_ref(board, s.id.bytes()),
            "title": s.title,
            "column": s.column,
            "done": s.done,
            "archived": s.archived,
            "seq": s.seq,
        })).collect::<Vec<_>>(),
        "comments": card.comments.iter().map(|c| comment_json(board, c)).collect::<Vec<_>>(),
        "activity": card.activity.iter().map(|a| activity_json(board, a)).collect::<Vec<_>>(),
    })
}

/// Render a dependency edge (`blocked_by` / `blocks`) on `board` as JSON: the
/// referenced card's id, ref, resolved title, and cleared state. The `ref` shares
/// `board` — like `parent_ref`, cross-board edges aren't re-homed. See [`EdgeRef`].
fn edge_json(board: &str, edge: &EdgeRef) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id.hex(),
        "ref": crate::wordid::card_ref(board, edge.id.bytes()),
        "title": edge.title,
        "done": edge.done,
    })
}

/// Render one activity-timeline entry on `board` as JSON: a `type` discriminant
/// plus that variant's fields, flattened. See [`card_json`].
pub fn activity_json(board: &str, activity: &ActivityView) -> serde_json::Value {
    let mut v = match &activity.kind {
        ActivityKind::Created => serde_json::json!({"type": "created"}),
        ActivityKind::Moved { from, to, .. } => {
            serde_json::json!({"type": "moved", "from": from, "to": to})
        }
        ActivityKind::Archived => serde_json::json!({"type": "archived"}),
        ActivityKind::Restored { to, .. } => serde_json::json!({"type": "restored", "to": to}),
        ActivityKind::Renamed { to } => serde_json::json!({"type": "renamed", "to": to}),
        ActivityKind::DescriptionEdited => serde_json::json!({"type": "description_edited"}),
        ActivityKind::LabelsChanged { added, removed } => {
            serde_json::json!({"type": "labels_changed", "added": added, "removed": removed})
        }
        ActivityKind::FieldChanged { field, to } => {
            serde_json::json!({"type": "field_changed", "field": field.label(), "to": to})
        }
        ActivityKind::ParentSet { parent, title } => serde_json::json!({
            "type": "parent_set",
            "parent": parent.hex(),
            "parent_ref": crate::wordid::card_ref(board, parent.bytes()),
            "parent_title": title,
        }),
        ActivityKind::ParentRemoved => serde_json::json!({"type": "parent_removed"}),
    };
    v["author"] = serde_json::json!(Pubkey::new(activity.author).hex());
    v["created_at"] = serde_json::json!(activity.created_at);
    v
}

/// Render a single comment on `board` as JSON. See [`card_json`].
pub fn comment_json(board: &str, comment: &CommentView) -> serde_json::Value {
    serde_json::json!({
        "id": comment.id.hex(),
        "ref": crate::wordid::card_ref(board, comment.id.bytes()),
        "author": Pubkey::new(comment.author).hex(),
        "parent": comment.parent.map(|p| p.hex()),
        "body": comment.body,
        "created_at": comment.created_at,
    })
}
