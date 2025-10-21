// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::fmt;
use std::sync::Arc;

use futures::StreamExt;
use url::Url;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, CloseEvent as JsCloseEvt, DomException, WebSocket as WebSysSocket};

use crate::wasm::pharos::{Filter, Observable, Observe, ObserveConfig, PharErr, SharedPharos};
use crate::wasm::{notify, CloseEvent, Error, WsEvent, WsState, WsStream};

/// The metadata related to a websocket. Allows access to the methods on the WebSocket API.
/// This is split from the `Stream`/`Sink` so you can pass the latter to a combinator whilst
/// continuing to use this API.
///
/// When you drop this, the connection does not get closed, however when you drop [WsStream] it does.
///
/// Most of the methods on this type directly map to the web API. For more documentation, check the
/// [MDN WebSocket documentation](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/WebSocket).
pub struct WebSocket {
    ws: Arc<WebSysSocket>,
    pharos: SharedPharos<WsEvent>,
}

impl WebSocket {
    const OPEN_CLOSE: Filter<WsEvent> =
        Filter::Pointer(|evt: &WsEvent| evt.is_open() | evt.is_closed());

    /// Connect to the server. The future will resolve when the connection has been established with a successful WebSocket
    /// handshake.
    pub async fn connect(url: &Url) -> Result<(Self, WsStream), Error> {
        let ws: Arc<WebSysSocket> = match WebSysSocket::new(url.as_str()) {
            Ok(ws) => Arc::new(ws),
            Err(e) => {
                let de: &DomException = e.unchecked_ref();
                return match de.code() {
                    DomException::SYNTAX_ERR => Err(Error::InvalidUrl {
                        supplied: url.to_string(),
                    }),
                    code => {
                        if code == 0 {
                            Err(Error::Other(
                                e.as_string().unwrap_or_else(|| String::from("None")),
                            ))
                        } else {
                            Err(Error::Dom(code))
                        }
                    }
                };
            }
        };

        // Create our pharos.
        let mut pharos = SharedPharos::default();
        let ph1 = pharos.clone();
        let ph2 = pharos.clone();
        let ph3 = pharos.clone();
        let ph4 = pharos.clone();

        // Setup our event listeners
        let on_open = Closure::wrap(Box::new(move || {
            // notify observers
            notify(ph1.clone(), WsEvent::Open)
        }) as Box<dyn FnMut()>);

        // TODO: is there no information at all in an error?
        #[allow(trivial_casts)]
        let on_error = Closure::wrap(Box::new(move || {
            // notify observers.
            notify(ph2.clone(), WsEvent::Error)
        }) as Box<dyn FnMut()>);

        #[allow(trivial_casts)]
        let on_close = Closure::wrap(Box::new(move |evt: JsCloseEvt| {
            let c = WsEvent::Closed(CloseEvent {
                code: evt.code(),
                reason: evt.reason(),
                was_clean: evt.was_clean(),
            });

            notify(ph3.clone(), c)
        }) as Box<dyn FnMut(JsCloseEvt)>);

        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        // In case of future task cancellation the current task may be interrupted at an await, therefore not reaching
        // the `WsStream` construction, whose `Drop` glue would have been responsible for unregistering the callbacks.
        // We therefore use a guard to be responsible for unregistering the callbacks until the `WsStream` is
        // constructed.
        let guard = {
            struct Guard<'lt> {
                ws: &'lt WebSysSocket,
            }

            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.ws.set_onopen(None);
                    self.ws.set_onclose(None);
                    self.ws.set_onerror(None);

                    // Check if connection is `OPEN`. Will cause a panic if is not `open`
                    if let Ok(WsState::Open) = self.ws.ready_state().try_into() {
                        let _ = self.ws.close();
                    }

                    println!(
                        "WebSocket::connect future was dropped while connecting to: {}.",
                        self.ws.url()
                    );
                }
            }

            Guard { ws: &ws }
        };

        // Listen to the events to figure out whether the connection opens successfully. We don't want to deal with
        // the error event. Either a close event happens, in which case we want to recover the CloseEvent to return it
        // to the user, or an Open event happens in which case we are happy campers.
        let mut evts = pharos
            .observe(Self::OPEN_CLOSE.into())
            .await
            .expect("we didn't close pharos");

        // If the connection is closed, return error

        if let Some(WsEvent::Closed(evt)) = evts.next().await {
            return Err(Error::ConnectionFailed { event: evt });
        }

        // We have now passed all the `await` points in this function and so the `WsStream` construction is guaranteed
        // so we let it take over the responsibility of unregistering the callbacks by disabling our guard.
        std::mem::forget(guard);

        // We don't handle Blob's
        ws.set_binary_type(BinaryType::Arraybuffer);

        Ok((
            Self {
                pharos,
                ws: ws.clone(),
            },
            WsStream::new(
                ws,
                ph4,
                Arc::new(on_open),
                Arc::new(on_error),
                Arc::new(on_close),
            ),
        ))
    }

    /// Retrieve the address to which this socket is connected.
    pub fn url(&self) -> String {
        self.ws.url()
    }
}

impl fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WebSocket for connection: {}", self.url())
    }
}

impl Observable<WsEvent> for WebSocket {
    type Error = PharErr;

    fn observe(&mut self, options: ObserveConfig<WsEvent>) -> Observe<'_, WsEvent, Self::Error> {
        self.pharos.observe(options)
    }
}
