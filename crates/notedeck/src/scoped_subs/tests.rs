use super::*;
use crate::{
    remote_data::RemoteIntentBatchBuilder,
    test_utils::RemoteOutboxReadModelHarness,
    test_utils::{
        nip65_write_relay_note_at_for_test, nip65_write_relay_note_for_test,
        wait_for_nip65_at_for_test, wait_for_nip65_for_test,
    },
    Accounts, UnknownIds, FALLBACK_PUBKEY,
};
use config::{ResolvedSubScope, ScopedSubKey, SubExecution};
use enostr::{
    NormRelayUrl, OutboxEvent, OutboxIdRegistry, OutboxSubId, OutboxSubRelayEose, Pubkey,
    RelayDemandPriority, RelayLegReadiness, RelayReqStatus, RelayRoutingPreference, RelayUrlSource,
};
use hashbrown::HashSet;
use nostrdb::{Config, Filter, SendFilter};
use std::hash::Hash;
use tempfile::TempDir;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FakeApp {
    Timelines,
    Threads,
    Messages,
}

macro_rules! test_owner {
    () => {
        SubOwnerKey::new((module_path!(), line!(), column!()))
    };
}

fn scoped_sub_test_runtime() -> (ScopedSubRuntime, RemoteOutboxReadModelHarness) {
    let bridge = RemoteOutboxReadModelHarness::default();
    let runtime = bridge.scoped_runtime();
    (runtime, bridge)
}

fn relay_policy(
    priority: RelayDemandPriority,
    routing_preference: RelayRoutingPreference,
) -> SubRelayPolicy {
    SubRelayPolicy::new(priority, routing_preference)
}

fn base_config(_scope: SubScope) -> SubConfig {
    SubConfig::builder(vec![Filter::new().kinds(vec![1]).limit(5).build()])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::default(),
        ))
        .build()
}

fn live_config(scope: SubScope) -> SubConfig {
    base_config(scope)
}

fn send_filter(filter: Filter) -> SendFilter {
    SendFilter::try_from_filter(filter).expect("test filter should be sendable")
}

fn send_filters(filters: Vec<Filter>) -> Vec<SendFilter> {
    filters.into_iter().map(send_filter).collect()
}

#[test]
#[should_panic(expected = "SubConfig requires at least one filter")]
fn sub_config_builder_rejects_empty_filter_set() {
    let _ = SubConfig::builder(Vec::new())
        .accounts_read_important()
        .build();
}

#[test]
#[should_panic(expected = "SubConfig requires at least one filter")]
fn sub_config_builder_rejects_filters_empty_after_normalization() {
    let _ = SubConfig::builder(vec![Filter::new().build()])
        .accounts_read_important()
        .build();
}

#[test]
#[should_panic(expected = "SubConfig requires sendable filters")]
fn sub_config_builder_rejects_custom_live_filter() {
    let _ = SubConfig::builder(vec![Filter::new().custom(|_| true).build()])
        .accounts_read_important()
        .build();
}

#[test]
#[should_panic(expected = "SubConfig requires sendable full-history filters")]
fn sub_config_builder_rejects_custom_full_history_filter() {
    let _ = SubConfig::builder(vec![Filter::new().kinds([1]).build()])
        .full_history(enostr::FullHistoryConfig::new(vec![Filter::new()
            .custom(|_| true)
            .build()]))
        .accounts_read_important()
        .build();
}

fn relay_set(url: &str) -> HashSet<NormRelayUrl> {
    let mut relays = HashSet::new();
    relays.insert(NormRelayUrl::new(url).unwrap());
    relays
}

fn aggregate_relay_eose_status(
    relay_statuses: impl IntoIterator<Item = RelayLegReadiness>,
) -> ScopedSubRelayEoseStatus {
    let mut tracked_relays = 0usize;
    let mut unsupported_relays = 0usize;
    let mut any_eose = false;
    let mut all_eosed = true;

    for status in relay_statuses {
        match status {
            RelayLegReadiness::Placed(RelayReqStatus::Eose) => {
                tracked_relays += 1;
                any_eose = true;
            }
            RelayLegReadiness::Placed(_) | RelayLegReadiness::PendingPlacement => {
                tracked_relays += 1;
                all_eosed = false;
            }
            RelayLegReadiness::Unsupported => {
                unsupported_relays += 1;
            }
        }
    }

    if tracked_relays == 0 {
        all_eosed = false;
    }

    ScopedSubRelayEoseStatus {
        tracked_relays,
        unsupported_relays,
        any_eose,
        all_eosed,
    }
}

fn account_pk(tag: u8) -> Pubkey {
    Pubkey::new([tag; 32])
}

#[test]
fn full_history_relay_packages_preserve_observed_relay_source() {
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let account_relay = NormRelayUrl::new("wss://account-read.example.com").unwrap();
    let overlap_relay = NormRelayUrl::new("wss://overlap.example.com").unwrap();
    let observed_extra = NormRelayUrl::new("wss://observed-extra.example.com").unwrap();
    let account_read_relays = HashSet::from([account_relay.clone(), overlap_relay.clone()]);
    let observed_relays = [overlap_relay, observed_extra.clone()];
    let spec = SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .accounts_read_important()
        .with_observed_relays(observed_relays)
        .build();

    let relay_pkgs = planner::full_history_relay_pkgs(&account_read_relays, &spec);

    assert_eq!(relay_pkgs.len(), 2);
    let account_pkg = relay_pkgs
        .iter()
        .find(|relay_pkgs| relay_pkgs.source() == RelayUrlSource::Explicit)
        .expect("account-read full-history package");
    assert_eq!(account_pkg.urls(), &account_read_relays);
    assert_eq!(
        account_pkg.demand_priority(),
        RelayDemandPriority::Important
    );
    assert_eq!(
        account_pkg.routing_preference(),
        RelayRoutingPreference::PreferDedicated
    );

    let observed_pkg = relay_pkgs
        .iter()
        .find(|relay_pkgs| relay_pkgs.source() == RelayUrlSource::RemoteAdvertised)
        .expect("observed full-history package");
    assert_eq!(observed_pkg.urls(), &HashSet::from([observed_extra]));
    assert_eq!(
        observed_pkg.demand_priority(),
        RelayDemandPriority::Opportunistic
    );
    assert_eq!(
        observed_pkg.routing_preference(),
        RelayRoutingPreference::NoPreference
    );
}

#[test]
fn full_history_relay_packages_for_explicit_only_ignore_account_read_relays() {
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let account_relay = NormRelayUrl::new("wss://account-read.example.com").unwrap();
    let explicit_relay = NormRelayUrl::new("wss://explicit.example.com").unwrap();
    let account_read_relays = HashSet::from([account_relay]);
    let spec = SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .explicit(
            [explicit_relay.clone()],
            SubRelayPolicy::accounts_read_important(),
        )
        .build();

    let relay_pkgs = planner::full_history_relay_pkgs(&account_read_relays, &spec);

    assert_eq!(relay_pkgs.len(), 1);
    assert_eq!(relay_pkgs[0].source(), RelayUrlSource::Explicit);
    assert_eq!(relay_pkgs[0].urls(), &HashSet::from([explicit_relay]));
}

#[test]
fn full_history_relay_packages_drop_blocked_observed_relays() {
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let blocked_observed = NormRelayUrl::new("wss://127.0.0.1").unwrap();
    let spec = SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .accounts_read_important()
        .with_observed_relays([blocked_observed])
        .build();

    let relay_pkgs = planner::full_history_relay_pkgs(&HashSet::new(), &spec);

    assert!(relay_pkgs.is_empty());
}

#[test]
fn author_outbox_full_history_targets_preserve_routed_filter_projection() {
    let author_a = account_pk(0xA1);
    let author_b = account_pk(0xB2);
    let account_relay = NormRelayUrl::new("wss://account-read.example.com").unwrap();
    let author_relay_a = NormRelayUrl::new("wss://author-a.example.com").unwrap();
    let author_relay_b = NormRelayUrl::new("wss://author-b.example.com").unwrap();
    let history_filter = Filter::new()
        .authors([author_a.bytes(), author_b.bytes()])
        .kinds([1])
        .build();
    let routed_filter_a = Filter::new().authors([author_a.bytes()]).kinds([1]).build();
    let routed_filter_b = Filter::new().authors([author_b.bytes()]).kinds([1]).build();
    let spec = SubConfig::builder(vec![history_filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![history_filter]))
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .build();
    let author_outbox_routes = planner::PlannedAuthorOutboxRoutes::from_routed_filters(
        Vec::new(),
        vec![
            crate::author_outbox::RoutedFilter {
                relay: author_relay_a.clone(),
                filter_index: 0,
                filter: routed_filter_a,
                relay_priority: crate::author_outbox::RoutedRelayPriority::default(),
            },
            crate::author_outbox::RoutedFilter {
                relay: author_relay_b.clone(),
                filter_index: 0,
                filter: routed_filter_b,
                relay_priority: crate::author_outbox::RoutedRelayPriority::default(),
            },
        ],
    );

    let targets = planner::full_history_targets_with_author_routes(
        &HashSet::from([account_relay.clone()]),
        &spec,
        Some(&author_outbox_routes),
    );

    let author_targets = targets
        .iter()
        .filter(|target| {
            target
                .relay_pkgs()
                .iter()
                .any(|relay_pkgs| relay_pkgs.source() == RelayUrlSource::RemoteAdvertised)
        })
        .collect::<Vec<_>>();
    assert_eq!(author_targets.len(), 2);
    assert!(author_targets.iter().any(|target| {
        target
            .relay_pkgs()
            .iter()
            .any(|relay_pkgs| relay_pkgs.urls() == &HashSet::from([author_relay_a.clone()]))
            && target
                .filters()
                .iter()
                .flat_map(crate::author_outbox::filter_author_pubkeys)
                .collect::<HashSet<_>>()
                == HashSet::from([author_a])
    }));
    assert!(author_targets.iter().any(|target| {
        target
            .relay_pkgs()
            .iter()
            .any(|relay_pkgs| relay_pkgs.urls() == &HashSet::from([author_relay_b.clone()]))
            && target
                .filters()
                .iter()
                .flat_map(crate::author_outbox::filter_author_pubkeys)
                .collect::<HashSet<_>>()
                == HashSet::from([author_b])
    }));
}

#[test]
fn author_outbox_demand_includes_full_history_authors() {
    let live_author = account_pk(0xC1);
    let history_author = account_pk(0xC2);
    let live_filter = Filter::new()
        .authors([live_author.bytes()])
        .kinds([1])
        .limit(10)
        .build();
    let history_filter = Filter::new()
        .authors([history_author.bytes()])
        .kinds([1])
        .build();
    let config = SubConfig::builder(vec![live_filter])
        .full_history(enostr::FullHistoryConfig::new(vec![history_filter]))
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .build();

    assert_eq!(
        author_runtime::sub_config_author_pubkeys(&config),
        HashSet::from([live_author, history_author])
    );
}

#[test]
fn sub_config_equality_uses_canonical_filters() {
    let pk_a = account_pk(0x11);
    let pk_b = account_pk(0x22);
    let left_filter = Filter::new()
        .authors([pk_a.bytes(), pk_b.bytes()])
        .kinds([1, 6])
        .limit(25)
        .build();
    let right_filter = Filter::new()
        .limit(25)
        .kinds([6, 1])
        .authors([pk_b.bytes(), pk_a.bytes()])
        .build();
    let policy = relay_policy(
        RelayDemandPriority::Important,
        RelayRoutingPreference::PreferDedicated,
    );

    let left = SubConfig::builder(vec![left_filter])
        .accounts_read(policy)
        .build();
    let right = SubConfig::builder(vec![right_filter])
        .accounts_read(policy)
        .build();

    assert_eq!(left, right);

    let kind_one = Filter::new().kinds([1]).limit(10).build();
    let kind_six = Filter::new().kinds([6]).limit(10).build();

    let left = SubConfig::builder(vec![kind_one.clone(), kind_six.clone()])
        .accounts_read(policy)
        .build();
    let right = SubConfig::builder(vec![kind_six, kind_one.clone()])
        .accounts_read(policy)
        .build();
    assert_eq!(left, right);

    let duplicated = SubConfig::builder(vec![kind_one.clone(), kind_one])
        .accounts_read(policy)
        .build();
    assert_ne!(left, duplicated);
}

fn make_key(parts: impl Hash) -> SubKey {
    SubKey::new(parts)
}

fn new_ndb() -> (TempDir, nostrdb::Ndb) {
    let tmp = TempDir::new().expect("tmp dir");
    let ndb = nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    (tmp, ndb)
}

fn test_accounts(ndb: &mut nostrdb::Ndb, txn: &nostrdb::Transaction) -> Accounts {
    let mut unknown_ids = UnknownIds::default();
    Accounts::new(
        None,
        Vec::new(),
        Vec::new(),
        FALLBACK_PUBKEY(),
        ndb,
        txn,
        &mut unknown_ids,
    )
}

struct PlannerFixture {
    _tmp: TempDir,
    ndb: nostrdb::Ndb,
}

impl PlannerFixture {
    fn new(tmp: TempDir, ndb: nostrdb::Ndb) -> Self {
        Self { _tmp: tmp, ndb }
    }

    fn ndb(&self) -> nostrdb::Ndb {
        self.ndb.clone()
    }
}

fn planner_fixture_with_local_relay_lists(
    entries: Vec<(enostr::FullKeypair, Vec<&str>)>,
) -> PlannerFixture {
    let (tmp, ndb) = new_ndb();
    for (account, relays) in &entries {
        let note = nip65_write_relay_note_for_test(account, relays);
        ndb.process_client_event(&note.json().expect("json"))
            .expect("ingest nip65");
        wait_for_nip65_for_test(&ndb, &account.pubkey);
    }
    PlannerFixture::new(tmp, ndb)
}

fn accountsread_spec(_scope: SubScope, kind: u64, limit: u64) -> SubConfig {
    SubConfig::builder(vec![Filter::new().kinds(vec![kind]).limit(limit).build()])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::default(),
        ))
        .build()
}

fn advance_author_outbox_plans_for_test(
    runtime: &mut ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    selected_account_pubkey: Pubkey,
    baseline_relays: &HashSet<NormRelayUrl>,
    ndb: &nostrdb::Ndb,
) {
    for _ in 0..8 {
        advance_author_outbox_plans_once_for_test(
            runtime,
            bridge,
            selected_account_pubkey,
            baseline_relays,
            ndb,
        );
    }
}

