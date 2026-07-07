use enostr::{Pubkey, RelayRoutingPreference};
use nostrdb::{Filter, FilterField};
use notedeck::{ScopedSubApi, SubConfig, SubKey};

use crate::{
    scoped_sub_owner_keys::timeline_remote_owner_key,
    timeline::{Timeline, TimelineKind},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::timeline) enum TimelineScopedSub {
    RemoteBaselineByKind,
}

/// Columns policy for remote timeline and thread subscription declarations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteSubscriptionPolicy {
    use_outbox_relays: bool,
}

impl RemoteSubscriptionPolicy {
    /// Build the remote subscription policy from the Columns outbox-relay setting.
    pub fn from_outbox_relays(use_outbox_relays: bool) -> Self {
        Self { use_outbox_relays }
    }

    /// Return whether author-outbox coverage should augment these remote filters.
    pub(crate) fn uses_author_outbox_for_filters(self, remote_filters: &[Filter]) -> bool {
        self.use_outbox_relays && remote_filters_have_authors(remote_filters)
    }

    /// Return whether observed relay coverage should augment selected-account read relays.
    pub(crate) fn uses_observed_relay_coverage(self, has_observed_relays: bool) -> bool {
        self.use_outbox_relays && has_observed_relays
    }
}

pub(in crate::timeline) fn timeline_remote_sub_key(
    kind: &TimelineKind,
    sub: TimelineScopedSub,
) -> SubKey {
    SubKey::builder(sub).with(kind).finish()
}

/// Returns true when any remote filter has a non-empty `authors` field.
fn remote_filters_have_authors(remote_filters: &[Filter]) -> bool {
    remote_filters.iter().any(|filter| {
        filter.into_iter().any(|field| match field {
            FilterField::Authors(authors) => authors.count() > 0,
            _ => false,
        })
    })
}

fn timeline_remote_sub_config(
    kind: &TimelineKind,
    live_filters: Vec<Filter>,
    routing_preference: RelayRoutingPreference,
    remote_policy: RemoteSubscriptionPolicy,
) -> SubConfig {
    let use_author_outbox = remote_policy.uses_author_outbox_for_filters(&live_filters);
    let builder = SubConfig::builder(live_filters);
    let accounts_read = if matches!(kind, TimelineKind::Notifications(_)) {
        builder.accounts_read_critical_with_preference(routing_preference)
    } else {
        builder.accounts_read_important_with_preference(routing_preference)
    };

    if use_author_outbox {
        // Author filters need selected-account read relays for baseline delivery; the
        // runtime adds author write-relay coverage under the same scoped key.
        return accounts_read.with_author_outbox_augmentation().build();
    }

    accounts_read.build()
}

pub(in crate::timeline) fn timeline_remote_sub_declaration(
    kind: &TimelineKind,
    live_filters: Vec<Filter>,
    routing_preference: RelayRoutingPreference,
    remote_policy: RemoteSubscriptionPolicy,
) -> (SubKey, SubConfig) {
    (
        timeline_remote_sub_key(kind, TimelineScopedSub::RemoteBaselineByKind),
        timeline_remote_sub_config(kind, live_filters, routing_preference, remote_policy),
    )
}

pub(crate) fn ensure_remote_timeline_subscription(
    timeline: &mut Timeline,
    account_pk: Pubkey,
    remote_filters: Vec<Filter>,
    scoped_subs: &mut ScopedSubApi<'_>,
    remote_policy: RemoteSubscriptionPolicy,
) {
    if remote_filters.is_empty() {
        clear_remote_timeline_subscription_for_account(timeline, account_pk, scoped_subs);
        return;
    }

    let owner = timeline_remote_owner_key(account_pk, &timeline.kind);
    timeline.remote_subscription_filters = Some(remote_filters.clone());
    let (key, config) = timeline_remote_sub_declaration(
        &timeline.kind,
        remote_filters,
        if matches!(&timeline.kind, TimelineKind::Notifications(_)) {
            RelayRoutingPreference::RequireDedicated
        } else {
            RelayRoutingPreference::default()
        },
        remote_policy,
    );
    let _ = scoped_subs.ensure_sub_for_account(account_pk, owner, key, config);
    timeline.subscription.mark_remote_registered(account_pk);
}

pub(crate) fn update_remote_timeline_subscription(
    timeline: &mut Timeline,
    remote_filters: Vec<Filter>,
    scoped_subs: &mut ScopedSubApi<'_>,
    remote_policy: RemoteSubscriptionPolicy,
) {
    update_remote_timeline_subscription_for_account(
        timeline,
        scoped_subs.selected_account_pubkey(),
        remote_filters,
        scoped_subs,
        remote_policy,
    );
}

pub(crate) fn update_remote_timeline_subscription_for_account(
    timeline: &mut Timeline,
    account_pk: Pubkey,
    remote_filters: Vec<Filter>,
    scoped_subs: &mut ScopedSubApi<'_>,
    remote_policy: RemoteSubscriptionPolicy,
) {
    if remote_filters.is_empty() {
        clear_remote_timeline_subscription_for_account(timeline, account_pk, scoped_subs);
        return;
    }

    let owner = timeline_remote_owner_key(account_pk, &timeline.kind);
    timeline.remote_subscription_filters = Some(remote_filters.clone());
    let (key, config) = timeline_remote_sub_declaration(
        &timeline.kind,
        remote_filters,
        if matches!(&timeline.kind, TimelineKind::Notifications(_)) {
            RelayRoutingPreference::RequireDedicated
        } else {
            RelayRoutingPreference::default()
        },
        remote_policy,
    );
    let _ = scoped_subs.set_sub_for_account(account_pk, owner, key, config);
    timeline.subscription.mark_remote_registered(account_pk);
}

fn clear_remote_timeline_subscription_for_account(
    timeline: &mut Timeline,
    account_pk: Pubkey,
    scoped_subs: &mut ScopedSubApi<'_>,
) {
    timeline.remote_subscription_filters = None;
    timeline.subscription.mark_remote_pending(account_pk);
    drop_timeline_remote_owner(timeline, account_pk, scoped_subs);
}

pub(crate) fn drop_timeline_remote_owner(
    timeline: &Timeline,
    account_pk: Pubkey,
    scoped_subs: &mut ScopedSubApi<'_>,
) {
    let owner = timeline_remote_owner_key(account_pk, &timeline.kind);
    let _ = scoped_subs.drop_owner(owner);
}
