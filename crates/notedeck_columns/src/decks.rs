use std::collections::{hash_map::ValuesMut, HashMap};

use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::{AppContext, FALLBACK_PUBKEY};
use tracing::{error, info};

use crate::{
    accounts::AccountsRoute,
    column::{Column, Columns},
    route::Route,
    timeline::{TimelineCache, TimelineKind},
    ui::{add_column::AddColumnRoute, configure_deck::ConfigureDeckResponse},
};

pub enum DecksAction {
    Switch(usize),
    Removing(usize),
}

pub struct DecksCache {
    account_to_decks: HashMap<Pubkey, Decks>,
    fallback_pubkey: Pubkey,
}

impl Default for DecksCache {
    fn default() -> Self {
        let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
        account_to_decks.insert(FALLBACK_PUBKEY(), Decks::default());
        DecksCache::new(account_to_decks)
    }
}

impl DecksCache {
    /// Gets the first column in the currently active user's active deck
    pub fn selected_column_mut(&mut self, accounts: &notedeck::Accounts) -> Option<&mut Column> {
        self.active_columns_mut(accounts).map(|ad| ad.selected())
    }

    /// Gets the active columns
    pub fn active_columns_mut(&mut self, accounts: &notedeck::Accounts) -> Option<&mut Columns> {
        let account = accounts.get_selected_account()?;

        self.decks_mut(&account.key.pubkey)
            .active_deck_mut()
            .map(|ad| ad.columns_mut())
    }

    pub fn new(mut account_to_decks: HashMap<Pubkey, Decks>) -> Self {
        let fallback_pubkey = FALLBACK_PUBKEY();
        account_to_decks.entry(fallback_pubkey).or_default();

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
        DecksCache::new(account_to_decks)
    }

    pub fn decks(&self, key: &Pubkey) -> &Decks {
        self.account_to_decks
            .get(key)
            .unwrap_or_else(|| self.fallback())
    }

    pub fn decks_mut(&mut self, key: &Pubkey) -> &mut Decks {
        self.account_to_decks.entry(*key).or_default()
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

    pub fn add_deck_default(&mut self, key: Pubkey) {
        self.account_to_decks.insert(key, Decks::default());
        info!(
            "Adding new default deck for {:?}. New decks size is {}",
            key,
            self.account_to_decks.get(&key).unwrap().decks.len()
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

    pub fn remove_for(&mut self, key: &Pubkey) {
        info!("Removing decks for {:?}", key);
        self.account_to_decks.remove(key);
    }

    pub fn get_fallback_pubkey(&self) -> &Pubkey {
        &self.fallback_pubkey
    }

    pub fn get_all_decks_mut(&mut self) -> ValuesMut<Pubkey, Decks> {
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

impl Default for Decks {
    fn default() -> Self {
        Decks::new(Deck::default())
    }
}

impl Decks {
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

    pub fn active_deck_mut(&mut self) -> Option<&mut Deck> {
        if self.decks.is_empty() {
            return None;
        }

        let active = self.active_index();
        if active > (self.decks.len() - 1) {
            return None;
        }

        Some(&mut self.decks[active])
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

    pub fn remove_deck(&mut self, index: usize) {
        if index < self.decks.len() {
            if self.decks.len() > 1 {
                self.decks.remove(index);

                let info_prefix = format!("Removed deck at index {}", index);
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
    }
}

pub struct Deck {
    pub icon: char,
    pub name: String,
    columns: Columns,
}

impl Default for Deck {
    fn default() -> Self {
        let mut columns = Columns::default();
        columns.new_column_picker();
        Self {
            icon: '🇩',
            name: String::from("Default Deck"),
            columns,
        }
    }
}

impl Deck {
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

pub fn demo_decks(
    demo_pubkey: Pubkey,
    timeline_cache: &mut TimelineCache,
    ctx: &mut AppContext,
) -> Decks {
    let deck = {
        let mut columns = Columns::default();
        columns.add_column(Column::new(vec![
            Route::AddColumn(AddColumnRoute::Base),
            Route::Accounts(AccountsRoute::Accounts),
        ]));

        let kind = TimelineKind::contact_list(demo_pubkey);
        let txn = Transaction::new(ctx.ndb).unwrap();

        if let Some(results) = columns.add_new_timeline_column(
            timeline_cache,
            &txn,
            ctx.ndb,
            ctx.note_cache,
            ctx.pool,
            &kind,
        ) {
            results.process(
                ctx.ndb,
                ctx.note_cache,
                &txn,
                timeline_cache,
                ctx.unknown_ids,
            );
        }

        //columns.add_new_timeline_column(Timeline::hashtag("introductions".to_string()));

        Deck {
            icon: '🇩',
            name: String::from("Demo Deck"),
            columns,
        }
    };

    Decks::new(deck)
}
