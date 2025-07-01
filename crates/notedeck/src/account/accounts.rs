use tracing::debug;
use uuid::Uuid;

use crate::account::cache::AccountCache;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{
    modify_advertised_relays, update_relay_configuration, AccountRelayData, RelayAction,
    RelayDefaults,
};
use crate::storage::AccountStorageWriter;
use crate::user_account::UserAccountSerializable;
use crate::{
    AccountStorage, MuteFun, SingleUnkIdAction, UnifiedSubscription, UnknownIds, UserAccount,
    ZapWallet,
};
use enostr::{ClientMessage, FilledKeypair, Keypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, Transaction};

// TODO: remove this
use std::sync::Arc;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    pub cache: AccountCache,
    storage_writer: Option<AccountStorageWriter>,
    relay_defaults: RelayDefaults,
    subs: AccountSubs,
}

impl Accounts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key_store: Option<AccountStorage>,
        forced_relays: Vec<String>,
        fallback: Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut RelayPool,
        ctx: &egui::Context,
        unknown_ids: &mut UnknownIds,
    ) -> Self {
        let (mut cache, unknown_id) = AccountCache::new(UserAccount::new(
            Keypair::only_pubkey(fallback),
            AccountData {
                relay: AccountRelayData::new(ndb, txn, fallback.bytes()),
                muted: AccountMutedData::new(ndb, txn, fallback.bytes()),
            },
        ));

        unknown_id.process_action(unknown_ids, ndb, txn);

        let mut storage_writer = None;
        if let Some(keystore) = key_store {
            let (reader, writer) = keystore.rw();
            match reader.get_accounts() {
                Ok(accounts) => {
                    for account in accounts {
                        add_account_from_storage(&mut cache, ndb, txn, account).process_action(
                            unknown_ids,
                            ndb,
                            txn,
                        )
                    }
                }
                Err(e) => {
                    tracing::error!("could not get keys: {e}");
                }
            }
            if let Some(selected) = reader.get_selected_key().ok().flatten() {
                cache.select(selected);
            }

            storage_writer = Some(writer);
        };

        let relay_defaults = RelayDefaults::new(forced_relays);

        let selected = cache.selected();
        let selected_data = &selected.data;

        let subs = {
            AccountSubs::new(
                ndb,
                pool,
                &relay_defaults,
                &selected.key.pubkey,
                selected_data,
                create_wakeup(ctx),
            )
        };

        Accounts {
            cache,
            storage_writer,
            relay_defaults,
            subs,
        }
    }

    pub fn remove_account(&mut self, pk: &Pubkey) {
        let Some(removed) = self.cache.remove(pk) else {
            return;
        };

        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.remove_key(&removed.key) {
                tracing::error!("Could not remove account {pk}: {e}");
            }
        }
    }

    pub fn contains_full_kp(&self, pubkey: &enostr::Pubkey) -> bool {
        self.cache
            .get(pubkey)
            .is_some_and(|u| u.key.secret_key.is_some())
    }

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        kp: Keypair,
    ) -> Option<AddAccountResponse> {
        let acc = if let Some(acc) = self.cache.get_mut(&kp.pubkey) {
            if kp.secret_key.is_none() || acc.key.secret_key.is_some() {
                tracing::info!("Already have account, not adding");
                return None;
            }

            acc.key = kp.clone();
            AccType::Acc(&*acc)
        } else {
            let new_account_data = AccountData {
                relay: AccountRelayData::new(ndb, txn, kp.pubkey.bytes()),
                muted: AccountMutedData::new(ndb, txn, kp.pubkey.bytes()),
            };
            AccType::Entry(
                self.cache
                    .add(UserAccount::new(kp.clone(), new_account_data)),
            )
        };

        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.write_account(&acc.get_acc().into()) {
                tracing::error!("Could not add key for {:?}: {e}", kp.pubkey);
            }
        }

        Some(AddAccountResponse {
            switch_to: kp.pubkey,
            unk_id_action: SingleUnkIdAction::pubkey(kp.pubkey),
        })
    }

    /// Update the `UserAccount` via callback and save the result to disk.
    /// return true if the update was successful
    pub fn update_current_account(&mut self, update: impl FnOnce(&mut UserAccount)) -> bool {
        let cur_account = self.get_selected_account_mut();

        update(cur_account);

        let cur_acc = self.get_selected_account();

        let Some(key_store) = &self.storage_writer else {
            return false;
        };

        if let Err(err) = key_store.write_account(&cur_acc.into()) {
            tracing::error!("Could not add account {:?} to storage: {err}", cur_acc.key);
            return false;
        }

        true
    }

    pub fn selected_filled(&self) -> Option<FilledKeypair<'_>> {
        self.get_selected_account().key.to_full()
    }

    /// Get the selected account's pubkey as bytes. Common operation so
    /// we make it a helper here.
    pub fn selected_account_pubkey_bytes(&self) -> &[u8; 32] {
        self.get_selected_account().key.pubkey.bytes()
    }

    pub fn selected_account_pubkey(&self) -> &Pubkey {
        &self.get_selected_account().key.pubkey
    }

    pub fn get_selected_account(&self) -> &UserAccount {
        self.cache.selected()
    }

    pub fn selected_account_has_wallet(&self) -> bool {
        self.get_selected_account().wallet.is_some()
    }

    fn get_selected_account_mut(&mut self) -> &mut UserAccount {
        self.cache.selected_mut()
    }

    pub fn get_selected_wallet_mut(&mut self) -> Option<&mut ZapWallet> {
        self.cache.selected_mut().wallet.as_mut()
    }

    fn get_selected_account_data(&self) -> &AccountData {
        &self.cache.selected().data
    }

    pub fn select_account(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        ctx: &egui::Context,
    ) {
        if !self.cache.select(*pk_to_select) {
            return;
        }

        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.select_key(Some(*pk_to_select)) {
                tracing::error!("Could not select key {:?}: {e}", pk_to_select);
            }
        }

        self.subs.swap_to(
            ndb,
            pool,
            &self.relay_defaults,
            pk_to_select,
            &self.cache.selected().data,
            create_wakeup(ctx),
        );
    }

    pub fn mutefun(&self) -> Box<MuteFun> {
        let account_data = self.get_selected_account_data();

        let muted = Arc::clone(&account_data.muted.muted);
        Box::new(move |note: &Note, thread: &[u8; 32]| muted.is_muted(note, thread))
    }

    pub fn send_initial_filters(&mut self, pool: &mut RelayPool, relay_url: &str) {
        let data = &self.get_selected_account().data;
        // send the active account's relay list subscription
        pool.send_to(
            &ClientMessage::req(
                self.subs.relay.remote.clone(),
                vec![data.relay.filter.clone()],
            ),
            relay_url,
        );
        // send the active account's muted subscription
        pool.send_to(
            &ClientMessage::req(
                self.subs.mute.remote.clone(),
                vec![data.muted.filter.clone()],
            ),
            relay_url,
        );
    }

    fn poll_for_updates(&mut self, ndb: &Ndb) -> bool {
        let mut changed = false;
        let relay_sub = self.subs.relay.local;
        let mute_sub = self.subs.mute.local;
        let acc = self.get_selected_account_mut();

        let nks = ndb.poll_for_notes(relay_sub, 1);
        if !nks.is_empty() {
            let txn = Transaction::new(ndb).expect("txn");
            let relays = AccountRelayData::harvest_nip65_relays(ndb, &txn, &nks);
            debug!(
                "pubkey {}: updated relays {:?}",
                acc.key.pubkey.hex(),
                relays
            );
            acc.data.relay.advertised = relays.into_iter().collect();
            changed = true;
        }

        let nks = ndb.poll_for_notes(mute_sub, 1);
        if !nks.is_empty() {
            let txn = Transaction::new(ndb).expect("txn");
            let muted = AccountMutedData::harvest_nip51_muted(ndb, &txn, &nks);
            debug!("pubkey {}: updated muted {:?}", acc.key.pubkey.hex(), muted);
            acc.data.muted.muted = Arc::new(muted);
            changed = true;
        }

        changed
    }

    pub fn update(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, ctx: &egui::Context) {
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        // If needed, update the relay configuration
        if self.poll_for_updates(ndb) {
            let acc = self.cache.selected();
            update_relay_configuration(
                pool,
                &self.relay_defaults,
                &acc.key.pubkey,
                &acc.data,
                create_wakeup(ctx),
            );
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    pub fn process_relay_action(
        &mut self,
        ctx: &egui::Context,
        pool: &mut RelayPool,
        action: RelayAction,
    ) {
        let acc = self.cache.selected_mut();
        modify_advertised_relays(&acc.key, action, pool, &self.relay_defaults, &mut acc.data);

        update_relay_configuration(
            pool,
            &self.relay_defaults,
            &acc.key.pubkey,
            &acc.data,
            create_wakeup(ctx),
        );
    }
}

enum AccType<'a> {
    Entry(hashbrown::hash_map::OccupiedEntry<'a, Pubkey, UserAccount>),
    Acc(&'a UserAccount),
}

impl<'a> AccType<'a> {
    fn get_acc(&'a self) -> &'a UserAccount {
        match self {
            AccType::Entry(occupied_entry) => occupied_entry.get(),
            AccType::Acc(user_account) => user_account,
        }
    }
}

fn create_wakeup(ctx: &egui::Context) -> impl Fn() + Send + Sync + Clone + 'static {
    let ctx = ctx.clone();
    move || {
        ctx.request_repaint();
    }
}

fn add_account_from_storage(
    cache: &mut AccountCache,
    ndb: &Ndb,
    txn: &Transaction,
    user_account_serializable: UserAccountSerializable,
) -> SingleUnkIdAction {
    let Some(acc) = get_acc_from_storage(ndb, txn, user_account_serializable) else {
        return SingleUnkIdAction::NoAction;
    };

    let pk = acc.key.pubkey;
    cache.add(acc);

    SingleUnkIdAction::pubkey(pk)
}

fn get_acc_from_storage(
    ndb: &Ndb,
    txn: &Transaction,
    user_account_serializable: UserAccountSerializable,
) -> Option<UserAccount> {
    let keypair = user_account_serializable.key;
    let new_account_data = AccountData {
        relay: AccountRelayData::new(ndb, txn, keypair.pubkey.bytes()),
        muted: AccountMutedData::new(ndb, txn, keypair.pubkey.bytes()),
    };

    let mut wallet = None;
    if let Some(wallet_s) = user_account_serializable.wallet {
        let m_wallet: Result<crate::ZapWallet, crate::Error> = wallet_s.into();
        match m_wallet {
            Ok(w) => wallet = Some(w),
            Err(e) => {
                tracing::error!("Problem creating wallet from disk: {e}");
            }
        };
    }

    Some(UserAccount {
        key: keypair,
        wallet,
        data: new_account_data,
    })
}

pub struct AccountData {
    pub(crate) relay: AccountRelayData,
    pub(crate) muted: AccountMutedData,
}

pub struct AddAccountResponse {
    pub switch_to: Pubkey,
    pub unk_id_action: SingleUnkIdAction,
}

struct AccountSubs {
    relay: UnifiedSubscription,
    mute: UnifiedSubscription,
}

impl AccountSubs {
    pub fn new(
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        data: &AccountData,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Self {
        let relay = subscribe(ndb, pool, &data.relay.filter);
        let mute = subscribe(ndb, pool, &data.muted.filter);
        update_relay_configuration(pool, relay_defaults, pk, data, wakeup);

        Self { relay, mute }
    }

    pub fn swap_to(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        new_selection_data: &AccountData,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        unsubscribe(ndb, pool, &self.relay);
        unsubscribe(ndb, pool, &self.mute);

        *self = AccountSubs::new(ndb, pool, relay_defaults, pk, new_selection_data, wakeup);
    }
}

fn subscribe(ndb: &Ndb, pool: &mut RelayPool, filter: &nostrdb::Filter) -> UnifiedSubscription {
    let filters = vec![filter.clone()];
    let sub = ndb
        .subscribe(&filters)
        .expect("ndb relay list subscription");

    // remote subscription
    let subid = Uuid::new_v4().to_string();
    pool.subscribe(subid.clone(), filters);

    UnifiedSubscription {
        local: sub,
        remote: subid,
    }
}

fn unsubscribe(ndb: &mut Ndb, pool: &mut RelayPool, sub: &UnifiedSubscription) {
    pool.unsubscribe(sub.remote.clone());

    // local subscription
    ndb.unsubscribe(sub.local)
        .expect("ndb relay list unsubscribe");
}
