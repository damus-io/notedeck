use std::collections::{hash_map::ValuesMut, HashMap};

use enostr::{Pubkey, RelayPool};
use nostrdb::Transaction;
use notedeck::{tr, AppContext, Localization, FALLBACK_PUBKEY};
use tracing::{error, info};

use crate::{
    column::{Column, Columns},
    timeline::{TimelineCache, TimelineKind},
    ui::configure_deck::ConfigureDeckResponse,
};

pub enum DecksAction {
    Switch(usize),
    Removing(usize),
}

pub struct DecksCache {
    account_to_decks: HashMap<Pubkey, Decks>,
    fallback_pubkey: Pubkey,
}

impl DecksCache {
    pub fn default_decks_cache(i18n: &mut Localization) -> Self {
        let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
        account_to_decks.insert(FALLBACK_PUBKEY(), Decks::default_decks(i18n));
        DecksCache::new(account_to_decks, i18n)
    }

    /// Gets the first column in the currently active user's active deck
    pub fn selected_column_mut(
        &mut self,
        i18n: &mut Localization,
        accounts: &notedeck::Accounts,
    ) -> Option<&mut Column> {
        self.active_columns_mut(i18n, accounts)
            .map(|ad| ad.selected_mut())
    }

    pub fn selected_column(&self, accounts: &notedeck::Accounts) -> Option<&Column> {
        self.active_columns(accounts).and_then(|ad| ad.selected())
    }

    pub fn selected_column_index(&self, accounts: &notedeck::Accounts) -> Option<usize> {
        self.active_columns(accounts).map(|ad| ad.selected as usize)
    }

    /// Gets a mutable reference to the active columns
    pub fn active_columns_mut(
        &mut self,
        i18n: &mut Localization,
        accounts: &notedeck::Accounts,
    ) -> Option<&mut Columns> {
        let account = accounts.get_selected_account();

        self.decks_mut(i18n, &account.key.pubkey)
            .active_deck_mut()
            .map(|ad| ad.columns_mut())
    }

    /// Gets an immutable reference to the active columns
    pub fn active_columns(&self, accounts: &notedeck::Accounts) -> Option<&Columns> {
        let account = accounts.get_selected_account();

        self.decks(&account.key.pubkey)
            .active_deck()
            .map(|ad| ad.columns())
    }

    pub fn new(mut account_to_decks: HashMap<Pubkey, Decks>, i18n: &mut Localization) -> Self {
        let fallback_pubkey = FALLBACK_PUBKEY();
        account_to_decks
            .entry(fallback_pubkey)
            .or_insert_with(|| Decks::default_decks(i18n));

        Self {
            account_to_decks,
            fallback_pubkey,
        }
    }

    pub fn new_with_demo_config(timeline_cache: &mut TimelineCache, ctx: &mut AppContext) -> Self {
        let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
        let fallback_pubkey = FALLBACK_PUBKEY();
        account_to_decks.insert(
            fallback_pubkey,
            demo_decks(fallback_pubkey, timeline_cache, ctx),
        );
        DecksCache::new(account_to_decks, ctx.i18n)
    }

    pub fn decks(&self, key: &Pubkey) -> &Decks {
        self.account_to_decks
            .get(key)
            .unwrap_or_else(|| self.fallback())
    }

    pub fn decks_mut(&mut self, i18n: &mut Localization, key: &Pubkey) -> &mut Decks {
        self.account_to_decks
            .entry(*key)
            .or_insert_with(|| Decks::default_decks(i18n))
    }

    pub fn fallback(&self) -> &Decks {
        self.account_to_decks
            .get(&self.fallback_pubkey)
            .unwrap_or_else(|| panic!("fallback deck not found"))
    }

    pub fn fallback_mut(&mut self) -> &mut Decks {
        self.account_to_decks
            .get_mut(&self.fallback_pubkey)
            .unwrap_or_else(|| panic!("fallback deck not found"))
    }

    pub fn add_deck_default(
        &mut self,
        ctx: &mut AppContext,
        timeline_cache: &mut TimelineCache,
        pubkey: Pubkey,
    ) {
        let mut decks = Decks::default_decks(ctx.i18n);

        // add home and notifications for new accounts
        add_demo_columns(
            ctx,
            timeline_cache,
            pubkey,
            &mut decks.decks_mut()[0].columns,
        );

        self.account_to_decks.insert(pubkey, decks);
        info!(
            "Adding new default deck for {:?}. New decks size is {}",
            pubkey,
            self.account_to_decks.get(&pubkey).unwrap().decks.len()
        );
    }

