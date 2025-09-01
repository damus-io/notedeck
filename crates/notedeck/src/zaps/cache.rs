use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use nwc::nostr::nips::nip47::PayInvoiceResponse;
use poll_promise::Promise;
use tokio::task::JoinError;

use crate::{get_wallet_for, Accounts, GlobalWallet, ZapError};

use super::{
    networking::{fetch_invoice_lnurl, fetch_invoice_lud16, FetchedInvoice, FetchingInvoice},
    zap::Zap,
};

type ZapId = u32;

#[derive(Default)]
pub struct Zaps {
    next_id: ZapId,
    zap_keys: hashbrown::HashMap<ZapKeyOwned, Vec<ZapId>>,
    // using `ZapId`s like this allows us to be flexible. in the future, we can also do cheap queries for any zaps from only specific senders or targets:
    // zap_targets: hashbrown::HashMap<ZapTargetOwned, Vec<ZapId>>,
    // zap_senders: hashbrown::HashMap<Pubkey, Vec<ZapId>>,
    zaps: std::collections::HashMap<ZapId, ZapState>,
    in_flight: Vec<ZapPromise>,
    events: Vec<EventResponse>,
}

fn process_event(
    id: ZapId,
    event: ZapEvent,
    accounts: &mut Accounts,
    global_wallet: &mut GlobalWallet,
    ndb: &Ndb,
    txn: &Transaction,
) -> NextState {
    match event {
        ZapEvent::FetchInvoice {
            zap_ctx,
            sender_relays,
        } => process_new_zap_event(zap_ctx, accounts, ndb, txn, sender_relays),
        ZapEvent::SendNWC {
            zap_ctx,
            req_noteid,
            invoice,
        } => {
            let Some(wallet) = get_wallet_for(accounts, global_wallet, &zap_ctx.key.sender) else {
                return NextState::Event(EventResponse {
                    id,
                    event: Err(ZappingError::SenderNoWallet),
                });
            };

            let promise = wallet.wallet.pay_invoice(&invoice);

            let ctx = SendingNWCInvoiceContext {
                request_noteid: req_noteid,
                zap_ctx,
            };
            NextState::Transition(ZapPromise::SendingNWCInvoice { ctx, promise })
        }
        ZapEvent::EndpointConfirmed {
            zap_ctx,
            req_noteid,
        } => NextState::Success {
            id: zap_ctx.id,
            zap: LocalConfirmedZap {
                request_noteid: req_noteid,
                sender: zap_ctx.key.sender,
                target: zap_ctx.key.target,
                msats: zap_ctx.msats,
            },
        },
    }
}

fn process_new_zap_event(
    zap_ctx: ZapCtx,
    accounts: &Accounts,
    ndb: &Ndb,
    txn: &Transaction,
    sender_relays: Vec<String>,
) -> NextState {
    let Some(full_kp) = accounts.get_selected_account().key.to_full() else {
        return NextState::Event(EventResponse {
            id: zap_ctx.id,
            event: Err(ZappingError::InvalidAccount),
        });
    };

    // TODO(kernelkind): support ZapTarget::Profile
    let ZapTargetOwned::Note(note_target) = zap_ctx.key.target.clone() else {
        return NextState::Event(EventResponse {
            id: zap_ctx.id,
            event: Err(ZappingError::UnsupportedOperation),
        });
    };

    let id = zap_ctx.id;
    let promise = send_note_zap(
        ndb,
        txn,
        note_target,
        zap_ctx.msats,
        &full_kp.secret_key.secret_bytes(),
        sender_relays,
    )
    .map(|promise| ZapPromise::FetchingInvoice {
        ctx: zap_ctx,
        promise,
    });
    let Some(promise) = promise else {
        return NextState::Event(EventResponse {
            id,
            event: Err(ZappingError::InvalidZapAddress),
        });
    };

    NextState::Transition(promise)
}

fn send_note_zap(
    ndb: &Ndb,
    txn: &Transaction,
    note_target: NoteZapTargetOwned,
    msats: u64,
    nsec: &[u8; 32],
    relays: Vec<String>,
) -> Option<FetchingInvoice> {
    let address = get_users_zap_address(txn, ndb, &note_target.zap_recipient)?;

    let promise = match address {
        ZapAddress::Lud16(s) => {
            fetch_invoice_lud16(s, msats, *nsec, ZapTargetOwned::Note(note_target), relays)
        }
        ZapAddress::Lud06(s) => {
            fetch_invoice_lnurl(s, msats, *nsec, ZapTargetOwned::Note(note_target), relays)
        }
    };
    Some(promise)
}

enum ZapAddress {
    Lud16(String),
    Lud06(String),
}

