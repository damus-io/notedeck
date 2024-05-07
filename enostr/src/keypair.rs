use crate::Pubkey;
use crate::SecretKey;

#[derive(Debug, Eq, PartialEq)]
pub struct Keypair {
    pub pubkey: Pubkey,
    pub secret_key: Option<SecretKey>,
}

impl Keypair {
    pub fn new(secret_key: SecretKey) -> Self {
        let cloned_secret_key = secret_key.clone();
        let nostr_keys = nostr::Keys::new(secret_key);
        Keypair {
            pubkey: Pubkey::new(&nostr_keys.public_key().to_bytes()),
            secret_key: Some(cloned_secret_key),
        }
    }

    pub fn only_pubkey(pubkey: Pubkey) -> Self {
        Keypair {
            pubkey,
            secret_key: None,
        }
    }

    pub fn to_full(self) -> Option<FullKeypair> {
        if let Some(secret_key) = self.secret_key {
            Some(FullKeypair {
                pubkey: self.pubkey,
                secret_key,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct FullKeypair {
    pub pubkey: Pubkey,
    pub secret_key: SecretKey,
}

impl FullKeypair {
    pub fn new(pubkey: Pubkey, secret_key: SecretKey) -> Self {
        FullKeypair { pubkey, secret_key }
    }
}

impl std::fmt::Display for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Keypair:\n\tpublic: {}\n\tsecret: {}",
            self.pubkey,
            match self.secret_key {
                Some(_) => "Some(<hidden>)",
                None => "None",
            }
        )
    }
}

impl std::fmt::Display for FullKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Keypair:\n\tpublic: {}\n\tsecret: {}",
            self.pubkey, "<hidden>"
        )
    }
}
