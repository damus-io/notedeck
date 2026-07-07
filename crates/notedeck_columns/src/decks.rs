use std::collections::{hash_map::ValuesMut, HashMap};

use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::{tr, AppContext, Localization, FALLBACK_PUBKEY};
use tracing::{error, info};

use crate::{
    column::{Column, ColumnId, Columns},
    route::Route,
    timeline::{RemoteSubscriptionPolicy, TimelineCache, TimelineKind},
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

#[must_use = "removed deck routes must be disposed"]
pub(crate) struct DecksRemoval {
    pub account_pk: Pubkey,
    pub decks: Vec<Deck>,
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
            decks.active_mut().columns_mut(),
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

    pub(crate) fn remove(&mut self, i18n: &mut Localization, key: &Pubkey) -> Option<DecksRemoval> {
        let decks = self.account_to_decks.remove(key)?;
        info!("Removing decks for {:?}", key);

        let removal = DecksRemoval {
            account_pk: *key,
            decks: decks.decks,
        };

        if !self.account_to_decks.contains_key(&self.fallback_pubkey) {
            self.account_to_decks
                .insert(self.fallback_pubkey, Decks::default_decks(i18n));
        }

        Some(removal)
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

    /// Remove the top route from the column owned by `account_pk` and `column_id`.
    pub(crate) fn remove_top_route_for_column(
        &mut self,
        account_pk: &Pubkey,
        column_id: ColumnId,
    ) -> Option<Route> {
        self.account_to_decks
            .get_mut(account_pk)
            .and_then(|decks| decks.remove_top_route_for_column(column_id))
    }
}

pub struct Decks {
    active_deck: usize,
    removal_request: Option<usize>,
    decks: Vec<Deck>,
    next_deck_id: u64,
}

impl Decks {
    pub fn default_decks(i18n: &mut Localization) -> Self {
        Decks::new(Deck::default_deck(i18n))
    }

    pub fn new(deck: Deck) -> Self {
        let mut next_deck_id = 1;
        let mut decks = vec![deck];
        assign_deck_ids(&mut decks, &mut next_deck_id);

        Decks {
            active_deck: 0,
            removal_request: None,
            decks,
            next_deck_id,
        }
    }

    pub fn from_decks(active_deck: usize, decks: Vec<Deck>) -> Self {
        let mut next_deck_id = 1;
        let mut decks = decks;
        assign_deck_ids(&mut decks, &mut next_deck_id);

        Self {
            active_deck,
            removal_request: None,
            decks,
            next_deck_id,
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

    pub fn decks(&self) -> &[Deck] {
        &self.decks
    }

    pub fn decks_mut(&mut self) -> &mut Vec<Deck> {
        &mut self.decks
    }

    #[cfg(test)]
    pub fn deck_mut(&mut self, index: usize) -> Option<&mut Deck> {
        self.decks.get_mut(index)
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

    pub fn add_deck(&mut self, deck: Deck) {
        let mut deck = deck;
        assign_deck_id(&mut deck, &mut self.next_deck_id);
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

    #[must_use = "removed deck routes must be disposed"]
    pub(crate) fn remove_deck(&mut self, index: usize) -> Option<Deck> {
        self.remove_deck_internal(index)
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

    /// Remove the top route from the deck column with `column_id`.
    pub(crate) fn remove_top_route_for_column(&mut self, column_id: ColumnId) -> Option<Route> {
        for deck in &mut self.decks {
            if let Some((_index, column)) = deck.columns_mut().column_mut_by_id(column_id) {
                return column.router_mut().remove_top_route_for_disposal();
            }
        }

        None
    }
}

fn assign_deck_ids(decks: &mut [Deck], next_deck_id: &mut u64) {
    for deck in decks {
        assign_deck_id(deck, next_deck_id);
    }
}

fn assign_deck_id(deck: &mut Deck, next_deck_id: &mut u64) {
    deck.columns_mut().assign_deck_id(*next_deck_id);
    *next_deck_id = next_deck_id.saturating_add(1);
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
        let mut scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
        if let Some(results) = columns.add_new_timeline_column(
            timeline_cache,
            &txn,
            ctx.ndb,
            ctx.note_cache,
            &mut scoped_subs,
            kind,
            pubkey,
            RemoteSubscriptionPolicy::from_outbox_relays(ctx.settings.columns_use_outbox_relays()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::ColumnId;
    use crate::route::Route;
    use crate::ui::add_column::AddColumnRoute;

    fn deck_with_column() -> Deck {
        let mut columns = Columns::new();
        columns.add_column(Column::new(vec![Route::AddColumn(AddColumnRoute::Base)]));

        Deck::new_with_columns(Deck::default_icon(), "Deck".to_owned(), columns)
    }

    #[test]
    fn decks_allocate_unique_column_ids_across_decks() {
        let first = deck_with_column();
        let second = deck_with_column();
        let second_initial_id = second.columns().column(0).id();
        let mut decks = Decks::new(first);

        decks.add_deck(second);
        decks.active_mut().columns_mut().new_column_picker();

        let first_id = decks.decks()[0].columns().column(0).id();
        let second_assigned_id = decks.decks()[1].columns().column(0).id();
        let first_added_id = decks.decks()[0].columns().column(1).id();
        assert_eq!(first_id, ColumnId::for_test_in_deck(1, 1));
        assert_eq!(second_initial_id, ColumnId::for_test_in_deck(0, 1));
        assert_eq!(second_assigned_id, ColumnId::for_test_in_deck(2, 1));
        assert_eq!(first_added_id, ColumnId::for_test_in_deck(1, 2));
        assert_ne!(first_id, second_assigned_id);
        assert_ne!(first_added_id, second_assigned_id);
    }

    #[test]
    fn adding_cloned_column_reassigns_id_to_target_deck() {
        let first = deck_with_column();
        let cloned_column = first.columns().column(0).clone();
        let mut decks = Decks::new(first);
        decks.add_deck(deck_with_column());

        decks
            .deck_mut(1)
            .expect("second deck")
            .columns_mut()
            .add_column(cloned_column);

        assert_eq!(
            decks.decks()[1].columns().column(1).id(),
            ColumnId::for_test_in_deck(2, 2)
        );
    }
}
