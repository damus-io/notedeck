use std::collections::HashMap;

use enostr::Pubkey;
use tracing::info;

use crate::column::Columns;

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub enum AccountId {
    User(Pubkey),
    Unnamed(u32),
}

pub struct Decks {
    fallback: Columns,
    account_to_columns: HashMap<AccountId, Columns>, // TODO: account_to_decks: HashMap<AccountId, Vec<Deck>>
}

/**
* struct Deck {
*   icon: ??,
*   name: String,
*   columns: Columns,
* }
**/

impl Default for Decks {
    fn default() -> Self {
        let fallback = default_columns();

        Self {
            fallback,
            account_to_columns: Default::default(),
        }
    }
}

impl Decks {
    pub fn get_active_columns_for(&self, account_id: &AccountId) -> &Columns {
        if let Some(cols) = self.account_to_columns.get(account_id) {
            cols
        } else {
            info!(
                "Did not have deck for {:?}, using fallback instead",
                account_id
            );
            &self.fallback
        }
    }

    pub fn get_active_columns_for_mut(&mut self, account_id: &AccountId) -> &mut Columns {
        if let Some(cols) = self.account_to_columns.get_mut(account_id) {
            cols
        } else {
            info!(
                "Did not have deck for {:?}, using fallback instead",
                account_id
            );
            &mut self.fallback
        }
    }

    pub fn get_default_columns(&self) -> &Columns {
        &self.fallback
    }

    pub fn get_default_columns_mut(&mut self) -> &mut Columns {
        &mut self.fallback
    }

    pub fn add_deck(&mut self, account: AccountId, columns: Columns) {
        info!("Adding new deck for {:?}", account);
        self.account_to_columns.insert(account, columns);
    }

    pub fn add_deck_default(&mut self, account: AccountId) {
        info!("Adding new deck for {:?}", account);
        self.account_to_columns.insert(account, default_columns());
    }

    pub fn remove_for(&mut self, account: AccountId) {
        info!("Removing decks for {:?}", account);
        self.account_to_columns.remove(&account);
    }
}

fn default_columns() -> Columns {
    let mut cols = Columns::default();
    cols.new_column_picker();
    cols
}
