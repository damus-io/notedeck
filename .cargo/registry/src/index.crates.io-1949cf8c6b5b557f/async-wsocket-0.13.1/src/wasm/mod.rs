// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

//! Wasm

#![allow(clippy::arc_with_non_send_sync)]

use std::time::Duration;

use async_utility::{task, time};
use url::Url;

mod error;
mod event;
mod message;
mod pharos;
mod socket;
mod state;
mod stream;

pub use self::error::Error;
use self::event::{CloseEvent, WsEvent};
use self::pharos::SharedPharos;
use self::socket::WebSocket as WasmWebSocket;
use self::state::WsState;
pub(crate) use self::stream::WsStream;
use crate::socket::WebSocket;

pub async fn connect(url: &Url, timeout: Duration) -> Result<WebSocket, Error> {
    let (_ws, stream) = time::timeout(Some(timeout), WasmWebSocket::connect(url))
        .await
        .ok_or(Error::Timeout)??;
    Ok(WebSocket::Wasm(stream))
}

/// Helper function to reduce code bloat
pub(crate) fn notify(pharos: SharedPharos<WsEvent>, evt: WsEvent) {
    task::spawn(async move {
        pharos
            .notify(evt)
            .await
            .map_err(|e| unreachable!("{:?}", e))
            .unwrap(); // only happens if we closed it.
    });
}
