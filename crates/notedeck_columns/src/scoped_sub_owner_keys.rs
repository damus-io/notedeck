use nostrdb_net::{NoteId, Pubkey};
use notedeck::SubOwnerKey;

use crate::column::ColumnId;
use crate::deeplink::DeepLinkId;
use crate::timeline::TimelineKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ColumnsOwner {
    Onboarding,
    ThreadScope,
    TimelineRemote,
}

/// Stable identity of the UI entry that owns one thread subscription stack.
///
/// The variant is part of the identity, so a transient deep-link render key can
/// never alias a real [`ColumnId`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ThreadOwnerId {
    /// A thread rendered inside a persistent deck column.
    Column(ColumnId),
    /// A thread rendered by one transient global deep-link entry.
    DeepLink(DeepLinkId),
}

impl From<ColumnId> for ThreadOwnerId {
    fn from(column_id: ColumnId) -> Self {
        Self::Column(column_id)
    }
}

/// Stable owner key for onboarding remote subscriptions within one column.
pub fn onboarding_owner_key(col: ColumnId) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::Onboarding)
        .with(col)
        .finish()
}

/// Stable owner key for one thread scope within an account and UI owner,
/// including transient deep-link owners.
pub(crate) fn thread_owner_scope_key(
    account_pk: Pubkey,
    owner: ThreadOwnerId,
    root_id: &NoteId,
    scope_id: u64,
) -> SubOwnerKey {
    SubOwnerKey::builder(ColumnsOwner::ThreadScope)
        .with(account_pk)
        .with(owner)
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