fn advance_author_outbox_plans_once_for_test(
    runtime: &mut ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    selected_account_pubkey: Pubkey,
    baseline_relays: &HashSet<NormRelayUrl>,
    ndb: &nostrdb::Ndb,
) {
    let (_, effects) = bridge.with_returned_outbox(|ids| {
        runtime.apply_author_outbox_plans_for_active_scoped_keys_with_effects(
            ids,
            selected_account_pubkey,
            baseline_relays,
        )
    });
    apply_scoped_effects_for_test(
        runtime,
        bridge,
        selected_account_pubkey,
        baseline_relays,
        ndb,
        effects,
    );
}

fn apply_scoped_effects_for_test(
    runtime: &mut ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    selected_account_pubkey: Pubkey,
    account_read_relays: &HashSet<NormRelayUrl>,
    ndb: &nostrdb::Ndb,
    effects: ScopedSubEffects,
) {
    for effect in effects.into_effects() {
        match effect {
            ScopedSubEffect::StartAuthorOutboxPlanJob(request) => {
                let completion = request.run(ndb.clone());
                let delta = runtime.apply_author_outbox_plan_completed(
                    selected_account_pubkey,
                    account_read_relays,
                    completion,
                );
                let next_effects = bridge.ingest_scoped_delta(delta);
                apply_scoped_effects_for_test(
                    runtime,
                    bridge,
                    selected_account_pubkey,
                    account_read_relays,
                    ndb,
                    next_effects,
                );
            }
        }
    }
}

fn collect_scoped_effect_outbox_ops_for_test(
    runtime: &mut ScopedSubRuntime,
    selected_account_pubkey: Pubkey,
    account_read_relays: &HashSet<NormRelayUrl>,
    ndb: &nostrdb::Ndb,
    effects: ScopedSubEffects,
) -> ScopedSubOutboxOps {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    for effect in effects.into_effects() {
        match effect {
            ScopedSubEffect::StartAuthorOutboxPlanJob(request) => {
                let completion = request.run(ndb.clone());
                let delta = runtime.apply_author_outbox_plan_completed(
                    selected_account_pubkey,
                    account_read_relays,
                    completion,
                );
                let (_output, ops, next_effects) = delta.into_parts();
                outbox_ops.extend(ops);
                outbox_ops.extend(collect_scoped_effect_outbox_ops_for_test(
                    runtime,
                    selected_account_pubkey,
                    account_read_relays,
                    ndb,
                    next_effects,
                ));
            }
        }
    }
    outbox_ops
}

#[allow(clippy::too_many_arguments)]
fn set_sub_with_relays_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    selected_account_pubkey: Pubkey,
    owner: SubOwnerKey,
    scope: SubScope,
    key: SubKey,
    config: SubConfig,
) -> (SetSubResult, ScopedSubOutboxOps) {
    let (result, outbox_ops, _) = runtime.set_sub_with_relays_with_effects(
        ids,
        account_read_relays,
        selected_account_pubkey,
        owner,
        scope,
        key,
        config,
    );
    (result, outbox_ops)
}

#[allow(clippy::too_many_arguments)]
fn set_inactive_account_sub_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    account_pubkey: Pubkey,
    owner: SubOwnerKey,
    key: SubKey,
    config: SubConfig,
) -> (SetSubResult, ScopedSubOutboxOps) {
    let (result, outbox_ops, _) = runtime.set_inactive_account_sub_with_effects(
        ids,
        selected_account_pubkey,
        account_pubkey,
        owner,
        key,
        config,
    );
    (result, outbox_ops)
}

fn clear_sub_with_selected_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    owner: SubOwnerKey,
    key: SubKey,
    scope: SubScope,
) -> (ClearSubResult, ScopedSubOutboxOps) {
    let (result, outbox_ops, _) = runtime.clear_owner_config_for_account(
        ids,
        selected_account_pubkey,
        &HashSet::new(),
        selected_account_pubkey,
        owner,
        key,
        scope,
    );
    (result, outbox_ops)
}

fn drop_owner_with_relays_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    account_read_relays: &HashSet<NormRelayUrl>,
    owner: SubOwnerKey,
) -> (bool, ScopedSubOutboxOps) {
    let (changed, outbox_ops, _) = runtime.drop_owner_with_relays_collect(
        ids,
        selected_account_pubkey,
        account_read_relays,
        owner,
    );
    (!changed.is_empty(), outbox_ops)
}

fn on_account_switched_with_relays_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    old_pk: Pubkey,
    new_pk: Pubkey,
    new_account_read_relays: &HashSet<NormRelayUrl>,
) -> ScopedSubOutboxOps {
    runtime
        .on_account_switched_with_relays_with_effects(ids, old_pk, new_pk, new_account_read_relays)
        .0
}

fn retarget_selected_account_read_relays_with_relays_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    account_read_relays: &HashSet<NormRelayUrl>,
) -> ScopedSubOutboxOps {
    runtime
        .retarget_selected_account_read_relays_with_effects(
            ids,
            selected_account_pubkey,
            account_read_relays,
        )
        .0
}

fn apply_author_outbox_plans_for_active_scoped_keys_for_test(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    baseline_relays: &HashSet<NormRelayUrl>,
) -> ScopedSubOutboxOps {
    runtime
        .apply_author_outbox_plans_for_active_scoped_keys_with_effects(
            ids,
            selected_account_pubkey,
            baseline_relays,
        )
        .0
}

#[allow(clippy::too_many_arguments)]
fn set_sub_and_apply_effects_for_test(
    runtime: &mut ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    account_read_relays: &HashSet<NormRelayUrl>,
    selected_account_pubkey: Pubkey,
    owner: SubOwnerKey,
    scope: SubScope,
    key: SubKey,
    config: SubConfig,
    ndb: &nostrdb::Ndb,
) -> SetSubResult {
    let (result, effects) = bridge.with_returned_outbox(|ids| {
        runtime.set_sub_with_relays_with_effects(
            ids,
            account_read_relays,
            selected_account_pubkey,
            owner,
            scope,
            key,
            config,
        )
    });
    apply_scoped_effects_for_test(
        runtime,
        bridge,
        selected_account_pubkey,
        account_read_relays,
        ndb,
        effects,
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn set_sub_with_planner_fixture(
    runtime: &mut ScopedSubRuntime,
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    selected_account_pubkey: Pubkey,
    planner_fixture: Option<&PlannerFixture>,
    owner: SubOwnerKey,
    scope: SubScope,
    key: SubKey,
    config: SubConfig,
) -> (SetSubResult, ScopedSubOutboxOps) {
    let (result, mut outbox_ops, effects) = runtime.set_sub_with_relays_with_effects(
        ids,
        account_read_relays,
        selected_account_pubkey,
        owner,
        scope,
        key,
        config,
    );
    if let Some(planner_fixture) = planner_fixture {
        outbox_ops.extend(collect_scoped_effect_outbox_ops_for_test(
            runtime,
            selected_account_pubkey,
            account_read_relays,
            &planner_fixture.ndb(),
            effects,
        ));
    }
    (result, outbox_ops)
}

fn realize_author_outbox_plan_for_test(
    runtime: &mut ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    selected_account_pubkey: Pubkey,
    baseline_relays: &HashSet<NormRelayUrl>,
    planner_fixture: &PlannerFixture,
) {
    for _ in 0..4 {
        let (_, effects) = bridge.with_returned_outbox(|ids| {
            runtime.apply_author_outbox_plans_for_active_scoped_keys_with_effects(
                ids,
                selected_account_pubkey,
                baseline_relays,
            )
        });
        apply_scoped_effects_for_test(
            runtime,
            bridge,
            selected_account_pubkey,
            baseline_relays,
            &planner_fixture.ndb(),
            effects,
        );
    }
}

fn owner_status(
    runtime: &ScopedSubRuntime,
    bridge: &mut RemoteOutboxReadModelHarness,
    selected_account_pubkey: Pubkey,
    slot: SubOwnerKey,
    key: SubKey,
    scope: SubScope,
) -> ScopedSubReadiness {
    runtime.sub_readiness_with_selected(
        &|id| bridge.outbox_sub_relay_eose(id),
        selected_account_pubkey,
        slot,
        key,
        scope,
    )
}

fn single_live_id(runtime: &ScopedSubRuntime, scoped: &ScopedSubKey) -> OutboxSubId {
    runtime.single_live_id_for_scoped_for_test(scoped)
}

fn routed_leg_id_for_relay(
    legs: &[(NormRelayUrl, OutboxSubId)],
    relay: &NormRelayUrl,
) -> OutboxSubId {
    let matches = legs
        .iter()
        .filter_map(|(leg_relay, live_id)| (leg_relay == relay).then_some(*live_id))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one routed leg for {relay}"
    );
    matches[0]
}

fn remote_history_relay_count(runtime: &ScopedSubRuntime, scoped: &ScopedSubKey) -> usize {
    runtime
        .full_history_targets_for_test(scoped)
        .expect("full-history relay packages")
        .iter()
        .flat_map(|target| target.relay_pkgs())
        .filter(|pkg| pkg.source() == RelayUrlSource::RemoteAdvertised)
        .map(|pkg| pkg.urls().len())
        .sum()
}

fn remote_history_remote_relays(
    runtime: &ScopedSubRuntime,
    scoped: &ScopedSubKey,
) -> HashSet<NormRelayUrl> {
    runtime
        .full_history_targets_for_test(scoped)
        .expect("full-history relay packages")
        .iter()
        .flat_map(|target| target.relay_pkgs())
        .filter(|pkg| pkg.source() == RelayUrlSource::RemoteAdvertised)
        .flat_map(|pkg| pkg.urls().iter().cloned().collect::<Vec<_>>())
        .collect()
}

fn full_history_has_relay_pkg(
    runtime: &ScopedSubRuntime,
    scoped: &ScopedSubKey,
    source: RelayUrlSource,
    relays: &HashSet<NormRelayUrl>,
) -> bool {
    runtime
        .full_history_targets_for_test(scoped)
        .expect("full-history targets")
        .iter()
        .flat_map(|target| target.relay_pkgs())
        .any(|pkg| pkg.source() == source && pkg.urls() == relays)
}

fn desired_explicit_relays(
    runtime: &ScopedSubRuntime,
    scoped: &ScopedSubKey,
) -> HashSet<NormRelayUrl> {
    let Some(config) = runtime.desired_for_test(scoped) else {
        return HashSet::new();
    };

    match &config.execution {
        SubExecution::AccountsReadPlusExplicit {
            explicit_relays, ..
        }
        | SubExecution::Explicit {
            relays: explicit_relays,
            ..
        } => explicit_relays.clone(),
        SubExecution::AccountsRead { .. } | SubExecution::AccountsReadWithAuthorOutbox { .. } => {
            HashSet::new()
        }
    }
}

fn assert_routed_live_id_author_sets(
    runtime: &ScopedSubRuntime,
    selected_account_pubkey: Pubkey,
    key: SubKey,
    scope: SubScope,
    live_id: OutboxSubId,
    expected: Vec<Vec<String>>,
) {
    let mut actual = runtime
        .routed_live_author_sets_for_test(selected_account_pubkey, key, scope)
        .into_iter()
        .find_map(|(_, id, authors)| (id == live_id).then_some(authors))
        .expect("routed live id")
        .into_iter()
        .map(|authors| {
            authors
                .into_iter()
                .map(|author| author.hex())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = expected;
    for authors in &mut expected {
        authors.sort();
    }
    expected.sort();
    assert_eq!(actual, expected);
}

fn author_filter(author: Pubkey, kind: u64) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds([kind])
        .limit(10)
        .build()
}

fn contact_outbox_config(filter: Filter) -> SubConfig {
    author_outbox_config(vec![filter])
}

fn author_outbox_config(filters: Vec<Filter>) -> SubConfig {
    SubConfig::builder(filters)
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ))
        .build()
}

fn author_outbox_full_history_config(filter: Filter) -> SubConfig {
    SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ))
        .build()
}

fn accounts_read_plus_explicit_config(
    filters: Vec<Filter>,
    relays: HashSet<NormRelayUrl>,
) -> SubConfig {
    SubConfig::builder(filters)
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_explicit_relays(
            relays,
            relay_policy(
                RelayDemandPriority::Opportunistic,
                RelayRoutingPreference::NoPreference,
            ),
        )
        .build()
}

fn accounts_read_plus_observed_config(
    filters: Vec<Filter>,
    relays: HashSet<NormRelayUrl>,
) -> SubConfig {
    SubConfig::builder(filters)
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_observed_relays(relays)
        .build()
}

/// Verifies repeated set_sub calls for the same key report true no-op semantics.
#[test]
fn set_sub_is_upsert_for_existing_key() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let key = SubKey::new(("messages", "dm-list", 7u8));
    let scope = SubScope::Global;
    let slot = test_owner!();

    let first = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            base_config(scope),
        )
    });
    let second = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            base_config(scope),
        )
    });

    assert!(matches!(first, SetSubResult::Created));
    assert!(matches!(second, SetSubResult::Unchanged));
    assert_eq!(runtime.desired_len(), 1);
    assert_eq!(runtime.live_len(), 1);
    assert_eq!(runtime.owner_len(), 1);
}

#[test]
fn active_same_config_set_sub_does_not_realize_missing_live_state() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let owner = test_owner!();
    let key = make_key(("same-config-active-no-live", 1u8));
    let config = base_config(SubScope::Global);

    let created = bridge.with_returned_outbox(|ids| {
        let (result, outbox_ops) = set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            owner,
            SubScope::Global,
            key,
            config.clone(),
        );
        assert!(outbox_ops.is_empty());
        (result, outbox_ops)
    });

    let repeated = bridge.with_returned_outbox(|ids| {
        let (result, outbox_ops) = set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relay_set("wss://relay-a.example.com"),
            selected,
            owner,
            SubScope::Global,
            key,
            config,
        );
        assert!(outbox_ops.is_empty());
        (result, outbox_ops)
    });

    assert_eq!(created, SetSubResult::Created);
    assert_eq!(repeated, SetSubResult::Unchanged);
    assert_eq!(runtime.live_len(), 0);
}

#[test]
fn inactive_same_config_set_sub_does_not_release_stale_live_state() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let inactive_account = account_pk(0x02);
    let owner = test_owner!();
    let key = make_key(("same-config-inactive-stale-live", 1u8));
    let relays = relay_set("wss://relay-a.example.com");
    let config = base_config(SubScope::Account);

    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            inactive_account,
            owner,
            SubScope::Account,
            key,
            config.clone(),
        )
    });

    let repeated = bridge.with_returned_outbox(|ids| {
        let (result, outbox_ops) = set_inactive_account_sub_for_test(
            &mut runtime,
            ids,
            selected,
            inactive_account,
            owner,
            key,
            config,
        );
        assert!(outbox_ops.is_empty());
        (result, outbox_ops)
    });

    assert_eq!(created, SetSubResult::Created);
    assert_eq!(repeated, SetSubResult::Unchanged);
    assert_eq!(runtime.live_len(), 1);
}

