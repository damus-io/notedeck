// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use web_sys::WebSocket;

use crate::wasm::Error;

/// Indicates the state of a Websocket connection. The only state in which it's valid to send and receive messages
/// is [WsState::Open].
///
/// See [MDN](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/readyState) for the ready state values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsState {
    Connecting,
    Open,
    Closing,
    Closed,
}

/// Internally ready state is a u16, so it's possible to create one from a u16. Only 0-3 are valid values.
///
/// See [MDN](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/readyState) for the ready state values.
impl TryFrom<u16> for WsState {
    type Error = Error;

    fn try_from(state: u16) -> Result<Self, Self::Error> {
        match state {
            WebSocket::CONNECTING => Ok(WsState::Connecting),
            WebSocket::OPEN => Ok(WsState::Open),
            WebSocket::CLOSING => Ok(WsState::Closing),
            WebSocket::CLOSED => Ok(WsState::Closed),
            _ => Err(Error::InvalidWsState { supplied: state }),
        }
    }
}
