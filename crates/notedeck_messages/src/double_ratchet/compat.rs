use serde::de::DeserializeOwned;
use serde::Serialize;

use nostr_double_ratchet::{Error, Result};

pub(crate) fn to_ndr_pubkey(pubkey: nostr::PublicKey) -> Result<nostr44::PublicKey> {
    nostr44::PublicKey::from_slice(&pubkey.to_bytes()).map_err(Error::from)
}

pub(crate) fn to_app_pubkey(pubkey: nostr44::PublicKey) -> Result<nostr::PublicKey> {
    nostr::PublicKey::from_slice(&pubkey.to_bytes())
        .map_err(|err| Error::InvalidEvent(format!("convert pubkey: {err}")))
}

pub(crate) fn ndr_keys_from_secret(secret: [u8; 32]) -> Result<nostr44::Keys> {
    let secret = nostr44::SecretKey::from_slice(&secret)?;
    Ok(nostr44::Keys::new(secret))
}

pub(crate) fn app_event_to_ndr(event: &nostr::Event) -> Result<nostr44::Event> {
    json_convert(event, "convert app event to ndr event")
}

pub(crate) fn ndr_event_to_app(event: &nostr44::Event) -> Result<nostr::Event> {
    json_convert(event, "convert ndr event to app event")
}

pub(crate) fn app_unsigned_to_ndr(event: &nostr::UnsignedEvent) -> Result<nostr44::UnsignedEvent> {
    json_convert(event, "convert app unsigned event to ndr unsigned event")
}

fn json_convert<T, U>(value: &T, label: &'static str) -> Result<U>
where
    T: Serialize,
    U: DeserializeOwned,
{
    let json = serde_json::to_string(value)
        .map_err(|err| Error::InvalidEvent(format!("{label}: {err}")))?;
    serde_json::from_str(&json).map_err(|err| Error::InvalidEvent(format!("{label}: {err}")))
}