fn ensure_owner_config_command(
    account_pubkey: Pubkey,
    owner: SubOwnerKey,
    scope: SubScope,
    key: SubKey,
    config: &SubConfig,
) -> ScopedSubCommand {
    ScopedSubCommand::EnsureOwnerConfig {
        account_pubkey,
        owner,
        scope,
        key,
        config: config.clone(),
    }
}

#[test]
fn ensure_owner_config_creates_missing_desired_state() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let account = account_pk(0x01);
    let key = SubKey::new(("messages", "dm-list", 8u8));
    let owner = test_owner!();
    let config = base_config(SubScope::Global);
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);

    let delta = runtime.apply_command(
        account,
        &relays,
        ensure_owner_config_command(account, owner, SubScope::Global, key, &config),
    );
    let effects = bridge.ingest_scoped_delta(delta);

    assert!(effects.into_effects().is_empty());
    assert_eq!(runtime.desired_len(), 1);
    assert_eq!(runtime.live_len(), 1);
    assert_eq!(runtime.owner_len(), 1);
    assert_eq!(runtime.desired_for_test(&scoped), Some(&config));
}

/// Verifies repeated `EnsureOwnerConfig` commands attach an owner once.
#[test]
fn ensure_owner_config_for_existing_key_is_idempotent() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let key = SubKey::new(("messages", "dm-list", 9u8));
    let config_owner = test_owner!();
    let added_owner = test_owner!();
    let config = base_config(SubScope::Global);

    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_pk(0x01),
            config_owner,
            SubScope::Global,
            key,
            config.clone(),
        )
    });
    assert_eq!(created, SetSubResult::Created);

    let first = runtime.apply_command(
        account_pk(0x01),
        &relays,
        ensure_owner_config_command(
            account_pk(0x01),
            added_owner,
            SubScope::Global,
            key,
            &config,
        ),
    );
    let first_effects = bridge.ingest_scoped_delta(first);
    assert!(first_effects.into_effects().is_empty());

    let second = runtime.apply_command(
        account_pk(0x01),
        &relays,
        ensure_owner_config_command(
            account_pk(0x01),
            added_owner,
            SubScope::Global,
            key,
            &config,
        ),
    );
    let (second_output, second_ops, second_effects) = second.into_parts();
    assert!(second_output.is_empty());
    assert!(second_ops.is_empty());
    assert!(second_effects.into_effects().is_empty());

    assert_eq!(runtime.desired_len(), 1);
    assert_eq!(runtime.live_len(), 1);
    assert_eq!(runtime.owner_len(), 2);
}

/// Verifies `EnsureOwnerConfig` does not mutate existing live filter state.
#[test]
fn ensure_owner_config_does_not_modify_existing_live_sub() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let key = SubKey::new(("timeline", "home", 1u8));
    let config_owner = test_owner!();
    let added_owner = test_owner!();

    let mut initial = base_config(SubScope::Global);
    initial.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(10).build()]);

    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_pk(0x01),
            config_owner,
            SubScope::Global,
            key,
            initial,
        )
    });
    assert!(matches!(created, SetSubResult::Created));

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);
    let before = runtime
        .desired_for_test(&scoped)
        .expect("desired config before ensure")
        .clone();

    let delta = runtime.apply_command(
        account_pk(0x01),
        &relays,
        ensure_owner_config_command(
            account_pk(0x01),
            added_owner,
            SubScope::Global,
            key,
            &base_config(SubScope::Global),
        ),
    );
    let effects = bridge.ingest_scoped_delta(delta);
    assert!(effects.into_effects().is_empty());

    assert_eq!(single_live_id(&runtime, &scoped), live_id);
    assert_eq!(
        runtime
            .desired_for_test(&scoped)
            .expect("desired config after ensure"),
        &before
    )
}

#[test]
fn ensure_owner_config_register_only_owner_survives_config_owner_drop() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let account = account_pk(0x01);
    let key = SubKey::new(("timeline", "register-only-owner-drop", 1u8));
    let owner_a = test_owner!();
    let owner_b = test_owner!();
    let mut config_a = base_config(SubScope::Global);
    config_a.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(10).build()]);
    let mut config_b = base_config(SubScope::Global);
    config_b.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(99).build()]);
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);

    let delta = runtime.apply_command(
        account,
        &relays,
        ensure_owner_config_command(account, owner_a, SubScope::Global, key, &config_a),
    );
    let effects = bridge.ingest_scoped_delta(delta);
    assert!(effects.into_effects().is_empty());

    let delta = runtime.apply_command(
        account,
        &relays,
        ensure_owner_config_command(account, owner_b, SubScope::Global, key, &config_b),
    );
    let effects = bridge.ingest_scoped_delta(delta);
    assert!(effects.into_effects().is_empty());
    assert_eq!(runtime.desired_for_test(&scoped), Some(&config_a));
    assert_eq!(runtime.owner_len(), 2);

    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &relays, owner_a)
    }));
    assert_eq!(runtime.desired_for_test(&scoped), Some(&config_a));
    assert_eq!(runtime.owner_len(), 1);

    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &relays, owner_b)
    }));
    assert_eq!(runtime.desired_len(), 0);
    assert_eq!(runtime.owner_len(), 0);
}

/// Verifies aggregate EOSE helper treats zero tracked relays as not fully EOSE'd.
#[test]
fn aggregate_relay_eose_status_zero_tracked_relays_is_not_all_eosed() {
    let status = aggregate_relay_eose_status(std::iter::empty());
    assert_eq!(
        status,
        ScopedSubRelayEoseStatus {
            tracked_relays: 0,
            unsupported_relays: 0,
            any_eose: false,
            all_eosed: false,
        }
    );
}

/// Verifies aggregate EOSE helper reports partial EOSE when relay legs are mixed.
#[test]
fn aggregate_relay_eose_status_mixed_relays_reports_partial_eose() {
    let status = aggregate_relay_eose_status([
        RelayLegReadiness::Placed(RelayReqStatus::InitialQuery),
        RelayLegReadiness::Placed(RelayReqStatus::Eose),
        RelayLegReadiness::Placed(RelayReqStatus::Closed),
    ]);
    assert_eq!(
        status,
        ScopedSubRelayEoseStatus {
            tracked_relays: 3,
            unsupported_relays: 0,
            any_eose: true,
            all_eosed: false,
        }
    );
}

/// Verifies aggregate EOSE helper reports fully EOSE'd only when all tracked relays are EOSE.
#[test]
fn aggregate_relay_eose_status_all_relays_eose_reports_all_eosed() {
    let status = aggregate_relay_eose_status([
        RelayLegReadiness::Placed(RelayReqStatus::Eose),
        RelayLegReadiness::Placed(RelayReqStatus::Eose),
    ]);
    assert_eq!(
        status,
        ScopedSubRelayEoseStatus {
            tracked_relays: 2,
            unsupported_relays: 0,
            any_eose: true,
            all_eosed: true,
        }
    );
}

/// Verifies unsupported relay legs are terminal but not counted as EOSE.
#[test]
fn aggregate_relay_eose_status_tracks_unsupported_relays_separately() {
    let status = aggregate_relay_eose_status([
        RelayLegReadiness::Placed(RelayReqStatus::Eose),
        RelayLegReadiness::Unsupported,
    ]);

    assert_eq!(
        status,
        ScopedSubRelayEoseStatus {
            tracked_relays: 1,
            unsupported_relays: 1,
            any_eose: true,
            all_eosed: true,
        }
    );
}

/// Verifies EOSE status lookup returns Missing when the owner does not own the requested key.
#[test]
fn sub_readiness_missing_when_owner_does_not_own_key() {
    let (runtime, mut bridge) = scoped_sub_test_runtime();
    let status = owner_status(
        &runtime,
        &mut bridge,
        account_pk(0x01),
        make_key(("missing-owner", 999u64)),
        make_key(("missing", 1u8)),
        SubScope::Global,
    );
    assert_eq!(status, ScopedSubReadiness::Missing);
}

/// Verifies live subscriptions expose aggregate EOSE state without leaking outbox ids.
#[test]
fn sub_readiness_live_reports_tracked_relays_and_eose_flags() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let slot = test_owner!();
    let key = make_key(("live", 1u8));
    let selected = account_pk(0x01);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            selected,
            slot,
            SubScope::Global,
            key,
            live_config(SubScope::Global),
        )
    });
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);
    bridge.apply_event(OutboxEvent::OutboxSubRelayEoseChanged {
        id: live_id,
        relay_eose: Some(OutboxSubRelayEose {
            tracked_relays: 1,
            unsupported_relays: 0,
            any_eose: false,
            all_eosed: false,
        }),
    });

    let status = owner_status(&runtime, &mut bridge, selected, slot, key, SubScope::Global);
    let ScopedSubReadiness::Live(live) = status else {
        panic!("expected live status, got {status:?}");
    };

    assert_eq!(live.relay_eose.tracked_relays, 1);
    assert!(!live.relay_eose.any_eose);
    assert!(!live.relay_eose.all_eosed);
}

/// Verifies unsupported desired relay legs remain visible to scoped readiness.
#[test]
fn sub_readiness_live_reports_unsupported_desired_relay_legs() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relay = NormRelayUrl::new("wss://relay-a.example.com").unwrap();
    let relays = HashSet::from([relay.clone()]);
    let slot = test_owner!();
    let key = make_key(("unsupported", 1u8));
    let selected = account_pk(0x01);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            selected,
            slot,
            SubScope::Global,
            key,
            live_config(SubScope::Global),
        )
    });
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);
    bridge.apply_event(OutboxEvent::OutboxSubRelayEoseChanged {
        id: live_id,
        relay_eose: Some(OutboxSubRelayEose {
            tracked_relays: 0,
            unsupported_relays: 1,
            any_eose: false,
            all_eosed: false,
        }),
    });

    let status = owner_status(&runtime, &mut bridge, selected, slot, key, SubScope::Global);
    let ScopedSubReadiness::Live(live) = status else {
        panic!("expected live status, got {status:?}");
    };

    assert_eq!(live.relay_eose.tracked_relays, 0);
    assert_eq!(live.relay_eose.unsupported_relays, 1);
    assert!(!live.relay_eose.any_eose);
    assert!(!live.relay_eose.all_eosed);
}

#[test]
fn outbox_selection_realizes_multiple_live_subs_for_one_logical_key() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-tracer", 1u8));
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();
    let author_c = enostr::FullKeypair::generate();
    let pk_a = author_a.pubkey;
    let pk_b = author_b.pubkey;
    let pk_c = author_c.pubkey;
    let relay_a = NormRelayUrl::new("wss://relay-a.example.com").expect("relay a");
    let relay_b = NormRelayUrl::new("wss://relay-b.example.com").expect("relay b");

    let filter = Filter::new()
        .authors([pk_a.bytes(), pk_b.bytes(), pk_c.bytes()])
        .kinds([1])
        .limit(20)
        .build();

    let directory = planner_fixture_with_local_relay_lists(vec![
        (author_a, vec!["wss://relay-a.example.com"]),
        (author_b, vec!["wss://relay-a.example.com"]),
        (author_c, vec!["wss://relay-b.example.com"]),
    ]);

    let config = author_outbox_config(vec![filter]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );
    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 2);

    let routed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(routed_legs.len(), 2);
    let relay_a_id = routed_leg_id_for_relay(&routed_legs, &relay_a);
    let relay_b_id = routed_leg_id_for_relay(&routed_legs, &relay_b);
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key,
        SubScope::Global,
        relay_a_id,
        vec![vec![pk_a.hex(), pk_b.hex()]],
    );
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key,
        SubScope::Global,
        relay_b_id,
        vec![vec![pk_c.hex()]],
    );

    let status = owner_status(&runtime, &mut bridge, selected, slot, key, SubScope::Global);
    let ScopedSubReadiness::Live(live) = status else {
        panic!("expected live status, got {status:?}");
    };
    assert_eq!(live.relay_eose.tracked_relays, 2);
}

#[test]
fn accounts_read_with_author_outbox_keeps_baseline_for_unresolved_author() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-composite-unresolved", 1u8));
    let author = account_pk(0xA7);
    let account_relays = relay_set("wss://account.example.com");
    let config = contact_outbox_config(author_filter(author, 1));

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            None,
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 1);
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
    assert!(runtime
        .desired_for_test(&ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key))
        .expect("desired config")
        .filters
        .iter()
        .flat_map(|filter| crate::author_outbox::filter_author_pubkeys(filter.as_filter()))
        .eq([author]));
}

#[test]
fn accounts_read_with_author_outbox_adds_routed_leg_for_known_author() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-composite-known", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let author = enostr::FullKeypair::generate();
    let author_relay = NormRelayUrl::new("wss://relay-a.example.com").expect("author relay");
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let config = contact_outbox_config(author_filter(author.pubkey, 1));

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 2);
    assert_eq!(
        runtime.routed_live_legs_for_test(selected, key, SubScope::Global),
        vec![(author_relay, live_ids[1])]
    );
}

#[test]
fn accounts_read_with_author_outbox_counts_routed_leg_as_incomplete_without_committed_eose() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-composite-queued-routed", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let author = enostr::FullKeypair::generate();
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);

    let config = SubConfig::builder(vec![author_filter(author.pubkey, 1)])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::RequireDedicated,
        ))
        .build();
    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let routed = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(routed.len(), 1);

    let status = owner_status(&runtime, &mut bridge, selected, slot, key, SubScope::Global);
    let ScopedSubReadiness::Live(live) = status else {
        panic!("expected live status, got {status:?}");
    };
    assert_eq!(
        live.relay_eose.tracked_relays, 2,
        "routed leg without committed EOSE must keep readiness incomplete"
    );
    assert!(!live.relay_eose.all_eosed);
}

#[test]
fn accounts_read_with_author_outbox_omits_blocked_remote_advertised_relay() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-composite-blocked-remote", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let author = enostr::FullKeypair::generate();
    let directory =
        planner_fixture_with_local_relay_lists(vec![(author.clone(), vec!["ws://127.0.0.1:7777"])]);
    let config = contact_outbox_config(author_filter(author.pubkey, 1));

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 1);
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
}