fn get_users_zap_address(txn: &Transaction, ndb: &Ndb, receiver: &Pubkey) -> Option<ZapAddress> {
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

fn try_get_promise_response(
    promises: &mut Vec<ZapPromise>,
    promise_index: usize, // this index must be guarenteed to exist
) -> Option<PromiseResponse> {
    if !is_promise_ready(&promises[promise_index]) {
        return None;
    }

    let promise = promises.remove(promise_index);

    match promise {
        ZapPromise::FetchingInvoice { ctx, promise } => {
            let result = promise.block_and_take();

            Some(PromiseResponse::FetchingInvoice { ctx, result })
        }
        ZapPromise::SendingNWCInvoice { ctx, promise } => {
            let result = promise.block_and_take();

            Some(PromiseResponse::SendingNWCInvoice { ctx, result })
        }
    }
}

fn is_promise_ready(zap_promise: &ZapPromise) -> bool {
    match zap_promise {
        ZapPromise::FetchingInvoice { ctx: _, promise } => promise.ready().is_some(),
        ZapPromise::SendingNWCInvoice { ctx: _, promise } => promise.ready().is_some(),
    }
}

enum NextState {
    Event(EventResponse),
    Transition(ZapPromise),
    Success { id: ZapId, zap: LocalConfirmedZap },
}

#[derive(Debug, Clone)]
struct EventResponse {
    id: ZapId,
    event: Result<ZapEvent, ZappingError>,
}

impl Zaps {
    fn get_next_id(&mut self) -> ZapId {
        let next = self.next_id;
        self.next_id += 1;
        next
    }

    fn send_event(&mut self, id: ZapId, event: ZapEvent) {
        self.events.push(EventResponse {
            id,
            event: Ok(event),
        });
    }

    pub fn send_error(&mut self, sender_pubkey: &[u8; 32], target: ZapTarget, error: ZappingError) {
        let id = self.get_next_id();
        let key = ZapKey {
            sender: sender_pubkey,
            target,
        };

        self.insert_new_state(&id, &key, ZapState::Pending(Err(error)));
    }

    pub fn send_zap(
        &mut self,
        sender_pubkey: &[u8; 32],
        sender_relays: Vec<String>,
        target: ZapTarget,
        msats: u64,
    ) {
        let id = self.get_next_id();
        let key = ZapKey {
            sender: sender_pubkey,
            target,
        };
        let event = ZapEvent::FetchInvoice {
            zap_ctx: ZapCtx {
                id,
                key: (&key).into(),
                msats,
            },
            sender_relays,
        };

        self.insert_new_state(&id, &key, ZapState::Pending(Ok(event.clone())));
        self.send_event(id, event);
    }

    fn insert_new_state(&mut self, id: &ZapId, key: &ZapKey, state: ZapState) {
        self.zaps.insert(*id, state);

        let Some(states) = self.zap_keys.get_mut(key) else {
            let states: Vec<ZapId> = vec![*id];
            self.zap_keys.insert((key).into(), states);
            return;
        };

        states.push(*id);
    }

    pub fn process(
        &mut self,
        accounts: &mut Accounts,
        global_wallet: &mut GlobalWallet,
        ndb: &Ndb,
    ) {
        for i in (0..self.in_flight.len()).rev() {
            let Some(resp) = try_get_promise_response(&mut self.in_flight, i) else {
                continue;
            };

            self.events.push(resp.take_as_event_response());
        }

        while let Some(event_resp) = self.events.pop() {
            let event = match event_resp.event {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::error!("transitioned to error for id {}: {e}", event_resp.id);
                    self.zaps.insert(event_resp.id, ZapState::Pending(Err(e)));
                    continue;
                }
            };

            let txn = nostrdb::Transaction::new(ndb).expect("txn");
            match process_event(event_resp.id, event, accounts, global_wallet, ndb, &txn) {
                NextState::Event(event_resp) => {
                    self.zaps
                        .insert(event_resp.id, ZapState::Pending(event_resp.event));
                }
                NextState::Transition(in_flight_promise) => {
                    self.in_flight.push(in_flight_promise);
                }
                NextState::Success { id, zap } => {
                    self.zaps.insert(id, ZapState::LocalConfirm(zap));
                }
            }
        }
    }

    pub fn get_states_for<'a>(
        &'a self,
        sender: &[u8; 32],
        target: ZapTarget<'a>,
    ) -> Option<Vec<&'a ZapState>> {
        let key = ZapKey { sender, target };
        let ids = self.zap_keys.get(&key)?;

        let mut states = Vec::new();
        for id in ids {
            if let Some(state) = self.zaps.get(id) {
                states.push(state);
            }
        }

        if states.is_empty() {
            return None;
        }

        Some(states)
    }

    /// if any of the states are `ZapState::Pending`, all other values will be ignored and `AnyZapState::Pending` will return
    /// if there is at least one `ZapState::LocalConfirm`, `AnyZapState::LocalOnly` will return
    /// if there are `ZapState::Confirm` and none others, `AnyZapState::Confirmed` will return
    /// otherwise `AnyZapState::None` will return
    pub fn any_zap_state_for<'a>(
        &'a self,
        sender: &[u8; 32],
        target: ZapTarget<'a>,
    ) -> Result<AnyZapState, ZappingError> {
        let key = ZapKey { sender, target };
        let Some(ids) = self.zap_keys.get(&key) else {
            return Ok(AnyZapState::None);
        };

        let mut has_confirmed = false;
        let mut has_local_confirmed = false;

        for id in ids {
            let Some(state) = self.zaps.get(id) else {
                continue;
            };

            match state {
                ZapState::Confirm(_) => {
                    has_confirmed = true;
                }
                ZapState::LocalConfirm(_) => {
                    has_local_confirmed = true;
                }
                ZapState::Pending(p) => {
                    if let Err(e) = p {
                        return Err(e.to_owned());
                    }
                    return Ok(AnyZapState::Pending);
                }
            }
        }

        if has_local_confirmed {
            return Ok(AnyZapState::LocalOnly);
        }

        if has_confirmed {
            Ok(AnyZapState::Confirmed)
        } else {
            Ok(AnyZapState::None)
        }
    }

    pub fn clear_error_for(&mut self, sender: &[u8; 32], target: ZapTarget<'_>) {
        let key = ZapKey { sender, target };
        let Some(ids) = self.zap_keys.get_mut(&key) else {
            return;
        };

        ids.retain(|id| {
            let should_keep = !matches!(self.zaps.get(id), Some(ZapState::Pending(Err(_))));
            if !should_keep {
                self.zaps.remove(id);
            }
            should_keep
        });
    }
}

