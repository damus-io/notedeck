use std::{cell::RefCell, rc::Rc};

use egui_virtual_list::VirtualList;
use enostr::Pubkey;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use notedeck::{
    create_nip51_set, filter::default_limit, Nip51SetCache, RelaySelection, ScopedSubApi,
    ScopedSubIdentity, SubConfig, SubKey, SubOwnerKey, UnknownIds,
};

#[derive(Debug)]
enum OnboardingState {
    AwaitingTrustedPksList(Vec<Filter>),
    HaveFollowPacks { packs: Nip51SetCache },
}

/// Manages onboarding discovery of trusted follow packs.
///
/// This first requests the trusted-author list (kind `30000`) and then
/// installs a scoped account subscription for follow packs from those authors.
#[derive(Default)]
pub struct Onboarding {
    state: Option<Result<OnboardingState, OnboardingError>>,
    pub list: Rc<RefCell<VirtualList>>,
}

/// Side effects emitted by one `Onboarding::process` pass.
pub enum OnboardingEffect {
    /// Request a one-shot fetch for the provided filters.
    Oneshot(Vec<Filter>),
}

impl Onboarding {
    pub fn get_follow_packs(&self) -> Option<&Nip51SetCache> {
        let Some(Ok(OnboardingState::HaveFollowPacks { packs, .. })) = &self.state else {
            return None;
        };

        Some(packs)
    }

    pub fn get_follow_packs_mut(&mut self) -> Option<&mut Nip51SetCache> {
        let Some(Ok(OnboardingState::HaveFollowPacks { packs, .. })) = &mut self.state else {
            return None;
        };

        Some(packs)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        scoped_subs: &mut ScopedSubApi<'_, '_>,
        owner: SubOwnerKey,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
    ) -> Option<OnboardingEffect> {
        match &self.state {
            Some(res) => {
                let Ok(OnboardingState::AwaitingTrustedPksList(filter)) = res else {
                    return None;
                };

                let txn = Transaction::new(ndb).expect("txns");
                let Ok(res) = ndb.query(&txn, filter, 1) else {
                    return None;
                };

                if res.is_empty() {
                    return None;
                }

                let key = res.first().expect("checked empty").note_key;

                let new_state = get_trusted_authors(ndb, &txn, key).and_then(|trusted_pks| {
                    let pks: Vec<&[u8; 32]> = trusted_pks.iter().map(|f| f.bytes()).collect();
                    let follow_filter = follow_packs_filter(pks);
                    let sub_key = follow_packs_sub_key();
                    let identity = ScopedSubIdentity::account(owner, sub_key);
                    let sub_config = SubConfig {
                        relays: RelaySelection::AccountsRead,
                        filters: vec![follow_filter.clone()],
                        use_transparent: false,
                    };
                    let _ = scoped_subs.ensure_sub(identity, sub_config);

                    Nip51SetCache::new_local(ndb, &txn, unknown_ids, vec![follow_filter])
                        .map(|packs| OnboardingState::HaveFollowPacks { packs })
                        .ok_or(OnboardingError::InvalidNip51Set)
                });

                self.state = Some(new_state);
                None
            }
            None => {
                let filter = vec![trusted_pks_list_filter()];
                let new_state = Some(Ok(OnboardingState::AwaitingTrustedPksList(filter)));
                self.state = new_state;
                let Some(Ok(OnboardingState::AwaitingTrustedPksList(filters))) = &self.state else {
                    return None;
                };

                Some(OnboardingEffect::Oneshot(filters.clone()))
            }
        }
    }

    // Unsubscribe and clear state
    pub fn end_onboarding(&mut self, ndb: &mut Ndb) {
        let Some(Ok(OnboardingState::HaveFollowPacks { packs })) = &mut self.state else {
            self.state = None;
            return;
        };

        let _ = ndb.unsubscribe(packs.local_sub());

        self.state = None;
    }
}

