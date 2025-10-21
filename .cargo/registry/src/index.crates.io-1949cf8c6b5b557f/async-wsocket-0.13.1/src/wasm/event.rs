// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use web_sys::CloseEvent as JsCloseEvt;

use crate::wasm::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    /// The connection is now Open and ready for use.
    Open,
    /// An error happened on the connection. For more information about when this event
    /// occurs, see the [HTML Living Standard](https://html.spec.whatwg.org/multipage/web-sockets.html).
    /// Since the browser is not allowed to convey any information to the client code as to why an error
    /// happened (for security reasons), as described in the HTML specification, there usually is no extra
    /// information available. That's why this event has no data attached to it.
    Error,
    /// The connection has started closing, but is not closed yet. You shouldn't try to send messages over
    /// it anymore. Trying to do so will result in an error.
    Closing,
    /// The connection was closed. The enclosed [`CloseEvent`] has some extra information.
    Closed(CloseEvent),
    /// An error happened, not on the connection, but inside _ws_stream_wasm_. This currently happens
    /// when an incoming message can not be converted to Rust types, eg. a String message with invalid
    /// encoding.
    WsErr(Error),
}

impl WsEvent {
    /// Predicate indicating whether this is a [WsEvent::Open] event. Can be used as a filter for the
    /// event stream obtained with [`pharos::Observable::observe`] on [`WsMeta`](crate::WsMeta).
    #[inline]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    /// Predicate indicating whether this is a [WsEvent::Closed] event. Can be used as a filter for the
    /// event stream obtained with [`pharos::Observable::observe`] on [`WsMeta`](crate::WsMeta).
    #[inline]
    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Closed(_))
    }
}

/// An event holding information about how/why the connection was closed.
///
/// See: [MDN Documentation](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/close).
// We use this wrapper because the web_sys version isn't Send and pharos requires events
// to be Send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseEvent {
    /// The close code.
    pub code: u16,
    /// The reason why the connection was closed.
    pub reason: String,
    /// Whether the connection was closed cleanly.
    pub was_clean: bool,
}

impl From<JsCloseEvt> for CloseEvent {
    fn from(js_evt: JsCloseEvt) -> Self {
        Self {
            code: js_evt.code(),
            reason: js_evt.reason(),
            was_clean: js_evt.was_clean(),
        }
    }
}