#[derive(Clone)]
pub enum AnyZapState {
    None,
    Pending,
    LocalOnly,
    Confirmed,
}

#[derive(Debug)]
pub enum ZapState {
    Confirm(Zap),
    LocalConfirm(LocalConfirmedZap),
    Pending(Result<ZapEvent, ZappingError>),
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct LocalConfirmedZap {
    request_noteid: NoteId,
    sender: Pubkey,
    target: ZapTargetOwned,
    msats: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ZapKeyOwned {
    sender: Pubkey,
    target: ZapTargetOwned,
}

#[derive(Debug, Hash)]
struct ZapKey<'a> {
    sender: &'a [u8; 32],
    target: ZapTarget<'a>,
}

struct SendingNWCInvoiceContext {
    request_noteid: NoteId,
    zap_ctx: ZapCtx,
}

#[derive(Clone, Debug)]
pub struct ZapCtx {
    id: ZapId,
    key: ZapKeyOwned,
    msats: u64,
}

#[derive(Clone, Debug)]
pub enum ZapEvent {
    FetchInvoice {
        zap_ctx: ZapCtx,
        sender_relays: Vec<String>,
    },
    SendNWC {
        zap_ctx: ZapCtx,
        req_noteid: NoteId,
        invoice: String,
    },
    EndpointConfirmed {
        zap_ctx: ZapCtx,
        req_noteid: NoteId,
    },
}

#[derive(Clone, Debug)]
pub enum ZappingError {
    InvoiceFetchFailed(ZapError),
    InvalidAccount,
    UnsupportedOperation, // TODO(kernelkind): support profile zaps
    InvalidZapAddress,
    SenderNoWallet,
    InvalidNWCResponse(String),
    FutureError(String),
}

impl std::fmt::Display for ZappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZappingError::InvoiceFetchFailed(err) => write!(f, "Failed to fetch invoice: {err}"),
            ZappingError::InvalidAccount => write!(f, "Invalid account"),
            ZappingError::UnsupportedOperation => {
                write!(f, "Unsupported operation (e.g. profile zaps)")
            }
            ZappingError::InvalidZapAddress => write!(f, "Invalid zap address"),
            ZappingError::SenderNoWallet => write!(f, "Sender has no wallet"),
            ZappingError::InvalidNWCResponse(msg) => write!(f, "Invalid NWC response: {msg}"),
            ZappingError::FutureError(msg) => write!(f, "Future error: {msg}"),
        }
    }
}

enum ZapPromise {
    FetchingInvoice {
        ctx: ZapCtx,
        promise: Promise<Result<Result<FetchedInvoice, ZapError>, JoinError>>,
    },
    SendingNWCInvoice {
        ctx: SendingNWCInvoiceContext,
        promise: Promise<Result<PayInvoiceResponse, nwc::Error>>,
    },
}

