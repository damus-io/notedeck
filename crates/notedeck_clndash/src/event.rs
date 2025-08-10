use lightning_invoice::Bolt11Invoice;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Debug, Clone)]
pub struct WaitRequest {
    pub indexname: String,
    pub subsystem: String,
    pub nextvalue: u64,
}

#[derive(Clone, Debug)]
pub enum Request {
    GetInfo,
    ListPeerChannels,
    PaidInvoices(u32),
}

#[derive(Deserialize, Serialize)]
pub struct ListPeerChannel {
    pub short_channel_id: String,
    pub our_reserve_msat: i64,
    pub to_us_msat: i64,
    pub total_msat: i64,
    pub their_reserve_msat: i64,
}

pub struct Channel {
    pub to_us: i64,
    pub to_them: i64,
    pub original: ListPeerChannel,
}

pub struct Channels {
    pub max_total_msat: i64,
    pub avail_in: i64,
    pub avail_out: i64,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Invoice {
    pub lastpay_index: Option<u64>,
    pub label: String,
    pub bolt11: Bolt11Invoice,
    pub payment_hash: String,
    pub amount_msat: u64,
    pub status: String,
    pub description: String,
    pub expires_at: u64,
    pub created_index: u64,
    pub updated_index: u64,
}

/// Responses from the socket
pub enum ClnResponse {
    GetInfo(Value),
    ListPeerChannels(Result<Channels, lnsocket::Error>),
    PaidInvoices(Result<Vec<Invoice>, lnsocket::Error>),
}

pub enum Event {
    /// We lost the socket somehow
    Ended {
        reason: String,
    },

    Connected,

    Response(ClnResponse),
}
