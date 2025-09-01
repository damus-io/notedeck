mod cache;
mod default_zap;
mod networking;
mod zap;

pub use cache::{
    AnyZapState, NoteZapTarget, NoteZapTargetOwned, ZapTarget, ZapTargetOwned, ZappingError, Zaps,
};

pub use default_zap::{
    get_current_default_msats, DefaultZapError, DefaultZapMsats, PendingDefaultZapState,
    UserZapMsats,
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};

pub enum ZapAddress {
    Lud16(String),
    Lud06(String),
}

pub fn get_users_zap_address(
    txn: &Transaction,
    ndb: &Ndb,
    receiver: &Pubkey,
) -> Option<ZapAddress> {
    let profile = ndb
        .get_profile_by_pubkey(txn, receiver.bytes())
        .ok()?
        .record()
        .profile()?;

    profile
        .lud06()
        .map(|l| ZapAddress::Lud06(l.to_string()))
        .or(profile.lud16().map(|l| ZapAddress::Lud16(l.to_string())))
}
