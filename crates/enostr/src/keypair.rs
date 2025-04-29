use nostr::nips::nip19::FromBech32;
use nostr::nips::nip19::ToBech32;
use nostr::nips::nip49::EncryptedSecretKey;
use serde::Deserialize;
use serde::Serialize;
use tokenator::ParseError;
use tokenator::TokenParser;
use tokenator::TokenSerializable;

use crate::Pubkey;
use crate::SecretKey;

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Keypair {
    pub pubkey: Pubkey,
    pub secret_key: Option<SecretKey>,
}

pub struct KeypairUnowned<'a> {
    pub pubkey: &'a Pubkey,
    pub secret_key: Option<&'a SecretKey>,
}

impl<'a> From<&'a Keypair> for KeypairUnowned<'a> {
    fn from(value: &'a Keypair) -> Self {
        Self {
            pubkey: &value.pubkey,
            secret_key: value.secret_key.as_ref(),
        }
    }
}

impl Keypair {
    pub fn from_secret(secret_key: SecretKey) -> Self {
        let cloned_secret_key = secret_key.clone();
        let nostr_keys = nostr::Keys::new(secret_key);
        Keypair {
            pubkey: Pubkey::new(nostr_keys.public_key().to_bytes()),
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

    pub fn to_full(&self) -> Option<FilledKeypair<'_>> {
        self.secret_key.as_ref().map(|secret_key| FilledKeypair {
            pubkey: &self.pubkey,
            secret_key,
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct FullKeypair {
    pub pubkey: Pubkey,
    pub secret_key: SecretKey,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct FilledKeypair<'a> {
    pub pubkey: &'a Pubkey,
    pub secret_key: &'a SecretKey,
}

impl<'a> FilledKeypair<'a> {
    pub fn new(pubkey: &'a Pubkey, secret_key: &'a SecretKey) -> Self {
        FilledKeypair { pubkey, secret_key }
    }

    pub fn to_full(&self) -> FullKeypair {
        FullKeypair {
            pubkey: self.pubkey.to_owned(),
            secret_key: self.secret_key.to_owned(),
        }
    }
}

impl<'a> From<&'a FilledKeypair<'a>> for KeypairUnowned<'a> {
    fn from(value: &'a FilledKeypair<'a>) -> Self {
        Self {
            pubkey: value.pubkey,
            secret_key: Some(value.secret_key),
        }
    }
}

impl FullKeypair {
    pub fn new(pubkey: Pubkey, secret_key: SecretKey) -> Self {
        FullKeypair { pubkey, secret_key }
    }

    pub fn to_filled(&self) -> FilledKeypair<'_> {
        FilledKeypair::new(&self.pubkey, &self.secret_key)
    }

    pub fn generate() -> Self {
        let mut rng = nostr::secp256k1::rand::rngs::OsRng;
        let (secret_key, _) = &nostr::SECP256K1.generate_keypair(&mut rng);
        let (xopk, _) = secret_key.x_only_public_key(nostr::SECP256K1);
        let secret_key = nostr::SecretKey::from(*secret_key);
        FullKeypair {
            pubkey: Pubkey::new(xopk.serialize()),
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
            pubkey: kp.pubkey,
            encrypted_secret_key: kp.secret_key.clone().and_then(|s| {
                EncryptedSecretKey::new(&s, pass, log_n, nostr::nips::nip49::KeySecurity::Weak).ok()
            }),
        }
    }

    pub fn to_keypair(&self, pass: &str) -> Keypair {
        Keypair::new(
            self.pubkey,
            self.encrypted_secret_key.and_then(|e| e.decrypt(pass).ok()),
        )
    }
}

impl TokenSerializable for Pubkey {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.parse_token(PUBKEY_TOKEN)?;
        let raw = parser.pull_token()?;
        let pubkey =
            Pubkey::try_from_bech32_string(raw, true).map_err(|_| ParseError::DecodeFailed)?;
        Ok(pubkey)
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        writer.write_token(PUBKEY_TOKEN);

        let Some(bech) = self.npub() else {
            tracing::error!("Could not convert pubkey to bech: {}", self.hex());
            return;
        };

        writer.write_token(&bech);
    }
}

impl TokenSerializable for Keypair {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        TokenParser::alt(
            parser,
            &[
                |p| Ok(Keypair::only_pubkey(Pubkey::parse_from_tokens(p)?)),
                |p| Ok(Keypair::from_secret(parse_seckey(p)?)),
            ],
        )
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        if let Some(seckey) = &self.secret_key {
            writer.write_token(ESECKEY_TOKEN);
            let maybe_eseckey = EncryptedSecretKey::new(
                seckey,
                ESECKEY_PASS,
                7,
                nostr::nips::nip49::KeySecurity::Unknown,
            );

            let Ok(eseckey) = maybe_eseckey else {
                tracing::error!("Could not convert seckey to EncryptedSecretKey");
                return;
            };
            let Ok(serialized) = eseckey.to_bech32() else {
                tracing::error!("Could not serialize ncryptsec");
                return;
            };

            writer.write_token(&serialized);
        } else {
            self.pubkey.serialize_tokens(writer);
        }
    }
}

const ESECKEY_TOKEN: &str = "eseckey";
const ESECKEY_PASS: &str = "notedeck";
const PUBKEY_TOKEN: &str = "pubkey";

fn parse_seckey<'a>(parser: &mut TokenParser<'a>) -> Result<SecretKey, ParseError<'a>> {
    parser.parse_token(ESECKEY_TOKEN)?;

    let raw = parser.pull_token()?;

    let eseckey = EncryptedSecretKey::from_bech32(raw).map_err(|_| ParseError::DecodeFailed)?;

    let seckey = eseckey
        .decrypt(ESECKEY_PASS)
        .map_err(|_| ParseError::DecodeFailed)?;

    Ok(seckey)
}

#[cfg(test)]
mod tests {

    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use super::{FullKeypair, Keypair};

    #[test]
    fn test_token_eseckey_serialize_deserialize() {
        let kp = FullKeypair::generate();

        let mut writer = TokenWriter::new("\t");
        kp.clone().to_keypair().serialize_tokens(&mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();

        let mut parser = TokenParser::new(data);
        let m_new_kp = Keypair::parse_from_tokens(&mut parser);
        assert!(m_new_kp.is_ok());

        let new_kp = m_new_kp.unwrap();

        assert_eq!(kp, new_kp.to_full().unwrap().to_full());
    }

    #[test]
    fn test_token_pubkey_serialize_deserialize() {
        let kp = Keypair::only_pubkey(FullKeypair::generate().pubkey);

        let mut writer = TokenWriter::new("\t");
        kp.clone().serialize_tokens(&mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();

        let mut parser = TokenParser::new(data);
        let m_new_kp = Keypair::parse_from_tokens(&mut parser);
        assert!(m_new_kp.is_ok());

        let new_kp = m_new_kp.unwrap();

        assert_eq!(kp, new_kp);
    }
}