#[test]
fn accounts_read_plus_explicit_retargets_observed_relay_after_account_read_removes_it() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("thread-observed-retarget", 1u8));
    let account_relay = NormRelayUrl::new("wss://account.example.com").expect("account relay");
    let observed_relay_a =
        NormRelayUrl::new("wss://observed-a.example.com").expect("observed relay a");
    let observed_relay_b =
        NormRelayUrl::new("wss://observed-b.example.com").expect("observed relay b");
    let initial_account_relays = HashSet::from_iter([
        account_relay.clone(),
        observed_relay_a.clone(),
        observed_relay_b.clone(),
    ]);
    let retargeted_account_relays = HashSet::from_iter([account_relay]);
    let config = accounts_read_plus_explicit_config(
        vec![Filter::new().kinds([1]).limit(10).build()],
        HashSet::from_iter([observed_relay_a.clone(), observed_relay_b.clone()]),
    );

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &initial_account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    assert_eq!(
        runtime
            .live_sub_ids_for_test(selected, key, SubScope::Global)
            .len(),
        1,
        "observed relay is initially covered by the account-read baseline"
    );
    assert!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .is_empty(),
        "no explicit augmentation is needed while account-read already covers the observed relay"
    );

    bridge.with_returned_outbox(|ids| {
        retarget_selected_account_read_relays_with_relays_for_test(
            &mut runtime,
            ids,
            selected,
            &retargeted_account_relays,
        )
    });

    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 2);
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
    assert_ne!(live_ids[0], live_ids[1]);
}

#[test]
fn accounts_read_plus_explicit_omits_blocked_observed_relay() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("thread-observed-blocked", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let blocked_observed = NormRelayUrl::new("wss://127.0.0.1").expect("blocked relay");
    let config = accounts_read_plus_observed_config(
        vec![Filter::new().kinds([1]).limit(10).build()],
        HashSet::from_iter([blocked_observed]),
    );
    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    assert_eq!(
        runtime
            .live_sub_ids_for_test(selected, key, SubScope::Global)
            .len(),
        1
    );
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
}

#[test]
fn accounts_read_plus_explicit_adopts_existing_accountsread_baseline() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("explicit-adopts-baseline", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let explicit_relays = relay_set("wss://observed.example.com");
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let baseline_policy = relay_policy(
        RelayDemandPriority::Important,
        RelayRoutingPreference::PreferDedicated,
    );

    let single = SubConfig::builder(vec![filter.clone()])
        .accounts_read(baseline_policy)
        .build();
    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            single,
        )
    });
    assert_eq!(created, SetSubResult::Created);
    let baseline_before = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("single accountsread live id");

    let with_explicit = accounts_read_plus_explicit_config(vec![filter], explicit_relays);
    let updated = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            with_explicit,
        )
    });
    assert_eq!(updated, SetSubResult::Updated);

    let baseline_after = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("accountsread baseline live id");
    assert_eq!(baseline_after, baseline_before);
    let live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(live_ids.len(), 2);
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
    assert_ne!(live_ids[0], live_ids[1]);
}

#[test]
fn accounts_read_with_author_outbox_adopts_existing_accountsread_baseline() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("author-outbox-adopts-baseline", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let author = account_pk(0xA7);
    let filter = author_filter(author, 1);
    let baseline_policy = relay_policy(
        RelayDemandPriority::Important,
        RelayRoutingPreference::PreferDedicated,
    );
    let single = SubConfig::builder(vec![filter.clone()])
        .accounts_read(baseline_policy)
        .build();
    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            single,
        )
    });
    assert_eq!(created, SetSubResult::Created);
    let baseline_before = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("single accountsread live id");

    let with_author_outbox = author_outbox_config(vec![filter]);
    let updated = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            None,
            slot,
            SubScope::Global,
            key,
            with_author_outbox,
        )
    });
    assert_eq!(updated, SetSubResult::Updated);

    let baseline_after = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("accountsread baseline live id");
    assert_eq!(baseline_after, baseline_before);
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
}

#[test]
fn accounts_read_plus_explicit_to_single_accountsread_keeps_baseline_and_drops_explicit() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("explicit-to-single-keeps-baseline", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let explicit_relays = relay_set("wss://observed.example.com");
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let initial = accounts_read_plus_explicit_config(vec![filter.clone()], explicit_relays);

    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            initial,
        )
    });
    assert_eq!(created, SetSubResult::Created);
    let baseline_before = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("accountsread baseline live id");
    let initial_live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(initial_live_ids.len(), 2);
    let explicit_id = initial_live_ids[1];

    let single = SubConfig::builder(vec![filter])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .build();
    let updated = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            single,
        )
    });
    assert_eq!(updated, SetSubResult::Updated);

    let baseline_after = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("single accountsread live id");
    assert_eq!(baseline_after, baseline_before);
    let current_live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(current_live_ids.len(), 1);
    assert!(!current_live_ids.contains(&explicit_id));
}

#[test]
fn accounts_read_with_author_outbox_clears_routed_leg_when_author_demand_is_removed() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-composite-clear-author", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let author = enostr::FullKeypair::generate();
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let initial = contact_outbox_config(author_filter(author.pubkey, 1));

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            initial,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let routed_id = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .into_iter()
        .next()
        .expect("initial routed leg")
        .1;

    let updated = contact_outbox_config(Filter::new().kinds([1]).limit(10).build());
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            updated,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );

    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
    let current_live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(current_live_ids.len(), 1);
    assert!(!current_live_ids.contains(&routed_id));
}

#[test]
fn accounts_read_plus_explicit_source_change_replaces_additive_sub() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("routed-source-policy", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let routed_relay = NormRelayUrl::new("wss://routed.example.com").expect("routed relay");
    let routed_relays = HashSet::from([routed_relay]);
    let filter = Filter::new().kinds([1]).limit(10).build();

    let initial = accounts_read_plus_explicit_config(vec![filter.clone()], routed_relays.clone());
    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            initial,
        )
    });
    assert_eq!(created, SetSubResult::Created);
    let initial_live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(initial_live_ids.len(), 2);
    let initial_leg_id = initial_live_ids[1];
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert_eq!(
        runtime.live_id_relay_url_source_for_test(&scoped, initial_leg_id),
        Some(RelayUrlSource::Explicit)
    );
    let updated = accounts_read_plus_observed_config(vec![filter], routed_relays);
    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            updated,
        )
    });

    assert_eq!(result, SetSubResult::Updated);
    let refreshed_live_ids = runtime.live_sub_ids_for_test(selected, key, SubScope::Global);
    assert_eq!(refreshed_live_ids.len(), 2);
    let refreshed_leg_id = refreshed_live_ids[1];
    assert_ne!(initial_leg_id, refreshed_leg_id);
    assert!(!refreshed_live_ids.contains(&initial_leg_id));
    assert_eq!(
        runtime.live_id_relay_url_source_for_test(&scoped, refreshed_leg_id),
        Some(RelayUrlSource::RemoteAdvertised)
    )
}

#[tokio::test]
async fn author_outbox_full_history_uses_resolved_author_relay_packages() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-full-history-resolved", 1u8));
    let author = enostr::FullKeypair::generate();
    let author_relay = NormRelayUrl::new("wss://relay-a.example.com").expect("author relay");
    let account_relays = relay_set("wss://account.example.com");
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let filter = author_filter(author.pubkey, 1);
    let config = author_outbox_full_history_config(filter);

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config.clone(),
        )
    });

    assert_eq!(result, SetSubResult::Created);
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let routed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(routed_legs.len(), 1);
    assert_eq!(routed_legs[0].0, author_relay);
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert!(runtime.full_history_id_for_test(&scoped).is_some());
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::Explicit,
        &account_relays
    ));
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::RemoteAdvertised,
        &HashSet::from([author_relay.clone()])
    ));

    let repeat_result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });
    assert_eq!(repeat_result, SetSubResult::Unchanged);
    let repeated_routed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(repeated_routed_legs, routed_legs);
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::Explicit,
        &account_relays
    ));
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::RemoteAdvertised,
        &HashSet::from([author_relay.clone()])
    ));

    bridge.with_returned_outbox(|ids| {
        apply_author_outbox_plans_for_active_scoped_keys_for_test(
            &mut runtime,
            ids,
            selected,
            &account_relays,
        )
    });

    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .into_iter()
            .map(|(relay, _)| relay)
            .collect::<Vec<_>>(),
        vec![author_relay.clone()]
    );
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::Explicit,
        &account_relays
    ));
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::RemoteAdvertised,
        &HashSet::from([author_relay.clone()])
    ));
}

#[tokio::test]
async fn author_outbox_full_history_is_concurrent_by_default() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-full-history-concurrent-live", 1u8));
    let account_relay = NormRelayUrl::new("wss://account.example.com").expect("account relay");
    let author = enostr::FullKeypair::generate();
    let author_relay = NormRelayUrl::new("wss://relay-a.example.com").expect("author relay");
    let account_relays = HashSet::from([account_relay]);
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let filter = author_filter(author.pubkey, 1);
    let config = SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ))
        .build();

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let routed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(routed_legs.len(), 1);
    assert_eq!(routed_legs[0].0, author_relay);
}

#[tokio::test]
async fn author_outbox_full_history_routes_history_filters() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-full-history-history-filter", 1u8));
    let account_relays = relay_set("wss://account.example.com");
    let live_author = enostr::FullKeypair::generate();
    let history_author = enostr::FullKeypair::generate();
    let history_relay =
        NormRelayUrl::new("wss://history-author.example.com").expect("history relay");
    let directory = planner_fixture_with_local_relay_lists(vec![
        (live_author.clone(), vec!["wss://live-author.example.com"]),
        (
            history_author.clone(),
            vec!["wss://history-author.example.com"],
        ),
    ]);
    let live_filter = author_filter(live_author.pubkey, 1);
    let history_filter = author_filter(history_author.pubkey, 1);
    let config = SubConfig::builder(vec![live_filter])
        .full_history(enostr::FullHistoryConfig::new(vec![history_filter]))
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ))
        .build();

    let result = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    assert_eq!(result, SetSubResult::Created);
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &directory,
    );
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert!(runtime.full_history_id_for_test(&scoped).is_some());
    assert!(full_history_has_relay_pkg(
        &runtime,
        &scoped,
        RelayUrlSource::RemoteAdvertised,
        &HashSet::from([history_relay])
    ));
}

#[test]
fn author_outbox_plan_application_reuses_unchanged_routed_live_legs() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-diff", 1u8));
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();
    let author_c = enostr::FullKeypair::generate();
    let relay_a = NormRelayUrl::new("wss://relay-a.example.com").expect("relay a");
    let relay_b = NormRelayUrl::new("wss://relay-b.example.com").expect("relay b");
    let relay_c = NormRelayUrl::new("wss://relay-c.example.com").expect("relay c");

    let filter = Filter::new()
        .authors([
            author_a.pubkey.bytes(),
            author_b.pubkey.bytes(),
            author_c.pubkey.bytes(),
        ])
        .kinds([1])
        .limit(20)
        .build();

    let directory = planner_fixture_with_local_relay_lists(vec![
        (author_a.clone(), vec!["wss://relay-a.example.com"]),
        (author_b.clone(), vec!["wss://relay-a.example.com"]),
        (author_c.clone(), vec!["wss://relay-b.example.com"]),
    ]);

    let config = author_outbox_config(vec![filter]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );

    let initial_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(initial_legs.len(), 2);
    let relay_a_id = initial_legs
        .iter()
        .find(|(relay, _)| *relay == relay_a)
        .expect("relay-a leg")
        .1;
    let relay_b_id = initial_legs
        .iter()
        .find(|(relay, _)| *relay == relay_b)
        .expect("relay-b leg")
        .1;

    bridge.with_returned_outbox(|ids| {
        apply_author_outbox_plans_for_active_scoped_keys_for_test(
            &mut runtime,
            ids,
            selected,
            &HashSet::new(),
        )
    });

    let refreshed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(refreshed_legs.len(), 2);
    let refreshed_relay_a_id = refreshed_legs
        .iter()
        .find(|(relay, _)| *relay == relay_a)
        .expect("relay-a leg after refresh")
        .1;
    let refreshed_relay_b_id = refreshed_legs
        .iter()
        .find(|(relay, _)| *relay == relay_b)
        .expect("relay-b leg after refresh")
        .1;

    assert_eq!(refreshed_relay_a_id, relay_a_id);
    assert_eq!(refreshed_relay_b_id, relay_b_id);
    assert!(!refreshed_legs.iter().any(|(relay, _)| *relay == relay_c));
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key,
        SubScope::Global,
        relay_a_id,
        vec![vec![author_a.pubkey.hex(), author_b.pubkey.hex()]],
    );
}

#[test]
fn selected_account_read_retarget_applies_author_outbox_without_churning_baseline() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-retarget-stable-baseline", 1u8));
    let account_relays_a = relay_set("wss://account-a.example.com");
    let account_relays_b = relay_set("wss://account-b.example.com");
    let author = enostr::FullKeypair::generate();
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://author-relay.example.com"],
    )]);
    let config = author_outbox_config(vec![author_filter(author.pubkey, 1)]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays_a,
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });
    let baseline_before = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("baseline live id before retarget");

    bridge.with_returned_outbox(|ids| {
        retarget_selected_account_read_relays_with_relays_for_test(
            &mut runtime,
            ids,
            selected,
            &account_relays_b,
        )
    });

    let baseline_after = runtime
        .live_id_with_selected(selected, key, SubScope::Global)
        .expect("baseline live id after retarget");
    assert_eq!(baseline_before, baseline_after);
}

#[test]
fn author_outbox_plan_application_preserves_multiple_filters_on_same_relay() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-same-relay-filters", 1u8));
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();

    let filter_a = Filter::new()
        .authors([author_a.pubkey.bytes()])
        .kinds([1])
        .limit(10)
        .build();
    let filter_b = Filter::new()
        .authors([author_b.pubkey.bytes()])
        .kinds([6])
        .limit(10)
        .build();

    let directory = planner_fixture_with_local_relay_lists(vec![
        (author_a.clone(), vec!["wss://relay-a.example.com"]),
        (author_b.clone(), vec!["wss://relay-a.example.com"]),
    ]);
    let config = author_outbox_config(vec![filter_a, filter_b]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );
    let relay = NormRelayUrl::new("wss://relay-a.example.com").expect("relay url");
    let initial_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(initial_legs.len(), 1);
    let initial_id = routed_leg_id_for_relay(&initial_legs, &relay);
    let mut expected_authors = vec![vec![author_a.pubkey.hex()], vec![author_b.pubkey.hex()]];
    expected_authors.sort();
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key,
        SubScope::Global,
        initial_id,
        expected_authors.clone(),
    );

    bridge.with_returned_outbox(|ids| {
        apply_author_outbox_plans_for_active_scoped_keys_for_test(
            &mut runtime,
            ids,
            selected,
            &HashSet::new(),
        )
    });

    let refreshed_legs = runtime.routed_live_legs_for_test(selected, key, SubScope::Global);
    assert_eq!(refreshed_legs.len(), 1);
    let refreshed_id = routed_leg_id_for_relay(&refreshed_legs, &relay);
    assert_eq!(refreshed_id, initial_id);
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key,
        SubScope::Global,
        refreshed_id,
        expected_authors,
    );
}

