mod cache;
mod default_zap;
mod networking;
mod zap;

pub use cache::{
    AnyZapState, NoteZapTarget, NoteZapTargetOwned, ZapTarget, ZapTargetOwned, ZappingError, Zaps,
};

pub use zap::parse_bolt11_msats;

pub use default_zap::{
    get_current_default_msats, DefaultZapError, DefaultZapMsats, PendingDefaultZapState,
    UserZapMsats,
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};

use crate::ZapError;

pub enum ZapAddress {
    Lud16(String),
    Lud06(String),
}

pub fn get_users_zap_address(
    txn: &Transaction,
    ndb: &Ndb,
    receiver: &Pubkey,
) -> Result<ZapAddress, ZapError> {
    let Some(profile) = ndb
        .get_profile_by_pubkey(txn, receiver.bytes())
        .map_err(|e| ZapError::Ndb(e.to_string()))?
        .record()
        .profile()
    else {
        return Err(ZapError::Ndb(format!("No profile for {receiver}")));
    };

    let Some(address) = profile
        .lud06()
        .map(|l| ZapAddress::Lud06(l.to_string()))
        .or(profile.lud16().map(|l| ZapAddress::Lud16(l.to_string())))
    else {
        return Err(ZapError::Ndb(format!(
            "profile for {receiver} doesn't have lud06 or lud16"
        )));
    };

    Ok(address)
}
