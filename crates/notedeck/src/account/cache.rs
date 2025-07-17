use enostr::Pubkey;
use hashbrown::{hash_map::OccupiedEntry, HashMap};

use crate::{SingleUnkIdAction, UserAccount};

pub struct AccountCache {
    selected: Pubkey,
    fallback: Pubkey,
    fallback_account: UserAccount,

    // never empty at rest
    accounts: HashMap<Pubkey, UserAccount>,
}

impl AccountCache {
    pub(super) fn new(fallback: UserAccount) -> (Self, SingleUnkIdAction) {
        let mut accounts = HashMap::with_capacity(1);

        let pk = fallback.key.pubkey;
        accounts.insert(pk, fallback.clone());

        (
            Self {
                selected: pk,
                fallback: pk,
                fallback_account: fallback,
                accounts,
            },
            SingleUnkIdAction::pubkey(pk),
        )
    }

    pub fn get(&self, pk: &Pubkey) -> Option<&UserAccount> {
        self.accounts.get(pk)
    }

    pub fn get_bytes(&self, pk: &[u8; 32]) -> Option<&UserAccount> {
        self.accounts.get(pk)
    }

    pub(super) fn get_mut(&mut self, pk: &Pubkey) -> Option<&mut UserAccount> {
        self.accounts.get_mut(pk)
    }

    pub(super) fn add<'a>(
        &'a mut self,
        account: UserAccount,
    ) -> OccupiedEntry<'a, Pubkey, UserAccount> {
        let pk = account.key.pubkey;
        self.accounts.entry(pk).insert(account)
    }

    pub(super) fn remove(&mut self, pk: &Pubkey) -> Option<UserAccount> {
        if *pk == self.fallback && self.accounts.len() == 1 {
            // no point in removing it since it'll just get re-added anyway
            return None;
        }

        let removed = self.accounts.remove(pk);

        if self.accounts.is_empty() {
            self.accounts
                .insert(self.fallback, self.fallback_account.clone());
        }

        if removed.is_some() && self.selected == *pk {
            // TODO(kernelkind): choose next better
            let (next, _) = self
                .accounts
                .iter()
                .next()
                .expect("accounts can never be empty");
            self.selected = *next;
        }

        removed
    }

    /// guarenteed that all selected exist in accounts
    pub(super) fn select(&mut self, pk: Pubkey) -> bool {
        if !self.accounts.contains_key(&pk) {
            return false;
        }

        self.selected = pk;
        true
    }

    pub fn selected(&self) -> &UserAccount {
        self.accounts
            .get(&self.selected)
            .expect("guarenteed that selected exists in accounts")
    }

    pub(super) fn selected_mut(&mut self) -> &mut UserAccount {
        self.accounts
            .get_mut(&self.selected)
            .expect("guarenteed that selected exists in accounts")
    }

    pub fn fallback(&self) -> &Pubkey {
        &self.fallback
    }
}

impl<'a> IntoIterator for &'a AccountCache {
    type Item = (&'a Pubkey, &'a UserAccount);
    type IntoIter = hashbrown::hash_map::Iter<'a, Pubkey, UserAccount>;

    fn into_iter(self) -> Self::IntoIter {
        self.accounts.iter()
    }
}