#[test]
fn author_outbox_replaces_routed_live_legs_when_demand_priority_changes() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let slot = test_owner!();
    let selected = account_pk(0x01);
    let key = make_key(("outbox-priority", 1u8));
    let author = enostr::FullKeypair::generate();

    let filter = Filter::new()
        .authors([author.pubkey.bytes()])
        .kinds([1])
        .limit(20)
        .build();

    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);

    let config = author_outbox_config(vec![filter.clone()]);
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );

    let initial_leg_id = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .into_iter()
        .next()
        .expect("initial leg")
        .1;

    let updated = SubConfig::builder(vec![filter])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Critical,
            RelayRoutingPreference::NoPreference,
        ))
        .build();
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot,
            SubScope::Global,
            key,
            updated,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );

    let refreshed_leg_id = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .into_iter()
        .next()
        .expect("refreshed leg")
        .1;

    assert_ne!(initial_leg_id, refreshed_leg_id);
    assert!(!runtime
        .live_sub_ids_for_test(selected, key, SubScope::Global)
        .contains(&initial_leg_id));
}

#[test]
fn author_outbox_index_deduplicates_authors_within_one_scoped_key() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let author = account_pk(0xA1);
    let slot = test_owner!();
    let key = make_key(("outbox-index-dedup", 1u8));
    let initial = author_outbox_config(vec![author_filter(author, 1), author_filter(author, 6)]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            slot,
            SubScope::Global,
            key,
            initial,
        )
    });
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author])
    );
    let updated = author_outbox_config(vec![author_filter(author, 1)]);
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            slot,
            SubScope::Global,
            key,
            updated,
        )
    });

    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author])
    )
}

#[test]
fn author_outbox_index_keeps_shared_author_until_last_scoped_key_drops() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let author = account_pk(0xA2);
    let slot_a = test_owner!();
    let slot_b = test_owner!();
    let key_a = make_key(("outbox-index-shared", "a"));
    let key_b = make_key(("outbox-index-shared", "b"));
    let config = author_outbox_config(vec![author_filter(author, 1)]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            slot_a,
            SubScope::Global,
            key_a,
            config.clone(),
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            slot_b,
            SubScope::Global,
            key_b,
            config,
        )
    });
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author])
    );
    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                selected,
                slot_a,
                key_a,
                SubScope::Global,
            )
        }),
        ClearSubResult::Cleared
    );
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author])
    );

    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                selected,
                slot_b,
                key_b,
                SubScope::Global,
            )
        }),
        ClearSubResult::Cleared
    );
    assert!(runtime.author_outbox_active_authors_for_test().is_empty());
}

#[test]
fn author_outbox_index_switches_active_account_scope() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xA3);
    let account_b = account_pk(0xB3);
    let author_a = account_pk(0xA4);
    let author_b = account_pk(0xB4);
    let relays = HashSet::new();
    let slot_a = test_owner!();
    let slot_b = test_owner!();
    let key_a = make_key(("outbox-index-account", "a"));
    let key_b = make_key(("outbox-index-account", "b"));

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_a,
            slot_a,
            SubScope::Account,
            key_a,
            author_outbox_config(vec![author_filter(author_a, 1)]),
        )
    });
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author_a])
    );
    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays)
    });
    assert!(runtime.author_outbox_active_authors_for_test().is_empty());

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_b,
            slot_b,
            SubScope::Account,
            key_b,
            author_outbox_config(vec![author_filter(author_b, 1)]),
        )
    });
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author_b])
    );
    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays)
    });
    assert_eq!(
        runtime.author_outbox_active_authors_for_test(),
        HashSet::from_iter([author_a])
    );
}

#[test]
fn accounts_read_with_author_outbox_account_switch_restores_whole_live_state() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xA8);
    let account_b = account_pk(0xB8);
    let relays_a = relay_set("wss://account-a.example.com");
    let relays_b = relay_set("wss://account-b.example.com");
    let author = enostr::FullKeypair::generate();
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let slot = test_owner!();
    let key = make_key(("outbox-account-switch-live", 1u8));

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            Some(&directory),
            slot,
            SubScope::Account,
            key,
            contact_outbox_config(author_filter(author.pubkey, 1)),
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        account_a,
        &relays_a,
        &directory,
    );
    assert_eq!(
        runtime
            .live_sub_ids_for_test(account_a, key, SubScope::Account)
            .len(),
        2
    );

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays_b)
    });
    assert!(runtime
        .live_sub_ids_for_test(account_a, key, SubScope::Account)
        .is_empty());

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays_a)
    });

    assert_eq!(
        runtime
            .live_sub_ids_for_test(account_a, key, SubScope::Account)
            .len(),
        2
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(account_a, key, SubScope::Account)
            .len(),
        1
    );
}

#[test]
fn inactive_account_set_sub_rebuilds_retained_config_without_live_state() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xA9);
    let account_b = account_pk(0xB9);
    let relays_a = relay_set("wss://account-a.example.com");
    let relays_b = relay_set("wss://account-b.example.com");
    let author = enostr::FullKeypair::generate();
    let directory = planner_fixture_with_local_relay_lists(vec![(
        author.clone(),
        vec!["wss://relay-a.example.com"],
    )]);
    let slot = test_owner!();
    let key = make_key(("inactive-account-retained-config", 1u8));
    let filter = author_filter(author.pubkey, 1);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            Some(&directory),
            slot,
            SubScope::Account,
            key,
            contact_outbox_config(filter.clone()),
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        account_a,
        &relays_a,
        &directory,
    );
    assert_eq!(
        runtime
            .live_sub_ids_for_test(account_a, key, SubScope::Account)
            .len(),
        2
    );

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays_b)
    });
    assert!(runtime
        .live_sub_ids_for_test(account_a, key, SubScope::Account)
        .is_empty());

    let _ = bridge.with_returned_outbox(|ids| {
        set_inactive_account_sub_for_test(
            &mut runtime,
            ids,
            account_b,
            account_a,
            slot,
            key,
            SubConfig::builder(vec![filter])
                .accounts_read(relay_policy(
                    RelayDemandPriority::Important,
                    RelayRoutingPreference::default(),
                ))
                .build(),
        )
    });
    assert!(runtime
        .live_sub_ids_for_test(account_a, key, SubScope::Account)
        .is_empty());

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays_a)
    });

    assert_eq!(
        runtime
            .live_sub_ids_for_test(account_a, key, SubScope::Account)
            .len(),
        1
    );
    assert!(runtime
        .routed_live_legs_for_test(account_a, key, SubScope::Account)
        .is_empty());
}

#[test]
fn inactive_account_set_sub_same_config_is_unchanged() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0xB1);
    let inactive = account_pk(0xA1);
    let slot = test_owner!();
    let key = make_key(("inactive-account-same-config", 1u8));
    let config = SubConfig::builder(vec![Filter::new().kinds([1]).limit(10).build()])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::default(),
        ))
        .build();

    let created = bridge.with_returned_outbox(|ids| {
        set_inactive_account_sub_for_test(
            &mut runtime,
            ids,
            selected,
            inactive,
            slot,
            key,
            config.clone(),
        )
    });
    let repeated = bridge.with_returned_outbox(|ids| {
        set_inactive_account_sub_for_test(&mut runtime, ids, selected, inactive, slot, key, config)
    });

    assert_eq!(created, SetSubResult::Created);
    assert_eq!(repeated, SetSubResult::Unchanged);
    assert!(runtime
        .live_sub_ids_for_test(inactive, key, SubScope::Account)
        .is_empty());
}

#[test]
fn inactive_account_drop_owner_rebuilds_retained_config_before_switch_back() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0xB2);
    let inactive = account_pk(0xA2);
    let account_relays = relay_set("wss://account-a.example.com");
    let owner_a = test_owner!();
    let owner_b = test_owner!();
    let key = make_key(("inactive-account-owner-release", 1u8));
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();
    let relay_a = NormRelayUrl::new("wss://author-a.example.com").expect("relay a");
    let relay_b = NormRelayUrl::new("wss://author-b.example.com").expect("relay b");
    let directory = planner_fixture_with_local_relay_lists(vec![
        (author_a.clone(), vec!["wss://author-a.example.com"]),
        (author_b.clone(), vec!["wss://author-b.example.com"]),
    ]);

    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_inactive_account_sub_for_test(
                &mut runtime,
                ids,
                selected,
                inactive,
                owner_a,
                key,
                author_outbox_config(vec![author_filter(author_a.pubkey, 1)]),
            )
        }),
        SetSubResult::Created
    );
    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_inactive_account_sub_for_test(
                &mut runtime,
                ids,
                selected,
                inactive,
                owner_b,
                key,
                author_outbox_config(vec![author_filter(author_b.pubkey, 1)]),
            )
        }),
        SetSubResult::Updated
    );
    assert!(runtime
        .live_sub_ids_for_test(inactive, key, SubScope::Account)
        .is_empty());

    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, selected, &HashSet::new(), owner_b)
    }));
    assert!(runtime
        .live_sub_ids_for_test(inactive, key, SubScope::Account)
        .is_empty());

    let (_, effects) = bridge.with_returned_outbox(|ids| {
        runtime.on_account_switched_with_relays_with_effects(
            ids,
            selected,
            inactive,
            &account_relays,
        )
    });
    apply_scoped_effects_for_test(
        &mut runtime,
        &mut bridge,
        inactive,
        &account_relays,
        &directory.ndb(),
        effects,
    );
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        inactive,
        &account_relays,
        &directory,
    );

    let routed_relays = runtime
        .routed_live_legs_for_test(inactive, key, SubScope::Account)
        .into_iter()
        .map(|(relay, _)| relay)
        .collect::<Vec<_>>();
    assert_eq!(routed_relays, vec![relay_a]);
    assert!(!routed_relays.contains(&relay_b));
}

#[test]
fn author_outbox_plan_application_keeps_frozen_plan_after_relay_list_update() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (_tmp, ndb) = new_ndb();
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();
    let key_a = make_key(("outbox-index-plan-refresh", "a"));
    let key_b = make_key(("outbox-index-plan-refresh", "b"));
    let slot_a = test_owner!();
    let slot_b = test_owner!();

    for note in [
        nip65_write_relay_note_for_test(&author_a, &["wss://relay-a-old.example.com"]),
        nip65_write_relay_note_for_test(&author_b, &["wss://relay-b.example.com"]),
    ] {
        ndb.process_client_event(&note.json().expect("json"))
            .expect("ingest initial nip65");
    }
    wait_for_nip65_for_test(&ndb, &author_a.pubkey);
    wait_for_nip65_for_test(&ndb, &author_b.pubkey);

    let empty_relays = HashSet::new();
    let _ = set_sub_and_apply_effects_for_test(
        &mut runtime,
        &mut bridge,
        &empty_relays,
        selected,
        slot_a,
        SubScope::Global,
        key_a,
        author_outbox_config(vec![author_filter(author_a.pubkey, 1)]),
        &ndb,
    );
    let _ = set_sub_and_apply_effects_for_test(
        &mut runtime,
        &mut bridge,
        &empty_relays,
        selected,
        slot_b,
        SubScope::Global,
        key_b,
        author_outbox_config(vec![author_filter(author_b.pubkey, 1)]),
        &ndb,
    );

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    for _ in 0..20 {
        if !runtime
            .routed_live_legs_for_test(selected, key_a, SubScope::Global)
            .is_empty()
            && !runtime
                .routed_live_legs_for_test(selected, key_b, SubScope::Global)
                .is_empty()
        {
            break;
        }
        advance_author_outbox_plans_once_for_test(
            &mut runtime,
            &mut bridge,
            selected,
            &HashSet::new(),
            &ndb,
        );
    }
    let initial_a = runtime
        .routed_live_legs_for_test(selected, key_a, SubScope::Global)
        .into_iter()
        .next()
        .expect("initial a leg")
        .1;
    let initial_b = runtime
        .routed_live_legs_for_test(selected, key_b, SubScope::Global)
        .into_iter()
        .next()
        .expect("initial b leg")
        .1;

    ndb.process_client_event(
        &nip65_write_relay_note_at_for_test(&author_a, &["wss://relay-a-new.example.com"], 2)
            .json()
            .expect("json"),
    )
    .expect("ingest updated nip65");
    wait_for_nip65_at_for_test(&ndb, &author_a.pubkey, 2);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    let current_a = runtime
        .routed_live_legs_for_test(selected, key_a, SubScope::Global)
        .into_iter()
        .next()
        .expect("current a leg")
        .1;
    let current_b = runtime
        .routed_live_legs_for_test(selected, key_b, SubScope::Global)
        .into_iter()
        .next()
        .expect("current b leg")
        .1;

    assert_eq!(initial_a, current_a);
    assert_eq!(initial_b, current_b);
}

#[tokio::test]
async fn author_outbox_plan_application_keeps_frozen_full_history_plan_after_relay_list_update() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (_tmp, ndb) = new_ndb();
    let author = enostr::FullKeypair::generate();
    let key = make_key(("outbox-full-history-plan-refresh", "same-frame"));
    let slot = test_owner!();
    let account_relays = relay_set("wss://account.example.com");
    let old_relay = NormRelayUrl::new("wss://relay-old.example.com").expect("old relay");
    let filter = author_filter(author.pubkey, 1);

    ndb.process_client_event(
        &nip65_write_relay_note_for_test(&author, &["wss://relay-old.example.com"])
            .json()
            .expect("json"),
    )
    .expect("ingest initial nip65");
    wait_for_nip65_for_test(&ndb, &author.pubkey);

    let _ = set_sub_and_apply_effects_for_test(
        &mut runtime,
        &mut bridge,
        &account_relays,
        selected,
        slot,
        SubScope::Global,
        key,
        author_outbox_full_history_config(filter),
        &ndb,
    );

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert!(runtime.full_history_id_for_test(&scoped).is_some());
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .into_iter()
            .map(|(relay, _)| relay)
            .collect::<Vec<_>>(),
        vec![old_relay.clone()]
    );
    let initial_history_relays = remote_history_remote_relays(&runtime, &scoped);
    assert_eq!(initial_history_relays, HashSet::from([old_relay.clone()]));

    ndb.process_client_event(
        &nip65_write_relay_note_at_for_test(&author, &["wss://relay-new.example.com"], 2)
            .json()
            .expect("json"),
    )
    .expect("ingest updated nip65");
    wait_for_nip65_at_for_test(&ndb, &author.pubkey, 2);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .into_iter()
            .map(|(relay, _)| relay)
            .collect::<Vec<_>>(),
        vec![old_relay.clone()]
    );
    let refreshed_history_relays = remote_history_remote_relays(&runtime, &scoped);
    assert_eq!(refreshed_history_relays, HashSet::from([old_relay]));
}