enum PromiseResponse {
    FetchingInvoice {
        ctx: ZapCtx,
        result: Result<Result<FetchedInvoice, ZapError>, JoinError>,
    },
    SendingNWCInvoice {
        ctx: SendingNWCInvoiceContext,
        result: Result<PayInvoiceResponse, nwc::Error>,
    },
}

impl PromiseResponse {
    pub fn take_as_event_response(self) -> EventResponse {
        match self {
            PromiseResponse::FetchingInvoice { ctx, result } => {
                let id = ctx.id;
                let event = match result {
                    Ok(r) => match r {
                        Ok(invoice) => Ok(ZapEvent::SendNWC {
                            zap_ctx: ctx,
                            req_noteid: invoice.request_noteid,
                            invoice: invoice.invoice,
                        }),
                        Err(e) => {
                            tracing::error!("NWC error: {e}");
                            Err(ZappingError::InvoiceFetchFailed(e))
                        }
                    },
                    Err(e) => Err(ZappingError::FutureError(e.to_string())),
                };

                EventResponse { id, event }
            }
            PromiseResponse::SendingNWCInvoice { ctx, result } => {
                let id = ctx.zap_ctx.id;
                let event = match result {
                    Ok(_) => Ok(ZapEvent::EndpointConfirmed {
                        zap_ctx: ctx.zap_ctx,
                        req_noteid: ctx.request_noteid,
                    }),
                    Err(e) => Err(ZappingError::InvalidNWCResponse(e.to_string())),
                };

                EventResponse { id, event }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum ZapTargetOwned {
    Profile(Pubkey),
    Note(NoteZapTargetOwned),
}

#[derive(Debug, Hash)]
pub enum ZapTarget<'a> {
    Profile(&'a [u8; 32]),
    Note(NoteZapTarget<'a>),
}

impl ZapTargetOwned {
    #[allow(dead_code)]
    pub fn pubkey(&self) -> &Pubkey {
        match &self {
            ZapTargetOwned::Profile(pubkey) => pubkey,
            ZapTargetOwned::Note(note_zap_target) => &note_zap_target.zap_recipient,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct NoteZapTargetOwned {
    pub note_id: NoteId,
    pub zap_recipient: Pubkey,
}

#[derive(Debug, Hash)]
pub struct NoteZapTarget<'a> {
    pub note_id: &'a [u8; 32],
    pub zap_recipient: &'a [u8; 32],
}

impl From<&NoteZapTarget<'_>> for NoteZapTargetOwned {
    fn from(value: &NoteZapTarget) -> Self {
        Self {
            note_id: NoteId::new(*value.note_id),
            zap_recipient: Pubkey::new(*value.zap_recipient),
        }
    }
}

impl<'a> From<&'a NoteZapTargetOwned> for NoteZapTarget<'a> {
    fn from(value: &'a NoteZapTargetOwned) -> Self {
        Self {
            note_id: value.note_id.bytes(),
            zap_recipient: value.zap_recipient.bytes(),
        }
    }
}

impl From<&ZapTarget<'_>> for ZapTargetOwned {
    fn from(value: &ZapTarget) -> Self {
        match value {
            ZapTarget::Profile(pubkey) => ZapTargetOwned::Profile(Pubkey::new(**pubkey)),
            ZapTarget::Note(note_zap_target) => ZapTargetOwned::Note(note_zap_target.into()),
        }
    }
}

impl<'a> From<&'a ZapTargetOwned> for ZapTarget<'a> {
    fn from(value: &'a ZapTargetOwned) -> Self {
        match value {
            ZapTargetOwned::Profile(pubkey) => ZapTarget::Profile(pubkey.bytes()),
            ZapTargetOwned::Note(note_zap_target_owned) => {
                ZapTarget::Note(note_zap_target_owned.into())
            }
        }
    }
}

impl From<&ZapKey<'_>> for ZapKeyOwned {
    fn from(value: &ZapKey) -> Self {
        Self {
            sender: Pubkey::new(*value.sender),
            target: (&value.target).into(),
        }
    }
}

impl hashbrown::Equivalent<ZapKeyOwned> for ZapKey<'_> {
    fn equivalent(&self, key: &ZapKeyOwned) -> bool {
        if key.sender.bytes() != self.sender {
            return false;
        }

        match (&self.target, &key.target) {
            (ZapTarget::Profile(a), ZapTargetOwned::Profile(b)) => *a == b.bytes(),
            (ZapTarget::Note(a), ZapTargetOwned::Note(b)) => {
                a.note_id == b.note_id.bytes() && a.zap_recipient == b.zap_recipient.bytes()
            }
            _ => false,
        }
    }
}
