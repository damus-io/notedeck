//! Native implementation of the WebSocket client using the `tungstenite` crate.

use std::net::TcpStream;
use std::{
    ops::ControlFlow,
    sync::mpsc::{Receiver, TryRecvError},
};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::WebSocket;

use crate::tungstenite_common::into_requester;
use crate::{EventHandler, Options, Result, WsEvent, WsMessage};

/// This is how you send [`WsMessage`]s to the server.
///
/// When the last clone of this is dropped, the connection is closed.
pub struct WsSender {
    tx: Option<std::sync::mpsc::Sender<WsMessage>>,
}

impl Drop for WsSender {
    fn drop(&mut self) {
        self.close();
    }
}

impl WsSender {
    /// Send a message.
    ///
    /// You have to wait for [`WsEvent::Opened`] before you can start sending messages.
    pub fn send(&mut self, msg: WsMessage) {
        if let Some(tx) = &self.tx {
            tx.send(msg).ok();
        }
    }

    /// Close the connection.
    ///
    /// This is called automatically when the sender is dropped.
    pub fn close(&mut self) {
        if self.tx.is_some() {
            log::debug!("Closing WebSocket");
        }
        self.tx = None;
    }

    /// Forget about this sender without closing the connection.
    pub fn forget(mut self) {
        #[allow(clippy::mem_forget)] // intentional
        std::mem::forget(self.tx.take());
    }
}

pub(crate) fn ws_receive_impl(url: String, options: Options, on_event: EventHandler) -> Result<()> {
    std::thread::Builder::new()
        .name("ewebsock".to_owned())
        .spawn(move || {
            if let Err(err) = ws_receiver_blocking(&url, options, &on_event) {
                on_event(WsEvent::Error(err));
            } else {
                log::debug!("WebSocket connection closed.");
            }
        })
        .map_err(|err| format!("Failed to spawn thread: {err}"))?;

    Ok(())
}

/// Connect and call the given event handler on each received event.
///
/// Blocking version of [`crate::ws_receive`], only available on native.
///
/// # Errors
/// All errors are returned to the caller, and NOT reported via `on_event`.
pub fn ws_receiver_blocking(url: &str, options: Options, on_event: &EventHandler) -> Result<()> {
    let uri: tungstenite::http::Uri = url
        .parse()
        .map_err(|err| format!("Failed to parse URL {url:?}: {err}"))?;
    let config = tungstenite::protocol::WebSocketConfig::from(options.clone());
    let max_redirects = 3; // tungstenite default

    let read_timeout = options.read_timeout;
    let (mut socket, response) = match tungstenite::client::connect_with_config(
        into_requester(uri, options),
        Some(config),
        max_redirects,
    ) {
        Ok(result) => result,
        Err(err) => {
            return Err(format!("Connect: {err}"));
        }
    };

    set_read_timeout(&mut socket, read_timeout)?;

    log::debug!("WebSocket HTTP response code: {}", response.status());
    log::trace!(
        "WebSocket response contains the following headers: {:?}",
        response.headers()
    );

    let control = on_event(WsEvent::Opened);
    if control.is_break() {
        log::trace!("Closing connection due to Break");
        return socket
            .close(None)
            .map_err(|err| format!("Failed to close connection: {err}"));
    }

    loop {
        let control = read_from_socket(&mut socket, on_event)?;

        if control.is_break() {
            log::trace!("Closing connection due to Break");
            return socket
                .close(None)
                .map_err(|err| format!("Failed to close connection: {err}"));
        }

        std::thread::yield_now();
    }
}

pub(crate) fn ws_connect_impl(
    url: String,
    options: Options,
    on_event: EventHandler,
) -> Result<WsSender> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("ewebsock".to_owned())
        .spawn(move || {
            if let Err(err) = ws_connect_blocking(&url, options, &on_event, &rx) {
                on_event(WsEvent::Error(err));
            } else {
                log::debug!("WebSocket connection closed.");
            }
        })
        .map_err(|err| format!("Failed to spawn thread: {err}"))?;

    Ok(WsSender { tx: Some(tx) })
}