#[derive(Debug)]
pub enum OnboardingError {
    /// Follow-pack note could not be parsed as a valid NIP-51 set.
    InvalidNip51Set,
    /// Trusted-author note exists but is not kind `30000`.
    InvalidTrustedPksListKind,
    /// Trusted-author note key could not be resolved from NostrDB.
    NdbCouldNotFindNote,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum OnboardingScopedSub {
    FollowPacks,
}

// author providing the list of trusted follow pack authors
const FOLLOW_PACK_AUTHOR: [u8; 32] = [
    0x89, 0x5c, 0x2a, 0x90, 0xa8, 0x60, 0xac, 0x18, 0x43, 0x4a, 0xa6, 0x9e, 0x7b, 0x0d, 0xa8, 0x46,
    0x57, 0x21, 0x21, 0x6f, 0xa3, 0x6e, 0x42, 0xc0, 0x22, 0xe3, 0x93, 0x57, 0x9c, 0x48, 0x6c, 0xba,
];

fn trusted_pks_list_filter() -> Filter {
    Filter::new()
        .kinds([30000])
        .limit(1)
        .authors(&[FOLLOW_PACK_AUTHOR])
        .tags(["trusted-follow-pack-authors"], 'd')
        .build()
}

pub fn follow_packs_filter(pks: Vec<&[u8; 32]>) -> Filter {
    Filter::new()
        .kinds([39089])
        .limit(default_limit())
        .authors(pks)
        .build()
}

fn follow_packs_sub_key() -> SubKey {
    SubKey::builder(OnboardingScopedSub::FollowPacks).finish()
}

/// gets the pubkeys from a kind 30000 follow set
fn get_trusted_authors(
    ndb: &Ndb,
    txn: &Transaction,
    key: NoteKey,
) -> Result<Vec<Pubkey>, OnboardingError> {
    let Ok(note) = ndb.get_note_by_key(txn, key) else {
        return Result::Err(OnboardingError::NdbCouldNotFindNote);
    };

    if note.kind() != 30000 {
        return Result::Err(OnboardingError::InvalidTrustedPksListKind);
    }

    let Some(nip51set) = create_nip51_set(note) else {
        return Result::Err(OnboardingError::InvalidNip51Set);
    };

    Ok(nip51set.pks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::{OutboxPool, OutboxSessionHandler};
    use nostrdb::Config;
    use notedeck::{Accounts, EguiWakeup, ScopedSubsState, FALLBACK_PUBKEY};
    use tempfile::TempDir;

    fn test_harness() -> (
        TempDir,
        Ndb,
        Accounts,
        UnknownIds,
        ScopedSubsState,
        OutboxPool,
    ) {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();
        let accounts = Accounts::new(
            None,
            vec!["wss://relay-onboarding.example.com".to_owned()],
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );

        (
            tmp,
            ndb,
            accounts,
            unknown_ids,
            ScopedSubsState::default(),
            OutboxPool::default(),
        )
    }

    /// Verifies onboarding emits a one-time oneshot effect on first process call
    /// and does not emit duplicate oneshot effects on subsequent calls.
    #[test]
    fn process_initially_emits_oneshot_effect_once() {
        let (_tmp, ndb, accounts, mut unknown_ids, mut scoped_sub_state, mut pool) = test_harness();
        let owner = SubOwnerKey::new(("onboarding", 1usize));
        let mut onboarding = Onboarding::default();

        let first = {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut scoped_subs = scoped_sub_state.api(&mut outbox, &accounts);
            onboarding.process(&mut scoped_subs, owner, &ndb, &mut unknown_ids)
        };

        match first {
            Some(OnboardingEffect::Oneshot(filters)) => {
                assert_eq!(filters.len(), 1);
                assert_eq!(
                    filters[0].json().expect("json"),
                    trusted_pks_list_filter().json().expect("json")
                );
            }
            None => panic!("expected onboarding oneshot effect"),
        }

        let second = {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut scoped_subs = scoped_sub_state.api(&mut outbox, &accounts);
            onboarding.process(&mut scoped_subs, owner, &ndb, &mut unknown_ids)
        };

        assert!(second.is_none());
    }
}
