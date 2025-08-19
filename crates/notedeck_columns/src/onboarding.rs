use enostr::{Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use notedeck::{create_nip51_set, filter::default_limit, Nip51SetCache, UnknownIds};
use uuid::Uuid;

use crate::subscriptions::Subscriptions;

#[derive(Debug)]
enum OnboardingState {
    AwaitingTrustedPksList(Vec<Filter>),
    HaveFollowPacks(Nip51SetCache),
}

/// Manages the onboarding process. Responsible for retriving the kind 30000 list of trusted pubkeys
/// and then retrieving all follow packs from the trusted pks updating when new ones arrive
#[derive(Default)]
pub struct Onboarding {
    state: Option<Result<OnboardingState, OnboardingError>>,
}

impl Onboarding {
    pub fn get_follow_packs(&self) -> Option<&Nip51SetCache> {
        let Some(Ok(OnboardingState::HaveFollowPacks(packs))) = &self.state else {
            return None;
        };

        Some(packs)
    }

    pub fn get_follow_packs_mut(&mut self) -> Option<&mut Nip51SetCache> {
        let Some(Ok(OnboardingState::HaveFollowPacks(packs))) = &mut self.state else {
            return None;
        };

        Some(packs)
    }

    pub fn process(
        &mut self,
        pool: &mut RelayPool,
        ndb: &Ndb,
        subs: &mut Subscriptions,
        unknown_ids: &mut UnknownIds,
    ) {
        match &self.state {
            Some(res) => {
                let Ok(OnboardingState::AwaitingTrustedPksList(filter)) = res else {
                    return;
                };

                let txn = Transaction::new(ndb).expect("txns");
                let Ok(res) = ndb.query(&txn, filter, 1) else {
                    return;
                };

                if res.is_empty() {
                    return;
                }

                let key = res.first().expect("checked empty").note_key;

                let new_state = get_trusted_authors(ndb, &txn, key).and_then(|trusted_pks| {
                    let pks: Vec<&[u8; 32]> = trusted_pks.iter().map(|f| f.bytes()).collect();
                    Nip51SetCache::new(pool, ndb, &txn, unknown_ids, vec![follow_packs_filter(pks)])
                        .map(OnboardingState::HaveFollowPacks)
                        .ok_or(OnboardingError::InvalidNip51Set)
                });

                self.state = Some(new_state);
            }
            None => {
                let filter = vec![trusted_pks_list_filter()];

                let subid = Uuid::new_v4().to_string();
                pool.subscribe(subid.clone(), filter.clone());
                subs.subs
                    .insert(subid, crate::subscriptions::SubKind::OneShot);

                let new_state = Some(Ok(OnboardingState::AwaitingTrustedPksList(filter)));
                self.state = new_state;
            }
        }
    }

    // Unsubscribe and clear state
    pub fn end_onboarding(&mut self, pool: &mut RelayPool, ndb: &mut Ndb) {
        let Some(Ok(OnboardingState::HaveFollowPacks(state))) = &mut self.state else {
            self.state = None;
            return;
        };

        let unified = &state.sub;

        pool.unsubscribe(unified.remote.clone());
        let _ = ndb.unsubscribe(unified.local);

        self.state = None;
    }
}

#[derive(Debug)]
pub enum OnboardingError {
    InvalidNip51Set,
    InvalidTrustedPksListKind,
    NdbCouldNotFindNote,
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
