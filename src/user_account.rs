use enostr::RelayPool;

pub struct UserAccount {
    pub key: nostr_sdk::Keys,
    pub relays: RelayPool,
}
