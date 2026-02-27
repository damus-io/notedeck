use enostr::{NoteId, Pubkey};
use notedeck::SubOwnerKey;

use crate::timeline::TimelineKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ColumnsOwner {
    OnboardingFollowPacks,
    ThreadScope,
    TimelineRemote,
}

/// Stable owner key for onboarding follow-pack scoped subscriptions.
pub fn onboarding_owner_key(col: usize) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::OnboardingFollowPacks)
        .with(col)
        .finish()
}

/// Stable owner key for one thread scope within one column and account.
pub fn thread_scope_owner_key(
    account_pk: Pubkey,
    col: usize,
    root_id: &NoteId,
    scope_depth: usize,
) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::ThreadScope)
        .with(account_pk)
        .with(col)
        .with(*root_id.bytes())
        .with(scope_depth)
        .finish()
}

/// Stable owner key for timeline remote subscriptions per account/kind pair.
pub fn timeline_remote_owner_key(account_pk: Pubkey, kind: &TimelineKind) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::TimelineRemote)
        .with(account_pk)
        .with(kind)
        .finish()
}