#[test]
fn author_outbox_plan_application_processes_pending_work_to_completion() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (_tmp, ndb) = new_ndb();
    let count = 20;
    let mut keys = Vec::new();

    for index in 0..count {
        let author = enostr::FullKeypair::generate();
        let relay = format!("wss://relay-{index}.example.com");
        ndb.process_client_event(
            &nip65_write_relay_note_for_test(&author, &[&relay])
                .json()
                .expect("json"),
        )
        .expect("ingest nip65");
        wait_for_nip65_for_test(&ndb, &author.pubkey);

        let key = make_key(("outbox-refresh-backlog", index));
        let owner = test_owner!();
        let empty_relays = HashSet::new();
        let _ = set_sub_and_apply_effects_for_test(
            &mut runtime,
            &mut bridge,
            &empty_relays,
            selected,
            owner,
            SubScope::Global,
            key,
            author_outbox_config(vec![author_filter(author.pubkey, 1)]),
            &ndb,
        );
        keys.push(key);
    }

    advance_author_outbox_plans_once_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    let realized_after_first_advance = keys
        .iter()
        .filter(|key| {
            !runtime
                .routed_live_legs_for_test(selected, **key, SubScope::Global)
                .is_empty()
        })
        .count();
    assert!(realized_after_first_advance <= count);

    for _ in 0..10 {
        if keys.iter().all(|key| {
            !runtime
                .routed_live_legs_for_test(selected, *key, SubScope::Global)
                .is_empty()
        }) {
            break;
        }
        advance_author_outbox_plans_once_for_test(
            &mut runtime,
            &mut bridge,
            selected,
            &HashSet::new(),
            &ndb,
        );
    }
    let realized_after_second_advance = keys
        .iter()
        .filter(|key| {
            !runtime
                .routed_live_legs_for_test(selected, **key, SubScope::Global)
                .is_empty()
        })
        .count();
    assert_eq!(realized_after_second_advance, count);
}

#[test]
fn author_outbox_initial_known_snapshot_limits_single_author_many_relay_live_realization() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-initial-known-single-author-many-relays", "live"));
    let author = enostr::FullKeypair::generate();
    let relay_count = 20usize;
    let relays = (0..relay_count)
        .map(|index| format!("wss://known-initial-{index}.example.com"))
        .collect::<Vec<_>>();
    let relay_refs = relays.iter().map(String::as_str).collect::<Vec<_>>();

    ndb.process_client_event(
        &nip65_write_relay_note_for_test(&author, &relay_refs)
            .json()
            .expect("json"),
    )
    .expect("ingest many-relay nip65");
    wait_for_nip65_for_test(&ndb, &author.pubkey);
    let planner_fixture = PlannerFixture::new(tmp, ndb.clone());

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&planner_fixture),
            slot,
            SubScope::Global,
            key,
            author_outbox_config(vec![author_filter(author.pubkey, 1)]),
        )
    });

    let first_live_count = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    assert!(first_live_count <= relay_count);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        relay_count
    );
}

#[test]
fn author_outbox_full_plan_application_limits_single_author_many_relay_live_realization() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-full-refresh-single-author-many-relays", "live"));
    let author = enostr::FullKeypair::generate();
    let relay_count = 20usize;
    let relays = (0..relay_count)
        .map(|index| format!("wss://known-refresh-{index}.example.com"))
        .collect::<Vec<_>>();
    let relay_refs = relays.iter().map(String::as_str).collect::<Vec<_>>();

    ndb.process_client_event(
        &nip65_write_relay_note_for_test(&author, &relay_refs)
            .json()
            .expect("json"),
    )
    .expect("ingest many-relay nip65");
    wait_for_nip65_for_test(&ndb, &author.pubkey);
    let planner_fixture = PlannerFixture::new(tmp, ndb.clone());

    let empty_relays = HashSet::new();
    let (_, pending_effects) = bridge.with_returned_outbox(|ids| {
        runtime.set_sub_with_relays_with_effects(
            ids,
            &empty_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            author_outbox_config(vec![author_filter(author.pubkey, 1)]),
        )
    });
    assert!(runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .is_empty());
    apply_scoped_effects_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &empty_relays,
        &planner_fixture.ndb(),
        pending_effects,
    );

    let first_live_count = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    assert!(first_live_count <= relay_count);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        relay_count
    );
}

#[test]
fn author_outbox_plan_application_keeps_frozen_many_relay_live_plan_after_empty_relay_list() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let (_tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-single-author-many-relays", "live-removal"));
    let author = enostr::FullKeypair::generate();
    let relay_count = 20usize;
    let relays = (0..relay_count)
        .map(|index| format!("wss://many-live-removal-{index}.example.com"))
        .collect::<Vec<_>>();
    let relay_refs = relays.iter().map(String::as_str).collect::<Vec<_>>();

    let empty_relays = HashSet::new();
    let (_, pending_effects) = bridge.with_returned_outbox(|ids| {
        runtime.set_sub_with_relays_with_effects(
            ids,
            &empty_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            author_outbox_config(vec![author_filter(author.pubkey, 1)]),
        )
    });
    ndb.process_client_event(
        &nip65_write_relay_note_for_test(&author, &relay_refs)
            .json()
            .expect("json"),
    )
    .expect("ingest many-relay nip65");
    wait_for_nip65_for_test(&ndb, &author.pubkey);

    apply_scoped_effects_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &empty_relays,
        &ndb,
        pending_effects,
    );
    advance_author_outbox_plans_for_test(&mut runtime, &mut bridge, selected, &empty_relays, &ndb);
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        relay_count
    );

    ndb.process_client_event(
        &nip65_write_relay_note_at_for_test(&author, &[], 2)
            .json()
            .expect("json"),
    )
    .expect("ingest empty nip65");
    wait_for_nip65_at_for_test(&ndb, &author.pubkey, 2);

    advance_author_outbox_plans_once_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    let first_live_count = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    assert!(first_live_count <= relay_count);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        relay_count
    );
}

#[tokio::test]
async fn author_outbox_plan_application_keeps_frozen_many_relay_full_history_plan_after_empty_relay_list(
) {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let account_relays = relay_set("wss://account-read-many-history-removal.example.com");
    let (_tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-single-author-many-relays", "history-removal"));
    let author = enostr::FullKeypair::generate();
    let filter = author_filter(author.pubkey, 1);
    let relay_count = 20usize;
    let relays = (0..relay_count)
        .map(|index| format!("wss://many-history-removal-{index}.example.com"))
        .collect::<Vec<_>>();
    let relay_refs = relays.iter().map(String::as_str).collect::<Vec<_>>();

    let (_, pending_effects) = bridge.with_returned_outbox(|ids| {
        runtime.set_sub_with_relays_with_effects(
            ids,
            &account_relays,
            selected,
            slot,
            SubScope::Global,
            key,
            author_outbox_full_history_config(filter),
        )
    });
    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert!(runtime.full_history_id_for_test(&scoped).is_some());

    ndb.process_client_event(
        &nip65_write_relay_note_for_test(&author, &relay_refs)
            .json()
            .expect("json"),
    )
    .expect("ingest many-relay nip65");
    wait_for_nip65_for_test(&ndb, &author.pubkey);

    apply_scoped_effects_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
        pending_effects,
    );
    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    assert_eq!(remote_history_relay_count(&runtime, &scoped), relay_count);

    ndb.process_client_event(
        &nip65_write_relay_note_at_for_test(&author, &[], 2)
            .json()
            .expect("json"),
    )
    .expect("ingest empty nip65");
    wait_for_nip65_at_for_test(&ndb, &author.pubkey, 2);

    advance_author_outbox_plans_once_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    let first_live_count = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    let first_history_count = remote_history_relay_count(&runtime, &scoped);
    assert!(first_live_count <= relay_count);
    assert!(first_live_count <= first_history_count);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        relay_count
    );
    assert_eq!(remote_history_relay_count(&runtime, &scoped), relay_count);
}

#[test]
fn large_known_author_outbox_set_sub_backfills_routed_legs_in_batches() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let account_relays = relay_set("wss://account-read.example.com");
    let (tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-known-author-backlog", "same-key"));
    let authors = (0..20)
        .map(|_| enostr::FullKeypair::generate())
        .collect::<Vec<_>>();
    let filter = Filter::new()
        .authors(authors.iter().map(|author| author.pubkey.bytes()))
        .kinds([1])
        .limit(20)
        .build();

    for (index, author) in authors.iter().enumerate() {
        let relay = format!("wss://relay-known-{index}.example.com");
        ndb.process_client_event(
            &nip65_write_relay_note_for_test(author, &[&relay])
                .json()
                .expect("json"),
        )
        .expect("ingest known author nip65");
        wait_for_nip65_for_test(&ndb, &author.pubkey);
    }

    let planner_fixture = PlannerFixture::new(tmp, ndb.clone());

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&planner_fixture),
            slot,
            SubScope::Global,
            key,
            author_outbox_config(vec![filter]),
        )
    });

    advance_author_outbox_plans_once_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    let first_batch_len = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    assert!(first_batch_len <= authors.len());

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        authors.len()
    );
}

#[tokio::test]
async fn large_known_author_outbox_full_history_backfills_routed_targets_in_batches() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let account_relays = relay_set("wss://account-read-history.example.com");
    let (tmp, ndb) = new_ndb();
    let slot = test_owner!();
    let key = make_key(("outbox-known-history-backlog", "same-key"));
    let authors = (0..20)
        .map(|_| enostr::FullKeypair::generate())
        .collect::<Vec<_>>();
    let filter = Filter::new()
        .authors(authors.iter().map(|author| author.pubkey.bytes()))
        .kinds([1])
        .limit(20)
        .build();

    for (index, author) in authors.iter().enumerate() {
        let relay = format!("wss://relay-known-history-{index}.example.com");
        ndb.process_client_event(
            &nip65_write_relay_note_for_test(author, &[&relay])
                .json()
                .expect("json"),
        )
        .expect("ingest known author nip65");
        wait_for_nip65_for_test(&ndb, &author.pubkey);
    }

    let planner_fixture = PlannerFixture::new(tmp, ndb.clone());

    let config = SubConfig::builder(vec![filter.clone()])
        .full_history(enostr::FullHistoryConfig::new(vec![filter]))
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .with_author_outbox(relay_policy(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ))
        .build();
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &account_relays,
            selected,
            Some(&planner_fixture),
            slot,
            SubScope::Global,
            key,
            config,
        )
    });

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    assert!(runtime.full_history_id_for_test(&scoped).is_some());
    advance_author_outbox_plans_once_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    let first_live_count = runtime
        .routed_live_legs_for_test(selected, key, SubScope::Global)
        .len();
    let first_history_count = remote_history_relay_count(&runtime, &scoped);
    assert!(first_live_count <= authors.len());
    assert!(first_history_count <= authors.len());
    assert!(first_live_count <= first_history_count);

    advance_author_outbox_plans_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &account_relays,
        &ndb,
    );
    assert_eq!(
        runtime
            .routed_live_legs_for_test(selected, key, SubScope::Global)
            .len(),
        authors.len()
    );
    assert_eq!(remote_history_relay_count(&runtime, &scoped), authors.len());
}

#[test]
fn outbox_planning_realizes_the_maximal_routed_set_across_multiple_logical_subs() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let author_a = enostr::FullKeypair::generate();
    let author_b = enostr::FullKeypair::generate();
    let author_c = enostr::FullKeypair::generate();
    let pk_a = author_a.pubkey;
    let pk_b = author_b.pubkey;
    let pk_c = author_c.pubkey;

    let directory = planner_fixture_with_local_relay_lists(vec![
        (author_a, vec!["wss://relay-a.example.com"]),
        (author_b, vec!["wss://relay-a.example.com"]),
        (author_c, vec!["wss://relay-b.example.com"]),
    ]);

    let slot_a = test_owner!();
    let slot_b = test_owner!();
    let key_a = make_key(("outbox-global", "a"));
    let key_b = make_key(("outbox-global", "b"));
    let config_a = author_outbox_config(vec![Filter::new()
        .authors([pk_a.bytes()])
        .kinds([1])
        .limit(10)
        .build()]);
    let config_b = author_outbox_config(vec![Filter::new()
        .authors([pk_b.bytes(), pk_c.bytes()])
        .kinds([1])
        .limit(10)
        .build()]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot_a,
            SubScope::Global,
            key_a,
            config_a,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_planner_fixture(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            Some(&directory),
            slot_b,
            SubScope::Global,
            key_b,
            config_b,
        )
    });
    realize_author_outbox_plan_for_test(
        &mut runtime,
        &mut bridge,
        selected,
        &HashSet::new(),
        &directory,
    );

    let scoped_a = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key_a);
    let scoped_b = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key_b);
    let live_a = runtime
        .routed_live_legs_for_test(selected, key_a, SubScope::Global)
        .into_iter()
        .map(|(_, live_id)| live_id)
        .collect::<Vec<_>>();
    let live_b_legs = runtime.routed_live_legs_for_test(selected, key_b, SubScope::Global);

    assert_eq!(live_a.len(), 1);
    assert_eq!(live_b_legs.len(), 2);
    assert!(runtime.has_live_for_scoped_for_test(&scoped_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_b));

    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key_a,
        SubScope::Global,
        live_a[0],
        vec![vec![pk_a.hex()]],
    );

    let relay_a = NormRelayUrl::new("wss://relay-a.example.com").expect("relay a");
    let relay_b = NormRelayUrl::new("wss://relay-b.example.com").expect("relay b");
    let live_b_relay_a_id = routed_leg_id_for_relay(&live_b_legs, &relay_a);
    let live_b_relay_b_id = routed_leg_id_for_relay(&live_b_legs, &relay_b);
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key_b,
        SubScope::Global,
        live_b_relay_a_id,
        vec![vec![pk_b.hex()]],
    );
    assert_routed_live_id_author_sets(
        &runtime,
        selected,
        key_b,
        SubScope::Global,
        live_b_relay_b_id,
        vec![vec![pk_c.hex()]],
    );
}

