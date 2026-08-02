use enostr::{NoteId, Pubkey};
use notedeck::SubOwnerKey;

use crate::column::ColumnId;
use crate::timeline::TimelineKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ColumnsOwner {
    Onboarding,
    ThreadScope,
    TimelineRemote,
}

/// Stable owner key for onboarding remote subscriptions within one column.
pub fn onboarding_owner_key(col: ColumnId) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::Onboarding)
        .with(col)
        .finish()
}

/// Stable owner key for one thread scope within one column and account.
pub fn thread_scope_owner_key(
    account_pk: Pubkey,
    col: ColumnId,
    root_id: &NoteId,
    scope_id: u64,
) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::ThreadScope)
        .with(account_pk)
        .with(col)
        .with(*root_id.bytes())
        .with(scope_id)
        .finish()
}

/// Stable owner key for timeline remote subscriptions per account/kind pair.
pub fn timeline_remote_owner_key(account_pk: Pubkey, kind: &TimelineKind) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::TimelineRemote)
        .with(account_pk)
        .with(kind)
        .finish()
}
