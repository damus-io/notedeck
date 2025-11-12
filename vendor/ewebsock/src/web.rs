#![allow(trivial_casts)]

use std::{ops::ControlFlow, rc::Rc};

use crate::{EventHandler, Options, Result, WsEvent, WsMessage};

#[allow(clippy::needless_pass_by_value)]
fn string_from_js_value(s: wasm_bindgen::JsValue) -> String {
    s.as_string().unwrap_or(format!("{s:#?}"))
}

#[allow(clippy::needless_pass_by_value)]
fn string_from_js_string(s: js_sys::JsString) -> String {
    s.as_string().unwrap_or(format!("{s:#?}"))
}

/// This is how you send messages to the server.
///
/// When this is dropped, the connection is closed.
pub struct WsSender {
    socket: Option<Rc<web_sys::WebSocket>>,
}

impl Drop for WsSender {
    fn drop(&mut self) {
        self.close();
    }
}

impl WsSender {
    /// Send the message to the server.
    pub fn send(&mut self, msg: WsMessage) {
        if let Some(socket) = &mut self.socket {
            let result = match msg {
                WsMessage::Binary(data) => {
                    socket.set_binary_type(web_sys::BinaryType::Blob);
                    socket.send_with_u8_array(&data)
                }
                WsMessage::Text(text) => socket.send_with_str(&text),
                unknown => {
                    panic!("Don't know how to send message: {unknown:?}");
                }
            };
            if let Err(err) = result.map_err(string_from_js_value) {
                log::error!("Failed to send: {err:?}");
            }
        }
    }

    /// Close the connection.
    ///
    /// This is called automatically when the sender is dropped.
    pub fn close(&mut self) {
        if let Some(socket) = self.socket.take() {
            close_socket(&socket);
        }
    }

    /// Forget about this sender without closing the connection.
    pub fn forget(mut self) {
        self.socket = None;
    }
}

pub(crate) fn ws_receive_impl(url: String, options: Options, on_event: EventHandler) -> Result<()> {
    ws_connect_impl(url, options, on_event).map(|sender| sender.forget())
}

#[allow(clippy::needless_pass_by_value)] // For consistency with the native version
pub(crate) fn ws_connect_impl(
    url: String,
    _ignored_options: Options,
    on_event: EventHandler,
) -> Result<WsSender> {
    // Based on https://rustwasm.github.io/wasm-bindgen/examples/websockets.html

    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast as _;

    // Connect to an server
    let socket = web_sys::WebSocket::new(&url).map_err(string_from_js_value)?;
    let socket = Rc::new(socket);

    // For small binary messages, like CBOR, Arraybuffer is more efficient than Blob handling
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);

    // Allow it to be shared by the different callbacks:
    let on_event: Rc<dyn Send + Fn(WsEvent) -> ControlFlow<()>> = on_event.into();

    // onmessage callback
    {
        let on_event = on_event.clone();
        let socket2 = socket.clone();
        let onmessage_callback = Closure::wrap(Box::new(move |e: web_sys::MessageEvent| {
            // Handle difference Text/Binary,...
            let control = if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let array = js_sys::Uint8Array::new(&abuf);
                on_event(WsEvent::Message(WsMessage::Binary(array.to_vec())))
            } else if let Ok(blob) = e.data().dyn_into::<web_sys::Blob>() {
                // better alternative to juggling with FileReader is to use https://crates.io/crates/gloo-file
                let file_reader = web_sys::FileReader::new().expect("Failed to create FileReader");
                let file_reader_clone = file_reader.clone();
                // create onLoadEnd callback
                let on_event = on_event.clone();
                let socket3 = socket2.clone();
                let onloadend_cb = Closure::wrap(Box::new(move |_e: web_sys::ProgressEvent| {
                    let control = match file_reader_clone.result() {
                        Ok(file_reader) => {
                            let array = js_sys::Uint8Array::new(&file_reader);
                            on_event(WsEvent::Message(WsMessage::Binary(array.to_vec())))
                        }
                        Err(err) => on_event(WsEvent::Error(format!(
                            "Failed to read binary blob: {}",
                            string_from_js_value(err)
                        ))),
                    };
                    if control.is_break() {
                        close_socket(&socket3);
                    }
                })
                    as Box<dyn FnMut(web_sys::ProgressEvent)>);
                file_reader.set_onloadend(Some(onloadend_cb.as_ref().unchecked_ref()));
                file_reader
                    .read_as_array_buffer(&blob)
                    .expect("blob not readable");
                onloadend_cb.forget();
                ControlFlow::Continue(())
            } else if let Ok(txt) = e.data().dyn_into::<js_sys::JsString>() {
                on_event(WsEvent::Message(WsMessage::Text(string_from_js_string(
                    txt,
                ))))
            } else {
                log::debug!("Unknown websocket message received: {:?}", e.data());
                on_event(WsEvent::Message(WsMessage::Unknown(string_from_js_value(
                    e.data(),
                ))))
            };
            if control.is_break() {
                close_socket(&socket2);
            }
        }) as Box<dyn FnMut(web_sys::MessageEvent)>);

        // set message event handler on WebSocket
        socket.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));

        // forget the callback to keep it alive
        onmessage_callback.forget();
    }

    {
        let on_event = on_event.clone();
        let onerror_callback = Closure::wrap(Box::new(move |error_event: web_sys::ErrorEvent| {
            // using reflect instead of error_event.message() to avoid panic on null
            let message = js_sys::Reflect::get(&error_event, &"message".into()).unwrap_or_default();
            let error = js_sys::Reflect::get(&error_event, &"error".into()).unwrap_or_default();
            log::error!("error event: {:?}: {:?}", message, error);
            on_event(WsEvent::Error(
                message
                    .as_string()
                    .unwrap_or_else(|| "Unknown error".to_owned()),
            ));
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);
        socket.set_onerror(Some(onerror_callback.as_ref().unchecked_ref()));
        onerror_callback.forget();
    }

    {
        let socket2 = socket.clone();
        let on_event = on_event.clone();
        let onopen_callback = Closure::wrap(Box::new(move |_| {
            let control = on_event(WsEvent::Opened);
            if control.is_break() {
                close_socket(&socket2);
            }
        }) as Box<dyn FnMut(wasm_bindgen::JsValue)>);
        socket.set_onopen(Some(onopen_callback.as_ref().unchecked_ref()));
        onopen_callback.forget();
    }

    {
        let onclose_callback = Closure::wrap(Box::new(move |_| {
            on_event(WsEvent::Closed);
        }) as Box<dyn FnMut(wasm_bindgen::JsValue)>);
        socket.set_onclose(Some(onclose_callback.as_ref().unchecked_ref()));
        onclose_callback.forget();
    }

    Ok(WsSender {
        socket: Some(socket),
    })
}

fn close_socket(socket: &web_sys::WebSocket) {
    if let Err(err) = socket.close() {
        log::warn!("Failed to close WebSocket: {}", string_from_js_value(err));
    } else {
        log::debug!("Closed WebSocket");
    }
}