/// Verifies account switch makes old account-scoped subs inactive and restores them on switch-back.
#[test]
fn account_scoped_sub_readiness_transitions_inactive_and_restores_on_switch_back() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays_a = relay_set("wss://relay-a.example.com");
    let relays_b = relay_set("wss://relay-b.example.com");
    let account_a = account_pk(0x0A);
    let account_b = account_pk(0x0B);
    let slot = test_owner!();
    let key = make_key(("account-scoped", 1u8));

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot,
            SubScope::Account,
            key,
            live_config(SubScope::Account),
        )
    });

    let before = owner_status(
        &runtime,
        &mut bridge,
        account_a,
        slot,
        key,
        SubScope::Account,
    );
    assert!(matches!(before, ScopedSubReadiness::Live(_)));

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays_b)
    });

    let old_while_switched = owner_status(
        &runtime,
        &mut bridge,
        account_a,
        slot,
        key,
        SubScope::Account,
    );
    assert_eq!(old_while_switched, ScopedSubReadiness::Inactive);

    let new_missing = owner_status(
        &runtime,
        &mut bridge,
        account_b,
        slot,
        key,
        SubScope::Account,
    );
    assert_eq!(new_missing, ScopedSubReadiness::Missing);

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays_a)
    });

    let restored = owner_status(
        &runtime,
        &mut bridge,
        account_a,
        slot,
        key,
        SubScope::Account,
    );
    assert!(matches!(restored, ScopedSubReadiness::Live(_)));
}

#[test]
fn missing_bridge_fact_clears_cached_readiness_without_clearing_ownership() {
    let (_tmp, mut ndb) = new_ndb();
    let txn = nostrdb::Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn);
    let mut state = ScopedSubsState::default();
    let mut batch = RemoteIntentBatchBuilder::new();
    let owner = SubOwnerKey::new("read-model-missing-owner");
    let key = SubKey::new("read-model-missing-key");
    let identity = ScopedSubIdentity::global(owner, key);

    {
        let mut api = state.api(&accounts, &mut batch);
        assert_eq!(
            api.ensure_sub(identity, base_config(SubScope::Global)),
            EnsureSubResult::Created
        );
    }

    let scoped = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key,
    };
    let live = ScopedSubReadiness::Live(ScopedSubLiveReadiness {
        relay_eose: ScopedSubRelayEoseStatus {
            tracked_relays: 1,
            unsupported_relays: 0,
            any_eose: false,
            all_eosed: false,
        },
    });
    state.apply_bridge_fact(ScopedSubFact::ReadinessChanged {
        scoped: scoped.clone(),
        readiness: live,
    });

    {
        let api = state.api(&accounts, &mut batch);
        assert!(matches!(
            api.sub_readiness(identity),
            ScopedSubReadiness::Live(_)
        ));
    }

    state.apply_bridge_fact(ScopedSubFact::ReadinessChanged {
        scoped,
        readiness: ScopedSubReadiness::Missing,
    });

    {
        let api = state.api(&accounts, &mut batch);
        assert_eq!(api.sub_readiness(identity), ScopedSubReadiness::Inactive);
    }
}

/// Verifies upsert updates a live subscription in place, and replaces it when transport mode changes.
#[test]
fn set_sub_upsert_modifies_live_sub() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let key = SubKey::new(("timeline", 1u64));
    let scope = SubScope::Global;
    let relays_a = relay_set("wss://relay-a.example.com");
    let relays_b = relay_set("wss://relay-b.example.com");
    let slot = test_owner!();

    let mut spec = base_config(scope);
    spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

    let first = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_pk(0x01),
            slot,
            scope,
            key,
            spec.clone(),
        )
    });
    assert!(matches!(first, SetSubResult::Created));

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);
    assert_eq!(
        runtime
            .desired_for_test(&scoped)
            .expect("desired config")
            .filters
            .len(),
        1
    );
    let mut updated = spec.clone();
    updated.filters = send_filters(vec![Filter::new().kinds(vec![3]).limit(1).build()]);

    let res = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_b,
            account_pk(0x01),
            slot,
            scope,
            key,
            updated.clone(),
        )
    });
    assert!(matches!(res, SetSubResult::Updated));

    assert_eq!(single_live_id(&runtime, &scoped), live_id);

    let transparent_update = SubConfig::builder(updated.owned_filters())
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::RequireDedicated,
        ))
        .build();

    let res = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_b,
            account_pk(0x01),
            slot,
            scope,
            key,
            transparent_update.clone(),
        )
    });
    assert!(matches!(res, SetSubResult::Updated));

    let new_live_id = single_live_id(&runtime, &scoped);
    assert_ne!(live_id, new_live_id);
    assert!(!runtime
        .live_sub_ids_for_test(account_pk(0x01), key, scope)
        .contains(&live_id));

    let higher_value_update = SubConfig::builder(transparent_update.owned_filters())
        .accounts_read(relay_policy(
            RelayDemandPriority::Critical,
            RelayRoutingPreference::RequireDedicated,
        ))
        .build();

    let res = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_b,
            account_pk(0x01),
            slot,
            scope,
            key,
            higher_value_update,
        )
    });
    assert!(matches!(res, SetSubResult::Updated));

    let higher_value_live_id = single_live_id(&runtime, &scoped);
    assert_ne!(new_live_id, higher_value_live_id);
    assert!(!runtime
        .live_sub_ids_for_test(account_pk(0x01), key, scope)
        .contains(&new_live_id));
}

/// Verifies clearing the last owner unsubscribes the live outbox subscription and removes desired state.
#[test]
fn clear_sub_unsubscribes_live_subscription() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let key = SubKey::new(("timeline", 1u64));
    let relays = relay_set("wss://relay-a.example.com");
    let slot = test_owner!();

    let mut spec = base_config(SubScope::Global);
    spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

    bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            spec,
        )
    });

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);

    assert!(matches!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                account_pk(0x01),
                slot,
                key,
                SubScope::Global,
            )
        }),
        ClearSubResult::Cleared
    ));

    assert_eq!(runtime.desired_len(), 0);
    assert_eq!(runtime.live_len(), 0);
    assert_eq!(runtime.owner_len(), 0);
    assert!(!runtime.has_live_for_scoped_for_test(&scoped));
    assert!(!runtime
        .live_sub_ids_for_test(account_pk(0x01), key, SubScope::Global)
        .contains(&live_id));

    assert!(matches!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                account_pk(0x01),
                slot,
                key,
                SubScope::Global,
            )
        }),
        ClearSubResult::NotFound
    ));
}

/// Verifies multiple owners share one live sub and only the final clear unsubscribes it.
#[test]
fn multiple_owners_share_single_live_sub_until_last_clear() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let relays = relay_set("wss://relay-a.example.com");
    let account = account_pk(0x33);
    let key = SubKey::new(("thread", [9u8; 32]));

    let mut spec = base_config(SubScope::Account);
    spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(25).build()]);

    let slot_a = test_owner!();
    let slot_b = test_owner!();

    let a = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account,
            slot_a,
            SubScope::Account,
            key,
            spec.clone(),
        )
    });
    let b = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account,
            slot_b,
            SubScope::Account,
            key,
            spec,
        )
    });

    assert!(matches!(a, SetSubResult::Created));
    assert!(matches!(b, SetSubResult::Unchanged));

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key);
    let live_id = single_live_id(&runtime, &scoped);
    assert_eq!(runtime.desired_len(), 1);
    assert_eq!(runtime.live_len(), 1);
    assert_eq!(runtime.owner_len(), 2);
    assert!(runtime.has_live_for_scoped_for_test(&scoped));

    assert!(matches!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                account,
                slot_a,
                key,
                SubScope::Account,
            )
        }),
        ClearSubResult::StillInUse
    ));

    assert_eq!(runtime.desired_len(), 1);
    assert_eq!(runtime.live_len(), 1);
    assert_eq!(runtime.owner_len(), 1);
    assert!(runtime.has_live_for_scoped_for_test(&scoped));

    assert!(matches!(
        bridge.with_returned_outbox(|ids| {
            clear_sub_with_selected_for_test(
                &mut runtime,
                ids,
                account,
                slot_b,
                key,
                SubScope::Account,
            )
        }),
        ClearSubResult::Cleared
    ));

    assert_eq!(runtime.desired_len(), 0);
    assert_eq!(runtime.live_len(), 0);
    assert_eq!(runtime.owner_len(), 0);
    assert!(!runtime.has_live_for_scoped_for_test(&scoped));
    assert!(!runtime
        .live_sub_ids_for_test(account, key, SubScope::Account)
        .contains(&live_id));
}

#[test]
fn multiple_owners_with_same_key_union_additive_relays_until_owner_drop() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_relays = relay_set("wss://account.example.com");
    let account = account_pk(0x33);
    let key = SubKey::new(("thread", "same-root"));
    let filter = Filter::new()
        .kinds(vec![1])
        .event(&[0x11; 32])
        .limit(500)
        .build();
    let relay_a = NormRelayUrl::new("wss://observed-a.example.com").unwrap();
    let relay_b = NormRelayUrl::new("wss://observed-b.example.com").unwrap();
    let slot_a = test_owner!();
    let slot_b = test_owner!();

    let config_a =
        accounts_read_plus_observed_config(vec![filter.clone()], HashSet::from([relay_a.clone()]));
    let config_b =
        accounts_read_plus_observed_config(vec![filter], HashSet::from([relay_b.clone()]));

    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_sub_with_relays_for_test(
                &mut runtime,
                ids,
                &account_relays,
                account,
                slot_a,
                SubScope::Account,
                key,
                config_a,
            )
        }),
        SetSubResult::Created
    );
    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_sub_with_relays_for_test(
                &mut runtime,
                ids,
                &account_relays,
                account,
                slot_b,
                SubScope::Account,
                key,
                config_b,
            )
        }),
        SetSubResult::Updated
    );
    let live_ids = runtime.live_sub_ids_for_test(account, key, SubScope::Account);
    assert_eq!(live_ids.len(), 2);
    assert_eq!(
        desired_explicit_relays(
            &runtime,
            &ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key)
        ),
        HashSet::from([relay_a.clone(), relay_b.clone()])
    );
    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &account_relays, slot_b)
    }));

    let live_ids = runtime.live_sub_ids_for_test(account, key, SubScope::Account);
    assert_eq!(live_ids.len(), 2);
    assert_eq!(
        desired_explicit_relays(
            &runtime,
            &ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key)
        ),
        HashSet::from([relay_a])
    );
}

#[test]
fn plain_accounts_read_owner_does_not_clear_another_owners_additive_relays() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_relays = relay_set("wss://account.example.com");
    let account = account_pk(0x33);
    let key = SubKey::new(("thread", "same-root-with-plain-owner"));
    let filter = Filter::new()
        .kinds(vec![1])
        .event(&[0x11; 32])
        .limit(500)
        .build();
    let observed_relay = NormRelayUrl::new("wss://observed.example.com").unwrap();
    let slot_additive = test_owner!();
    let slot_plain = test_owner!();

    let additive_config = accounts_read_plus_observed_config(
        vec![filter.clone()],
        HashSet::from([observed_relay.clone()]),
    );
    let plain_config = SubConfig::builder(vec![filter])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        ))
        .build();

    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_sub_with_relays_for_test(
                &mut runtime,
                ids,
                &account_relays,
                account,
                slot_additive,
                SubScope::Account,
                key,
                additive_config,
            )
        }),
        SetSubResult::Created
    );
    assert_eq!(
        bridge.with_returned_outbox(|ids| {
            set_sub_with_relays_for_test(
                &mut runtime,
                ids,
                &account_relays,
                account,
                slot_plain,
                SubScope::Account,
                key,
                plain_config,
            )
        }),
        SetSubResult::Unchanged
    );
    let live_ids = runtime.live_sub_ids_for_test(account, key, SubScope::Account);
    assert_eq!(live_ids.len(), 2);
    assert_eq!(
        desired_explicit_relays(
            &runtime,
            &ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key)
        ),
        HashSet::from([observed_relay])
    );

    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &account_relays, slot_additive)
    }));

    let live_ids = runtime.live_sub_ids_for_test(account, key, SubScope::Account);
    assert_eq!(live_ids.len(), 1);
    assert!(desired_explicit_relays(
        &runtime,
        &ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key)
    )
    .is_empty());
}

/// Verifies dropping an owner clears every scoped sub owned by that owner.
#[test]
fn drop_owner_clears_all_owned_subs() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account = account_pk(0x4A);
    let relays = relay_set("wss://relay-a.example.com");
    let owner = test_owner!();

    let key_account = SubKey::new(("timeline", "home"));
    let key_global = SubKey::new(("global", "discovery"));

    let mut account_spec = base_config(SubScope::Account);
    account_spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(5).build()]);

    let mut global_spec = base_config(SubScope::Global);
    global_spec.filters = send_filters(vec![Filter::new().kinds(vec![0]).limit(5).build()]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account,
            owner,
            SubScope::Account,
            key_account,
            account_spec,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account,
            owner,
            SubScope::Global,
            key_global,
            global_spec,
        )
    });

    assert_eq!(runtime.desired_len(), 2);
    assert_eq!(runtime.live_len(), 2);
    assert_eq!(runtime.owner_len(), 1);

    assert!(bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &relays, owner)
    }));

    assert_eq!(runtime.desired_len(), 0);
    assert_eq!(runtime.live_len(), 0);
    assert_eq!(runtime.owner_len(), 0);

    assert!(!bridge.with_returned_outbox(|ids| {
        drop_owner_with_relays_for_test(&mut runtime, ids, account, &relays, owner)
    }));
}

/// Verifies account switch unsubscribes the old account scope and restores it when switching back.
#[test]
fn account_switch_unsubscribes_old_scope_and_restores_new_scope() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xAA);
    let account_b = account_pk(0xBB);
    let relays_a = relay_set("wss://relay-a.example.com");
    let relays_b = relay_set("wss://relay-b.example.com");
    let key = SubKey::new(("timeline", "account-scoped"));
    let slot = test_owner!();

    let mut scoped_spec = base_config(SubScope::Account);
    scoped_spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot,
            SubScope::Account,
            key,
            scoped_spec,
        )
    });

    let scoped_a = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key);
    let initial_live_id = single_live_id(&runtime, &scoped_a);
    assert!(runtime.has_live_for_scoped_for_test(&scoped_a));

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays_b)
    });

    assert!(!runtime.has_live_for_scoped_for_test(&scoped_a));
    assert!(!runtime
        .live_sub_ids_for_test(account_a, key, SubScope::Account)
        .contains(&initial_live_id));
    assert_eq!(runtime.desired_len(), 1);

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays_a)
    });

    let restored_live_id = runtime.single_live_id_for_scoped_for_test(&scoped_a);
    assert_ne!(restored_live_id, initial_live_id);
    assert!(runtime.has_live_for_scoped_for_test(&scoped_a));
}

