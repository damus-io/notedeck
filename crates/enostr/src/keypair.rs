//! Keypair token codec.
//!
//! The keypair *types* (`Keypair`, `FullKeypair`, `FilledKeypair`,
//! `KeypairUnowned`, `SerializableKeypair`) now live in `nostrdb_net` and are
//! re-exported here during the nostrdb-net convergence (phase 4). enostr keeps
//! only the tokenator-based codec: `nostrdb_net` can't own it because
//! `tokenator` is an unpublished notedeck path-crate the fork can't depend on,
//! and the orphan rule forbids enostr from `impl`ing tokenator's trait for the
//! now-foreign `Keypair`/`Pubkey` — so the codec is exposed as free functions.

use nostr::nips::nip19::FromBech32;
use nostr::nips::nip19::ToBech32;
use nostr::nips::nip49::EncryptedSecretKey;
use tokenator::ParseError;
use tokenator::TokenParser;
use tokenator::TokenWriter;

use crate::Pubkey;
use crate::SecretKey;

pub use nostrdb_net::{FilledKeypair, FullKeypair, Keypair, KeypairUnowned, SerializableKeypair};

const ESECKEY_TOKEN: &str = "eseckey";
const ESECKEY_PASS: &str = "notedeck";
const PUBKEY_TOKEN: &str = "pubkey";

/// Token codec for a bare [`Pubkey`], as a free function rather than a
/// `TokenSerializable for Pubkey` impl: `Pubkey` is re-exported from
/// `nostrdb_net`, and the orphan rule forbids enostr from implementing
/// tokenator's trait for that foreign type.
fn parse_pubkey_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Pubkey, ParseError<'a>> {
    parser.parse_token(PUBKEY_TOKEN)?;
    let raw = parser.pull_token()?;
    Pubkey::try_from_bech32_string(raw, true).map_err(|_| ParseError::DecodeFailed)
}

fn serialize_pubkey_tokens(pubkey: &Pubkey, writer: &mut TokenWriter) {
    writer.write_token(PUBKEY_TOKEN);

    let Some(bech) = pubkey.npub() else {
        tracing::error!("Could not convert pubkey to bech: {}", pubkey.hex());
        return;
    };

    writer.write_token(&bech);
}

/// Parse a [`Keypair`] from tokens — either a bare pubkey or a NIP-49 encrypted
/// secret key. The tokenator counterpart to [`serialize_keypair_tokens`]. A free
/// function (not a `TokenSerializable` impl) because `Keypair` is re-exported
/// from `nostrdb_net` and the orphan rule forbids the impl here.
pub fn parse_keypair_from_tokens<'a>(
    parser: &mut TokenParser<'a>,
) -> Result<Keypair, ParseError<'a>> {
    TokenParser::alt(
        parser,
        &[
            |p| Ok(Keypair::only_pubkey(parse_pubkey_from_tokens(p)?)),
            |p| Ok(Keypair::from_secret(parse_seckey(p)?)),
        ],
    )
}

/// Serialize a [`Keypair`] to tokens: its NIP-49 encrypted secret key when one
/// is present, else just the pubkey. See [`parse_keypair_from_tokens`].
pub fn serialize_keypair_tokens(keypair: &Keypair, writer: &mut TokenWriter) {
    if let Some(seckey) = &keypair.secret_key {
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
        serialize_pubkey_tokens(&keypair.pubkey, writer);
    }
}

fn parse_seckey<'a>(parser: &mut TokenParser<'a>) -> Result<SecretKey, ParseError<'a>> {
    parser.parse_token(ESECKEY_TOKEN)?;

    let raw = parser.pull_token()?;

    let eseckey = EncryptedSecretKey::from_bech32(raw).map_err(|_| ParseError::DecodeFailed)?;

    let seckey = eseckey
        .to_secret_key(ESECKEY_PASS)
        .map_err(|_| ParseError::DecodeFailed)?;

    Ok(seckey)
}

#[cfg(test)]
mod tests {
    use tokenator::{TokenParser, TokenWriter};

    use super::{parse_keypair_from_tokens, serialize_keypair_tokens, FullKeypair, Keypair};

    #[test]
    fn from_secret_bytes_derives_matching_pubkey() {
        // A round-trip: generating a keypair then rebuilding it from its own
        // secret bytes must recover the same public key.
        let kp = FullKeypair::generate();
        let bytes = kp.secret_key.secret_bytes();
        let rebuilt = FullKeypair::from_secret_bytes(&bytes).expect("valid secret");
        assert_eq!(kp, rebuilt);

        // An all-zero secret key is invalid on secp256k1 and must be rejected.
        assert!(FullKeypair::from_secret_bytes(&[0u8; 32]).is_none());
    }

    #[test]
    fn test_token_eseckey_serialize_deserialize() {
        let kp = FullKeypair::generate();

        let mut writer = TokenWriter::new("\t");
        serialize_keypair_tokens(&kp.clone().to_keypair(), &mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();

        let mut parser = TokenParser::new(data);
        let m_new_kp = parse_keypair_from_tokens(&mut parser);
        assert!(m_new_kp.is_ok());

        let new_kp = m_new_kp.unwrap();

        assert_eq!(kp, new_kp.to_full().unwrap().to_full());
    }

    #[test]
    fn test_token_pubkey_serialize_deserialize() {
        let kp = Keypair::only_pubkey(FullKeypair::generate().pubkey);

        let mut writer = TokenWriter::new("\t");
        serialize_keypair_tokens(&kp.clone(), &mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();

        let mut parser = TokenParser::new(data);
        let m_new_kp = parse_keypair_from_tokens(&mut parser);
        assert!(m_new_kp.is_ok());

        let new_kp = m_new_kp.unwrap();

        assert_eq!(kp, new_kp);
    }
}
