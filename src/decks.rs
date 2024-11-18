use std::collections::HashMap;

use enostr::Pubkey;
use tracing::{error, info};

use crate::{column::Columns, ui::configure_deck::ConfigureDeckResponse};

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub enum AccountId {
    User(Pubkey),
    Unnamed(u32),
}

pub struct DecksCache {
    pub account_to_decks: HashMap<AccountId, Decks>,
}

impl Default for DecksCache {
    fn default() -> Self {
        let mut account_to_decks: HashMap<AccountId, Decks> = Default::default();
        account_to_decks.insert(AccountId::Unnamed(0), Decks::default());
        Self { account_to_decks }
    }
}

impl DecksCache {
    pub fn decks(&self, account_id: &AccountId) -> &Decks {
        self.account_to_decks
            .get(account_id)
            .unwrap_or_else(|| panic!("{:?} not found", account_id))
    }

    pub fn decks_mut(&mut self, account_id: &AccountId) -> &mut Decks {
        self.account_to_decks
            .get_mut(account_id)
            .unwrap_or_else(|| panic!("{:?} not found", account_id))
    }

    pub fn add_deck_default(&mut self, account: AccountId) {
        self.account_to_decks
            .insert(account.clone(), Decks::default());
        info!(
            "Adding new default deck for {:?}. New decks size is {}",
            account,
            self.account_to_decks.get(&account).unwrap().decks.len()
        );
    }

    pub fn add_decks(&mut self, account: AccountId, decks: Decks) {
        self.account_to_decks.insert(account.clone(), decks);
        info!(
            "Adding new deck for {:?}. New decks size is {}",
            account,
            self.account_to_decks.get(&account).unwrap().decks.len()
        );
    }

    pub fn add_deck(&mut self, account: AccountId, deck: Deck) {
        match self.account_to_decks.entry(account.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let decks = entry.get_mut();
                decks.add_deck(deck);
                info!(
                    "Created new deck for {:?}. New number of decks is {}",
                    account,
                    decks.decks.len()
                );
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                info!("Created first deck for {:?}", account);
                entry.insert(Decks::new(deck));
            }
        }
    }

    pub fn remove_for(&mut self, account: &AccountId) {
        info!("Removing decks for {:?}", account);
        self.account_to_decks.remove(account);
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
