use crate::channels::Channels;
use crate::invoice::Invoice;
use serde::Serialize;
use serde_json::Value;

pub enum ConnectionState {
    Dead(String),
    Connecting,
    Active,
}
pub enum LoadingState<T, E> {
    Loading,
    Failed(E),
    Loaded(T),
}

impl<T, E> Default for LoadingState<T, E> {
    fn default() -> Self {
        Self::Loading
    }
}

impl<T, E> LoadingState<T, E> {
    fn _as_ref(&self) -> LoadingState<&T, &E> {
        match self {
            Self::Loading => LoadingState::<&T, &E>::Loading,
            Self::Failed(err) => LoadingState::<&T, &E>::Failed(err),
            Self::Loaded(t) => LoadingState::<&T, &E>::Loaded(t),
        }
    }

    pub fn from_result(res: Result<T, E>) -> LoadingState<T, E> {
        match res {
            Ok(r) => LoadingState::Loaded(r),
            Err(err) => LoadingState::Failed(err),
        }
    }

    /*
    fn unwrap(self) -> T {
        let Self::Loaded(t) = self else {
            panic!("unwrap in LoadingState");
        };

        t
    }
    */
}

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
