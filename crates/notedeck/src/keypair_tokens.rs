//! Token codec for [`Keypair`] persistence.
//!
//! notedeck serializes accounts as tokenator string tokens (see
//! [`crate::user_account`]). The keypair *types* live in `nostrdb_net` (via
//! `enostr`'s re-export), and this token format is a notedeck storage concern —
//! not a nostr primitive — so the codec lives here, next to its only consumer,
//! rather than in `enostr`/`nostrdb_net`. It's free functions, not a
//! `TokenSerializable` impl, because both the trait (`tokenator`'s) and the type
//! (`nostrdb_net`'s) are foreign to this crate and the orphan rule forbids the
//! impl.

use enostr::{Keypair, Pubkey, SecretKey};
use nostr::nips::nip19::FromBech32;
use nostr::nips::nip19::ToBech32;
use nostr::nips::nip49::EncryptedSecretKey;
use tokenator::{ParseError, TokenParser, TokenWriter};

const ESECKEY_TOKEN: &str = "eseckey";
const ESECKEY_PASS: &str = "notedeck";
const PUBKEY_TOKEN: &str = "pubkey";

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
/// secret key. The counterpart to [`serialize_keypair_tokens`].
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
    use enostr::{FullKeypair, Keypair};
    use tokenator::{TokenParser, TokenWriter};

    use super::{parse_keypair_from_tokens, serialize_keypair_tokens};

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