    pub fn add_decks(&mut self, key: Pubkey, decks: Decks) {
        self.account_to_decks.insert(key, decks);
        info!(
            "Adding new deck for {:?}. New decks size is {}",
            key,
            self.account_to_decks.get(&key).unwrap().decks.len()
        );
    }

    pub fn add_deck(&mut self, key: Pubkey, deck: Deck) {
        match self.account_to_decks.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let decks = entry.get_mut();
                decks.add_deck(deck);
                info!(
                    "Created new deck for {:?}. New number of decks is {}",
                    key,
                    decks.decks.len()
                );
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                info!("Created first deck for {:?}", key);
                entry.insert(Decks::new(deck));
            }
        }
    }

    pub fn remove(
        &mut self,
        i18n: &mut Localization,
        key: &Pubkey,
        timeline_cache: &mut TimelineCache,
        ndb: &mut nostrdb::Ndb,
        pool: &mut RelayPool,
    ) {
        let Some(decks) = self.account_to_decks.remove(key) else {
            return;
        };
        info!("Removing decks for {:?}", key);

        decks.unsubscribe_all(timeline_cache, ndb, *key, pool);

        if !self.account_to_decks.contains_key(&self.fallback_pubkey) {
            self.account_to_decks
                .insert(self.fallback_pubkey, Decks::default_decks(i18n));
        }
    }

    pub fn get_fallback_pubkey(&self) -> &Pubkey {
        &self.fallback_pubkey
    }

    pub fn get_all_decks_mut(&mut self) -> ValuesMut<'_, Pubkey, Decks> {
        self.account_to_decks.values_mut()
    }

    pub fn get_mapping(&self) -> &HashMap<Pubkey, Decks> {
        &self.account_to_decks
    }
}

pub struct Decks {
    active_deck: usize,
    removal_request: Option<usize>,
    decks: Vec<Deck>,
}

impl Decks {
    pub fn default_decks(i18n: &mut Localization) -> Self {
        Decks::new(Deck::default_deck(i18n))
    }

    pub fn new(deck: Deck) -> Self {
        let decks = vec![deck];

        Decks {
            active_deck: 0,
            removal_request: None,
            decks,
        }
    }

    pub fn from_decks(active_deck: usize, decks: Vec<Deck>) -> Self {
        Self {
            active_deck,
            removal_request: None,
            decks,
        }
    }

    pub fn active(&self) -> &Deck {
        self.decks
            .get(self.active_deck)
            .expect("active_deck index was invalid")
    }

    pub fn active_mut(&mut self) -> &mut Deck {
        self.decks
            .get_mut(self.active_deck)
            .expect("active_deck index was invalid")
    }

    pub fn decks(&self) -> &Vec<Deck> {
        &self.decks
    }

    fn active_deck_index(&self) -> Option<usize> {
        if self.decks.is_empty() {
            return None;
        }

        let active = self.active_index();
        if active > (self.decks.len() - 1) {
            return None;
        }

        Some(active)
    }

    pub fn active_deck(&self) -> Option<&Deck> {
        self.active_deck_index().map(|ind| &self.decks[ind])
    }

    pub fn active_deck_mut(&mut self) -> Option<&mut Deck> {
        self.active_deck_index().map(|ind| &mut self.decks[ind])
    }

    pub fn decks_mut(&mut self) -> &mut Vec<Deck> {
        &mut self.decks
    }

    pub fn add_deck(&mut self, deck: Deck) {
        self.decks.push(deck);
    }

    pub fn active_index(&self) -> usize {
        self.active_deck
    }

    pub fn set_active(&mut self, index: usize) {
        if index < self.decks.len() {
            self.active_deck = index;
        } else {
            error!(
                "requested deck change that is invalid. decks len: {}, requested index: {}",
                self.decks.len(),
                index
            );
        }
    }

    pub fn remove_deck(
        &mut self,
        index: usize,
        timeline_cache: &mut TimelineCache,
        ndb: &mut nostrdb::Ndb,
        account_pk: Pubkey,
        pool: &mut enostr::RelayPool,
    ) {
        let Some(deck) = self.remove_deck_internal(index) else {
            return;
        };

        delete_deck(deck, timeline_cache, ndb, account_pk, pool);
    }