#[test]
fn account_deletion_purges_desired_and_live_state_for_deleted_scope() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xA5);
    let account_b = account_pk(0xB5);
    let relays = relay_set("wss://relay-account-delete.example.com");
    let global_relays = relay_set("wss://relay-global-delete.example.com");

    let slot_account_a = test_owner!();
    let slot_global = test_owner!();
    let slot_account_b = test_owner!();
    let key_account = make_key((FakeApp::Threads, "deleted-account"));
    let key_global = make_key((FakeApp::Timelines, "global-after-delete"));
    let key_account_b = make_key((FakeApp::Messages, "other-account"));

    let mut account_spec = base_config(SubScope::Account);
    account_spec.filters = send_filters(vec![Filter::new().kinds(vec![1]).limit(5).build()]);
    let mut global_spec = base_config(SubScope::Global);
    global_spec.filters = send_filters(vec![Filter::new().kinds(vec![0]).limit(5).build()]);
    let mut account_b_spec = base_config(SubScope::Account);
    account_b_spec.filters = send_filters(vec![Filter::new().kinds(vec![4]).limit(5).build()]);

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_a,
            slot_account_a,
            SubScope::Account,
            key_account,
            account_spec,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &global_relays,
            account_a,
            slot_global,
            SubScope::Global,
            key_global,
            global_spec,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            account_b,
            slot_account_b,
            SubScope::Account,
            key_account_b,
            account_b_spec,
        )
    });

    let scoped_a = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_account);
    let scoped_global = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key_global);
    let scoped_b =
        ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_b), key_account_b);
    let live_a = single_live_id(&runtime, &scoped_a);

    bridge.with_returned_outbox(|ids| {
        runtime.purge_account_scope(ids, account_a, &relays, account_a)
    });

    assert_eq!(runtime.desired_len(), 2);
    assert_eq!(runtime.live_len(), 2);
    assert!(!runtime.has_live_for_scoped_for_test(&scoped_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_global));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_b));

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays)
    });

    assert!(
        !runtime.has_live_for_scoped_for_test(&scoped_a),
        "deleted account scoped desired state must not restore on reselect"
    );
    assert!(!runtime
        .live_sub_ids_for_test(account_a, key_account, SubScope::Account)
        .contains(&live_a));
}

/// Verifies account-scoped and global subscriptions obey the account-switch contract across app domains.
#[test]
fn account_switch_contract_with_multiple_apps_and_mixed_scopes() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let account_a = account_pk(0xA1);
    let account_b = account_pk(0xB2);
    let peer_pk = account_pk(0xCC);

    let relays_a = relay_set("wss://relay-a.example.com");
    let relays_b = relay_set("wss://relay-b.example.com");

    let key_timeline_a = make_key((FakeApp::Timelines, "home", 1u64, account_a));
    let key_thread_a = make_key((FakeApp::Threads, "root", [7u8; 32], account_a));
    let key_messages_a = make_key((FakeApp::Messages, "dm-relay-list", peer_pk, account_a));
    let key_global = make_key((FakeApp::Timelines, "global-discovery", 99u64));

    let timeline_spec_a = SubConfig::builder(vec![Filter::new().kinds(vec![1]).limit(50).build()])
        .accounts_read_important()
        .build();

    let thread_spec_a = SubConfig::builder(vec![Filter::new().kinds(vec![1]).limit(200).build()])
        .accounts_read(relay_policy(
            RelayDemandPriority::Important,
            RelayRoutingPreference::RequireDedicated,
        ))
        .build();

    let messages_spec_a =
        SubConfig::builder(vec![Filter::new().kinds(vec![10002]).limit(20).build()])
            .accounts_read_important()
            .build();

    let global_spec = SubConfig::builder(vec![Filter::new().kinds(vec![0]).limit(10).build()])
        .accounts_read_important()
        .build();

    let slot_timeline = test_owner!();
    let slot_thread = test_owner!();
    let slot_messages = test_owner!();
    let slot_global = test_owner!();

    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot_timeline,
            SubScope::Account,
            key_timeline_a,
            timeline_spec_a,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot_thread,
            SubScope::Account,
            key_thread_a,
            thread_spec_a,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot_messages,
            SubScope::Account,
            key_messages_a,
            messages_spec_a,
        )
    });
    let _ = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays_a,
            account_a,
            slot_global,
            SubScope::Global,
            key_global,
            global_spec,
        )
    });

    let scoped_timeline_a =
        ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_timeline_a);
    let scoped_thread_a =
        ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_thread_a);
    let scoped_messages_a =
        ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_messages_a);
    let scoped_global = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key_global);

    assert!(runtime.has_live_for_scoped_for_test(&scoped_timeline_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_thread_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_messages_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_global));

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_a, account_b, &relays_b)
    });

    assert!(
        !runtime.has_live_for_scoped_for_test(&scoped_timeline_a)
            && !runtime.has_live_for_scoped_for_test(&scoped_thread_a)
            && !runtime.has_live_for_scoped_for_test(&scoped_messages_a)
    );
    assert!(runtime.has_live_for_scoped_for_test(&scoped_global));
    assert_eq!(runtime.desired_len(), 4);

    bridge.with_returned_outbox(|ids| {
        on_account_switched_with_relays_for_test(&mut runtime, ids, account_b, account_a, &relays_a)
    });

    assert!(runtime.has_live_for_scoped_for_test(&scoped_timeline_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_thread_a));
    assert!(runtime.has_live_for_scoped_for_test(&scoped_messages_a));
}

/// Verifies account switching retargets global subscriptions that depend on AccountsRead.
#[test]
fn account_switch_retargets_global_accountsread_subs() {
    let mut t = RetargetReadRelaysTest::new();

    let global_feed = t.submit_accountsread_global_feed();

    t.switch_selected_account_away();

    t.assert_live_id_unchanged(&global_feed);
    t.assert_still_live(&global_feed);
}

#[derive(Clone)]
struct SubmittedSub {
    scoped: ScopedSubKey,
    live_id: OutboxSubId,
}

// Scenario harness for selected-account read-relay retarget tests.
// Keep this narrow; it is intentionally not a generic scoped-subs fixture.
struct RetargetReadRelaysTest {
    runtime: ScopedSubRuntime,
    bridge: RemoteOutboxReadModelHarness,
    selected_account: Pubkey,
    other_account: Pubkey,
    relay_a: HashSet<NormRelayUrl>,
    relay_b: HashSet<NormRelayUrl>,
}

impl RetargetReadRelaysTest {
    fn new() -> Self {
        let (runtime, bridge) = scoped_sub_test_runtime();
        Self {
            runtime,
            bridge,
            selected_account: account_pk(0xA1),
            other_account: account_pk(0xB2),
            relay_a: relay_set("wss://relay-a.example.com"),
            relay_b: relay_set("wss://relay-b.example.com"),
        }
    }

    fn submit_accountsread_account_home(&mut self) -> SubmittedSub {
        self.submit_sub(
            SubScope::Account,
            make_key((FakeApp::Timelines, "home", 1u64)),
            accountsread_spec(SubScope::Account, 1, 50),
        )
    }

    fn submit_accountsread_global_feed(&mut self) -> SubmittedSub {
        self.submit_sub(
            SubScope::Global,
            make_key((FakeApp::Timelines, "global-ish", 2u64)),
            accountsread_spec(SubScope::Global, 0, 10),
        )
    }

    fn submit_accountsread_account_messages(&mut self) -> SubmittedSub {
        self.submit_sub(
            SubScope::Account,
            make_key((FakeApp::Messages, "relay-list", 3u64)),
            accountsread_spec(SubScope::Account, 10002, 1),
        )
    }

    fn submit_accountsread_other_account_home(&mut self) -> SubmittedSub {
        self.submit_sub_for_account(
            self.other_account,
            SubScope::Account,
            make_key((FakeApp::Timelines, "home", 99u64)),
            accountsread_spec(SubScope::Account, 1, 25),
        )
    }

    fn submit_sub(&mut self, scope: SubScope, key: SubKey, spec: SubConfig) -> SubmittedSub {
        self.submit_sub_for_account(self.selected_account, scope, key, spec)
    }

    fn submit_sub_for_account(
        &mut self,
        account: Pubkey,
        scope: SubScope,
        key: SubKey,
        spec: SubConfig,
    ) -> SubmittedSub {
        let slot = test_owner!();
        let _ = self.bridge.with_returned_outbox(|ids| {
            set_sub_with_relays_for_test(
                &mut self.runtime,
                ids,
                &self.relay_a,
                account,
                slot,
                scope,
                key,
                spec,
            )
        });

        let resolved_scope = match scope {
            SubScope::Account => ResolvedSubScope::Account(account),
            SubScope::Global => ResolvedSubScope::Global,
        };
        let scoped = ScopedSubRuntime::scoped_key(resolved_scope, key);
        let live_id = single_live_id(&self.runtime, &scoped);

        SubmittedSub { scoped, live_id }
    }

    fn retarget_to_relay_b(&mut self) {
        self.bridge.with_returned_outbox(|ids| {
            retarget_selected_account_read_relays_with_relays_for_test(
                &mut self.runtime,
                ids,
                self.selected_account,
                &self.relay_b,
            )
        });
    }

    fn retarget_to_empty_relays(&mut self) {
        self.bridge.with_returned_outbox(|ids| {
            retarget_selected_account_read_relays_with_relays_for_test(
                &mut self.runtime,
                ids,
                self.selected_account,
                &HashSet::new(),
            )
        });
    }

    fn assert_live_id_unchanged(&self, sub: &SubmittedSub) {
        assert_eq!(
            self.runtime.single_live_id_for_scoped_for_test(&sub.scoped),
            sub.live_id
        )
    }

    fn assert_still_live(&self, sub: &SubmittedSub) {
        assert!(self.runtime.has_live_for_scoped_for_test(&sub.scoped));
        assert_eq!(
            self.runtime.single_live_id_for_scoped_for_test(&sub.scoped),
            sub.live_id
        )
    }

    fn switch_selected_account_away(&mut self) {
        self.bridge.with_returned_outbox(|ids| {
            on_account_switched_with_relays_for_test(
                &mut self.runtime,
                ids,
                self.selected_account,
                self.other_account,
                &self.relay_b,
            )
        });
    }

    fn assert_not_live(&self, sub: &SubmittedSub) {
        assert!(!self.runtime.has_live_for_scoped_for_test(&sub.scoped));
    }

    fn assert_live_recreated(&self, sub: &SubmittedSub) {
        let recreated_live_id = self.runtime.single_live_id_for_scoped_for_test(&sub.scoped);
        assert_ne!(recreated_live_id, sub.live_id);
        assert!(self.runtime.has_live_for_scoped_for_test(&sub.scoped));
    }
}

/// Verifies selected-account relay list refresh retargets all AccountsRead subs in scope.
#[test]
fn selected_account_relay_refresh_updates_account_and_global_accountsread_subs() {
    let mut t = RetargetReadRelaysTest::new();

    let account_home = t.submit_accountsread_account_home();
    let global_feed = t.submit_accountsread_global_feed();
    let account_messages = t.submit_accountsread_account_messages();

    t.retarget_to_relay_b();

    t.assert_live_id_unchanged(&account_home);
    t.assert_live_id_unchanged(&global_feed);
    t.assert_live_id_unchanged(&account_messages);

    t.assert_still_live(&account_home);
    t.assert_still_live(&global_feed);
    t.assert_still_live(&account_messages);
}

/// Verifies retargeting recreates a missing live AccountsRead sub from desired state.
#[test]
fn selected_account_relay_retarget_recreates_missing_live_sub() {
    let mut t = RetargetReadRelaysTest::new();

    let account_home = t.submit_accountsread_account_home();
    t.switch_selected_account_away();
    t.assert_not_live(&account_home);

    t.retarget_to_relay_b();

    t.assert_live_recreated(&account_home);
}

/// Verifies retargeting to no selected-account read relays removes live state.
#[test]
fn selected_account_relay_retarget_to_empty_relays_makes_sub_inactive() {
    let mut t = RetargetReadRelaysTest::new();

    let account_home = t.submit_accountsread_account_home();
    t.retarget_to_empty_relays();

    t.assert_not_live(&account_home);
    assert!(t.runtime.store.contains_desired(&account_home.scoped));
}

/// Verifies updating an existing single AccountsRead sub to an empty resolved relay set removes live state.
#[test]
fn set_sub_modify_existing_to_empty_accountsread_relays_makes_sub_inactive() {
    let (mut runtime, mut bridge) = scoped_sub_test_runtime();
    let selected = account_pk(0x01);
    let slot = test_owner!();
    let key = make_key(("single-modify-empty-relays", 1u8));
    let scope = SubScope::Global;
    let relays = relay_set("wss://relay-a.example.com");
    let initial = accountsread_spec(scope, 1, 50);

    let created = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &relays,
            selected,
            slot,
            scope,
            key,
            initial,
        )
    });
    assert_eq!(created, SetSubResult::Created);

    let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
    let live_id = single_live_id(&runtime, &scoped);
    let updated = bridge.with_returned_outbox(|ids| {
        set_sub_with_relays_for_test(
            &mut runtime,
            ids,
            &HashSet::new(),
            selected,
            slot,
            scope,
            key,
            accountsread_spec(scope, 1, 25),
        )
    });

    assert_eq!(updated, SetSubResult::Updated);
    assert!(!runtime.has_live_for_scoped_for_test(&scoped));
    assert!(!runtime
        .live_sub_ids_for_test(selected, key, scope)
        .contains(&live_id));
    assert!(runtime.store.contains_desired(&scoped));
}

/// Verifies retargeting the selected account does not touch another account's account-scoped sub.
#[test]
fn selected_account_relay_retarget_ignores_other_account_scoped_subs() {
    let mut t = RetargetReadRelaysTest::new();

    let selected_account_home = t.submit_accountsread_account_home();
    let other_account_home = t.submit_accountsread_other_account_home();

    t.retarget_to_relay_b();

    t.assert_live_id_unchanged(&selected_account_home);
    t.assert_live_id_unchanged(&other_account_home);
    t.assert_still_live(&selected_account_home);
    t.assert_still_live(&other_account_home);
}

/// Verifies typed SubKey builder output is stable for identical inputs.
#[test]
fn subkey_builder_is_stable_and_typed() {
    let key_a = SubKey::builder(FakeApp::Messages)
        .with("dm-relay-list")
        .with(account_pk(0x11))
        .with(42u64)
        .finish();
    let key_b = SubKey::builder(FakeApp::Messages)
        .with("dm-relay-list")
        .with(account_pk(0x11))
        .with(42u64)
        .finish();
    let key_c = SubKey::builder(FakeApp::Messages)
        .with("dm-relay-list")
        .with(account_pk(0x11))
        .with(43u64)
        .finish();

    assert_eq!(key_a, key_b);
    assert_ne!(key_a, key_c);
}
