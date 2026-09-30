//! Nostr event model for headway boards.
//!
//! Cards are NIP-34 issues (kind 1621) anchored to a headway *board* (a custom
//! addressable kind). Because the issue event is immutable, everything mutable
//! about a card — its title, labels, description and which column it sits in —
//! lives in *separate* events:
//!
//! | concept           | kind    | mechanism                                  |
//! | ----------------- | ------- | ------------------------------------------ |
//! | board             | `30619` | addressable; `d` = board id, ordered `col` |
//! | card              | `1621`  | NIP-34 issue, `a` → board                  |
//! | title edit        | `1985`  | NIP-32 label, `L`/`l` namespace `#subject` |
//! | labels            | `1985`  | NIP-32 label, `L`/`l` namespace `#t`       |
//! | description edit  | `1624`  | gitworkshop cover note                     |
//! | placement         | `30620` | addressable; `col` + fractional `rank`     |
//! | relation          | `30621` | addressable; `d` = child, `parent` tag     |
//! | sequence          | `30622` | addressable; `d` = `<container>:<issue>`   |
//! | blockers          | `30624` | addressable; `d` = blocked, `blocked-by` tags |
//! | related           | `30625` | addressable; `d` = a card, `related` tags |
//! | review record     | `1626`  | append-only; `e` → card, commit/host/path |
//!
//! Effective state is resolved as **latest-authorised-wins** for every overlay
//! (placement, subject, cover note, and labels — each label event carries the
//! card's complete set, so the newest one wins), where "authorised"
//! means the event's author is the card author or the board's author
//! (maintainer). This mirrors the ngitstack/gitworkshop "Shared Issue / Patch /
//! PR Metadata" spec.
//!
//! This module is pure: it builds and parses notes and reduces a set of them
//! into a [`BoardView`]. Relay/ndb plumbing lives in the app layer.
//!
//! Each concern lives in a private submodule, all re-exported from here: the
//! event kinds (`kinds`), the value types (`model`), the note builders
//! (`build`), the parsed events (`parse`), the view model (`view`) and its JSON
//! rendering (`json`), the reducer (`reduce`), ndb loading (`load`), board and
//! card resolution (`resolve`), and fractional ranking (`rank`).

mod build;
mod json;
mod kinds;
mod load;
mod model;
mod parse;
mod rank;
mod reduce;
mod resolve;
mod view;

pub use build::{
    build_archive_placement, build_blockers, build_board, build_board_pref, build_comment,
    build_cover_note, build_field, build_issue, build_labels, build_placement, build_related,
    build_relation, build_review, build_review_comment, build_sequence, build_subject_edit,
};
pub use json::{
    activity_json, board_json, card_json, comment_json, review_comment_json, review_json,
};
pub use kinds::{
    HEADWAY_KINDS, KIND_BLOCKERS, KIND_BOARD, KIND_BOARD_PREF, KIND_COMMENT, KIND_COVER_NOTE,
    KIND_ISSUE, KIND_LABEL, KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_REVIEW,
    KIND_SEQUENCE, is_addressable,
};
pub(crate) use load::shared_fold_admits;
pub use load::{
    board_pref_created_at, board_scoped_filters, card_meta_filter, comment_filter,
    current_blockers, current_related, fold_board, fold_shared_board, headway_filter, load_board,
    load_board_pref, load_shared_board, reduce_delta,
};
pub use model::{
    BoardCoord, COL_ARCHIVED, COL_DELETED, ColumnDef, Date, Field, LineSide, Priority,
    ReviewFields, ReviewLocation, board_address, column_is_terminal,
};
pub use parse::{
    BlockerSet, BoardEvent, CommentEvent, Container, CoverNote, FieldEdit, HeadwayEvent,
    IssueEvent, LabelSet, PlacementEvent, RelatedSet, RelationEvent, ReviewCommentEvent,
    ReviewEvent, SequenceEvent, SubjectEdit, parse,
};
pub use rank::rank_between;
pub use reduce::{BoardReducer, reduce};
pub use resolve::{
    ColumnPos, LocatedCard, ResolvedCard, all_cards, card_in_board, card_with_column_in_board,
    find_board, locate_card, locate_card_in_boards, pick_board, pick_card, pick_card_with_column,
    resolve_card, resolve_card_by_wordid,
};
pub use view::{
    ActivityKind, ActivityView, ArchivedCard, BoardView, CardView, ColumnView, CommentView,
    EdgeRef, ReviewCommentView, ReviewView, SubissueView,
};