/// Connect and call the given event handler on each received event.
///
/// This is a blocking variant of [`crate::ws_connect`], only available on native.
///
/// # Errors
/// All errors are returned to the caller, and NOT reported via `on_event`.
pub fn ws_connect_blocking(
    url: &str,
    options: Options,
    on_event: &EventHandler,
    rx: &Receiver<WsMessage>,
) -> Result<()> {
    let config = tungstenite::protocol::WebSocketConfig::from(options.clone());
    let max_redirects = 3; // tungstenite default
    let uri: tungstenite::http::Uri = url
        .parse()
        .map_err(|err| format!("Failed to parse URL {url:?}: {err}"))?;

    let read_timeout = options.read_timeout;
    let (mut socket, response) = match tungstenite::client::connect_with_config(
        into_requester(uri, options),
        Some(config),
        max_redirects,
    ) {
        Ok(result) => result,
        Err(err) => {
            return Err(format!("Connect: {err}"));
        }
    };

    set_read_timeout(&mut socket, read_timeout)?;

    log::debug!("WebSocket HTTP response code: {}", response.status());
    log::trace!(
        "WebSocket response contains the following headers: {:?}",
        response.headers()
    );

    let control = on_event(WsEvent::Opened);
    if control.is_break() {
        log::trace!("Closing connection due to Break");
        return socket
            .close(None)
            .map_err(|err| format!("Failed to close connection: {err}"));
    }

    loop {
        match rx.try_recv() {
            Ok(outgoing_message) => {
                let outgoing_message = match outgoing_message {
                    WsMessage::Text(text) => tungstenite::protocol::Message::Text(text),
                    WsMessage::Binary(data) => tungstenite::protocol::Message::Binary(data),
                    WsMessage::Ping(data) => tungstenite::protocol::Message::Ping(data),
                    WsMessage::Pong(data) => tungstenite::protocol::Message::Pong(data),
                    WsMessage::Unknown(_) => panic!("You cannot send WsMessage::Unknown"),
                };
                if let Err(err) = socket.send(outgoing_message) {
                    socket.close(None).ok();
                    socket.flush().ok();
                    return Err(format!("send: {err}"));
                }
            }
            Err(TryRecvError::Disconnected) => {
                log::debug!("WsSender dropped - closing connection.");
                socket.close(None).ok();
                socket.flush().ok();
                return Ok(());
            }
            Err(TryRecvError::Empty) => {}
        };

        let control = read_from_socket(&mut socket, on_event)?;

        if control.is_break() {
            log::trace!("Closing connection due to Break");
            return socket
                .close(None)
                .map_err(|err| format!("Failed to close connection: {err}"));
        }

        std::thread::yield_now();
    }
}

fn read_from_socket(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    on_event: &EventHandler,
) -> Result<ControlFlow<()>> {
    let control = match socket.read() {
        Ok(incoming_msg) => match incoming_msg {
            tungstenite::protocol::Message::Text(text) => {
                on_event(WsEvent::Message(WsMessage::Text(text)))
            }
            tungstenite::protocol::Message::Binary(data) => {
                on_event(WsEvent::Message(WsMessage::Binary(data)))
            }
            tungstenite::protocol::Message::Ping(data) => {
                on_event(WsEvent::Message(WsMessage::Ping(data)))
            }
            tungstenite::protocol::Message::Pong(data) => {
                on_event(WsEvent::Message(WsMessage::Pong(data)))
            }
            tungstenite::protocol::Message::Close(close) => {
                on_event(WsEvent::Closed);
                log::debug!("WebSocket close received: {close:?}");
                ControlFlow::Break(())
            }
            tungstenite::protocol::Message::Frame(_) => ControlFlow::Continue(()),
        },
        // If we get `WouldBlock`, then the read timed out.
        // Windows may emit `TimedOut` instead.
        Err(tungstenite::Error::Io(io_err))
            if io_err.kind() == std::io::ErrorKind::WouldBlock
                || io_err.kind() == std::io::ErrorKind::TimedOut =>
        {
            ControlFlow::Continue(()) // Ignore
        }
        Err(err) => {
            return Err(format!("read: {err}"));
        }
    };

    Ok(control)
}

fn set_read_timeout(
    s: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    value: Option<std::time::Duration>,
) -> Result<()> {
    // zero timeout is the same as no timeout
    if value.is_none() || value.is_some_and(|value| value.is_zero()) {
        return Ok(());
    }

    match s.get_mut() {
        MaybeTlsStream::Plain(s) => {
            s.set_read_timeout(value)
                .map_err(|err| format!("failed to set read timeout: {err}"))?;
        }
        #[cfg(feature = "tls")]
        MaybeTlsStream::Rustls(s) => {
            s.get_mut()
                .set_read_timeout(value)
                .map_err(|err| format!("failed to set read timeout: {err}"))?;
        }
        _ => {}
    };

    Ok(())
}

#[test]
fn test_connect() {
    let options = crate::Options::default();
    // see documentation for more options
    let (mut sender, _receiver) = crate::connect("ws://example.com", options).unwrap();
    sender.send(crate::WsMessage::Text("Hello!".into()));
}
