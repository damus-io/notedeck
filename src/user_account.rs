use enostr::{FullKeypair, RelayPool};

pub struct UserAccount {
    pub key: FullKeypair,
    pub relays: RelayPool,
}
