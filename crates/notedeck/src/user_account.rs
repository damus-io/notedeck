use enostr::Keypair;

pub struct UserAccount {
    pub key: Keypair,
}

impl UserAccount {
    pub fn new(key: Keypair) -> Self {
        Self { key }
    }
}
