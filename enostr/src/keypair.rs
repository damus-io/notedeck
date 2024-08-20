use nostr::nips::nip49::EncryptedSecretKey;
use serde::Deserialize;
use serde::Serialize;

use crate::Pubkey;
use crate::SecretKey;

#[derive(Debug, Eq, PartialEq)]
pub struct Keypair {
    pub pubkey: Pubkey,
    pub secret_key: Option<SecretKey>,
}

impl Keypair {
    pub fn from_secret(secret_key: SecretKey) -> Self {
        let cloned_secret_key = secret_key.clone();
        let nostr_keys = nostr::Keys::new(secret_key);
        Keypair {
            pubkey: Pubkey::new(&nostr_keys.public_key().to_bytes()),
            secret_key: Some(cloned_secret_key),
        }
    }

    pub fn new(pubkey: Pubkey, secret_key: Option<SecretKey>) -> Self {
        Keypair { pubkey, secret_key }
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

    pub fn generate() -> Self {
        let mut rng = nostr::secp256k1::rand::rngs::OsRng;
        let (secret_key, _) = &nostr::SECP256K1.generate_keypair(&mut rng);
        let (xopk, _) = secret_key.x_only_public_key(&nostr::SECP256K1);
        let secret_key = nostr::SecretKey::from(*secret_key);
        FullKeypair {
            pubkey: Pubkey::new(&xopk.serialize()),
            secret_key,
        }
    }

    pub fn to_keypair(self) -> Keypair {
        Keypair {
            pubkey: self.pubkey,
            secret_key: Some(self.secret_key),
        }
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
        write!(f, "Keypair:\n\tpublic: {}\n\tsecret: <hidden>", self.pubkey)
    }
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SerializableKeypair {
    pub pubkey: Pubkey,
    pub encrypted_secret_key: Option<EncryptedSecretKey>,
}

impl SerializableKeypair {
    pub fn from_keypair(kp: &Keypair, pass: &str, log_n: u8) -> Self {
        Self {
            pubkey: kp.pubkey.clone(),
            encrypted_secret_key: kp.secret_key.clone().and_then(|s| {
                EncryptedSecretKey::new(&s, pass, log_n, nostr::nips::nip49::KeySecurity::Weak).ok()
            }),
        }
    }

    pub fn to_keypair(&self, pass: &str) -> Keypair {
        Keypair::new(
            self.pubkey.clone(),
            self.encrypted_secret_key
                .and_then(|e| e.to_secret_key(pass).ok()),
        )
    }
}