    fn remove_deck_internal(&mut self, index: usize) -> Option<Deck> {
        let mut res = None;
        if index < self.decks.len() {
            if self.decks.len() > 1 {
                res = Some(self.decks.remove(index));

                let info_prefix = format!("Removed deck at index {index}");
                match index.cmp(&self.active_deck) {
                    std::cmp::Ordering::Less => {
                        info!(
                            "{}. The active deck was index {}, now it is {}",
                            info_prefix,
                            self.active_deck,
                            self.active_deck - 1
                        );
                        self.active_deck -= 1
                    }
                    std::cmp::Ordering::Greater => {
                        info!(
                            "{}. Active deck remains at index {}.",
                            info_prefix, self.active_deck
                        )
                    }
                    std::cmp::Ordering::Equal => {
                        if index != 0 {
                            info!(
                                "{}. Active deck was index {}, now it is {}",
                                info_prefix,
                                self.active_deck,
                                self.active_deck - 1
                            );
                            self.active_deck -= 1;
                        } else {
                            info!(
                                "{}. Active deck remains at index {}.",
                                info_prefix, self.active_deck
                            )
                        }
                    }
                }
                self.removal_request = None;
            } else {
                error!("attempted unsucessfully to remove the last deck for this account");
            }
        } else {
            error!("index was out of bounds");
        }
        res
    }

    pub fn unsubscribe_all(
        self,
        timeline_cache: &mut TimelineCache,
        ndb: &mut nostrdb::Ndb,
        account_pk: Pubkey,
        pool: &mut enostr::RelayPool,
    ) {
        for deck in self.decks {
            delete_deck(deck, timeline_cache, ndb, account_pk, pool);
        }
    }
}

fn delete_deck(
    mut deck: Deck,
    timeline_cache: &mut TimelineCache,
    ndb: &mut nostrdb::Ndb,
    account_pk: Pubkey,
    pool: &mut enostr::RelayPool,
) {
    let cols = deck.columns_mut();
    let num_cols = cols.num_columns();
    for i in (0..num_cols).rev() {
        let kinds_to_pop = cols.delete_column(i);

        for kind in &kinds_to_pop {
            if let Err(err) = timeline_cache.pop(kind, account_pk, ndb, pool) {
                error!("error popping timeline: {err}");
            }
        }
    }
}

pub struct Deck {
    pub icon: char,
    pub name: String,
    columns: Columns,
}

impl Deck {
    pub fn default_icon() -> char {
        '🇩'
    }

    fn default_deck(i18n: &mut Localization) -> Self {
        let columns = Columns::default();
        Self {
            columns,
            icon: Deck::default_icon(),
            name: Deck::default_name(i18n).to_string(),
        }
    }

    pub fn default_name(i18n: &mut Localization) -> String {
        tr!(i18n, "Default Deck", "Name of the default deck feed")
    }

    pub fn new(icon: char, name: String) -> Self {
        let mut columns = Columns::default();

        columns.new_column_picker();

        Self {
            icon,
            name,
            columns,
        }
    }

    pub fn new_with_columns(icon: char, name: String, columns: Columns) -> Self {
        Self {
            icon,
            name,
            columns,
        }
    }

    pub fn columns(&self) -> &Columns {
        &self.columns
    }

    pub fn columns_mut(&mut self) -> &mut Columns {
        &mut self.columns
    }

    pub fn edit(&mut self, changes: ConfigureDeckResponse) {
        self.name = changes.name;
        self.icon = changes.icon;
    }
}

pub fn add_demo_columns(
    ctx: &mut AppContext,
    timeline_cache: &mut TimelineCache,
    pubkey: Pubkey,
    columns: &mut Columns,
) {
    let timeline_kinds = [
        TimelineKind::contact_list(pubkey),
        TimelineKind::notifications(pubkey),
    ];

    let txn = Transaction::new(ctx.ndb).unwrap();

    for kind in &timeline_kinds {
        if let Some(results) = columns.add_new_timeline_column(
            timeline_cache,
            &txn,
            ctx.ndb,
            ctx.note_cache,
            *ctx.accounts.selected_account_pubkey(),
            ctx.legacy_pool,
            kind,
        ) {
            results.process(
                ctx.ndb,
                ctx.note_cache,
                &txn,
                timeline_cache,
                ctx.unknown_ids,
            );
        }
    }
}

pub fn demo_decks(
    demo_pubkey: Pubkey,
    timeline_cache: &mut TimelineCache,
    ctx: &mut AppContext,
) -> Decks {
    let deck = {
        let mut columns = Columns::default();

        add_demo_columns(ctx, timeline_cache, demo_pubkey, &mut columns);

        //columns.add_new_timeline_column(Timeline::hashtag("introductions".to_string()));

        Deck {
            icon: Deck::default_icon(),
            name: Deck::default_name(ctx.i18n).to_string(),
            columns,
        }
    };

    Decks::new(deck)
}
