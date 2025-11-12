//! Native implementation of the WebSocket client using the `tungstenite` crate.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpStream};
use std::{
    ops::ControlFlow,
    sync::mpsc::{Receiver, TryRecvError},
};

use tungstenite::client::IntoClientRequest;
use tungstenite::error::{Error as WsError, UrlError};
use tungstenite::handshake::client::Response;
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::{Mode, MaybeTlsStream};
use tungstenite::WebSocket;

use crate::tungstenite_common::into_requester;
use crate::{
    EventHandler, Options, Result, SocksAuth, SocksOptions, Transport, WsEvent, WsMessage,
};

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
                let _ = on_event(WsEvent::Error(err));
            } else {
                log::debug!("WebSocket connection closed.");
            }
        })
        .map_err(|err| format!("Failed to spawn thread: {err}"))?;

    Ok(())
}

fn connect_socket(
    url: &str,
    options: Options,
) -> Result<(WebSocket<MaybeTlsStream<TcpStream>>, Response)> {
    let uri: tungstenite::http::Uri = url
        .parse()
        .map_err(|err| format!("Failed to parse URL {url:?}: {err}"))?;
    let max_redirects = 3;
    let options_for_config = options.clone();
    let transport = options_for_config.transport.clone();
    let config = tungstenite::protocol::WebSocketConfig::from(options_for_config);
    let requester = into_requester(uri.clone(), options);

    let connect_result = match transport {
        Transport::Direct => tungstenite::client::connect_with_config(
            requester,
            Some(config.clone()),
            max_redirects,
        ),
        Transport::Socks(cfg) => {
            let request = requester
                .into_client_request()
                .map_err(|err| format!("Connect: {err}"))?;
            try_socks_handshake(request, Some(config), &cfg)
        }
    };

    connect_result.map_err(|err| format!("Connect: {err}"))
}

/// Connect and call the given event handler on each received event.
///
/// Blocking version of [`crate::ws_receive`], only available on native.
///
/// # Errors
/// All errors are returned to the caller, and NOT reported via `on_event`.
pub fn ws_receiver_blocking(url: &str, options: Options, on_event: &EventHandler) -> Result<()> {
    let read_timeout = options.read_timeout;
    let (mut socket, response) = connect_socket(url, options)?;

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
                let _ = on_event(WsEvent::Error(err));
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
    let read_timeout = options.read_timeout;
    let (mut socket, response) = connect_socket(url, options)?;

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
                let _ = on_event(WsEvent::Closed);
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

/// Complete a tungstenite handshake while tunnelling through a SOCKS proxy.
fn try_socks_handshake(
    request: tungstenite::handshake::client::Request,
    config: Option<WebSocketConfig>,
    socks: &SocksOptions,
) -> std::result::Result<(WebSocket<MaybeTlsStream<TcpStream>>, Response), WsError> {
    use tungstenite::client::{client_with_config, uri_mode};

    let mode = uri_mode(request.uri())?;
    let host = request
        .uri()
        .host()
        .ok_or_else(|| WsError::Url(UrlError::NoHostName))?;
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(match mode {
            Mode::Plain => 80,
            Mode::Tls => 443,
        });

    let stream =
        socks_connect(&socks.proxy_address, host, port, socks.auth.as_ref()).map_err(WsError::Io)?;
    stream
        .set_nodelay(true)
        .map_err(|err| WsError::Io(err))?;

    match mode {
        Mode::Plain => client_with_config(request, MaybeTlsStream::Plain(stream), config).map_err(
            |e| match e {
                HandshakeError::Failure(f) => f,
                HandshakeError::Interrupted(_) => panic!("Bug: blocking handshake not blocked"),
            },
        ),
        Mode::Tls => {
            #[cfg(not(feature = "tls"))]
            {
                Err(WsError::Url(UrlError::TlsFeatureNotEnabled))
            }
            #[cfg(feature = "tls")]
            {
                tungstenite::client_tls_with_config(request, stream, config, None).map_err(
                    |e| match e {
                        HandshakeError::Failure(f) => f,
                        HandshakeError::Interrupted(_) => {
                            panic!("Bug: TLS handshake not blocked")
                        }
                    },
                )
            }
        }
    }
}

fn socks_connect(
    proxy_addr: &str,
    host: &str,
    port: u16,
    auth: Option<&SocksAuth>,
) -> io::Result<TcpStream> {
    use io::ErrorKind;

    let mut stream = TcpStream::connect(proxy_addr)?;

    let mut greeting = vec![0x05];
    if auth.is_some() {
        greeting.push(0x02);
        greeting.push(0x00);
        greeting.push(0x02);
    } else {
        greeting.push(0x01);
        greeting.push(0x00);
    }

    stream.write_all(&greeting)?;
    stream.flush()?;

    let mut response = [0u8; 2];
    stream.read_exact(&mut response)?;
    if response[0] != 0x05 {
        return Err(io::Error::new(
            ErrorKind::Other,
            format!("Unsupported SOCKS version {}", response[0]),
        ));
    }

    match (response[1], auth) {
        (0x00, _) => {}
        (0x02, Some(creds)) => {
            send_socks_auth(&mut stream, creds)?;
        }
        (0x02, None) => {
            return Err(io::Error::new(
                ErrorKind::Other,
                "SOCKS proxy requires username/password authentication",
            ));
        }
        (_, _) => {
            return Err(io::Error::new(
                ErrorKind::Other,
                format!("Unsupported SOCKS auth method {}", response[1]),
            ));
        }
    }

    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(ipv4) = host.parse::<Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&ipv4.octets());
    } else if let Ok(ipv6) = host.parse::<Ipv6Addr>() {
        request.push(0x04);
        request.extend_from_slice(&ipv6.octets());
    } else {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > u8::MAX as usize {
            return Err(io::Error::new(
                ErrorKind::Other,
                "SOCKS hostname too long",
            ));
        }
        request.push(0x03);
        request.push(host_bytes.len() as u8);
        request.extend_from_slice(host_bytes);
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request)?;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    if header[0] != 0x05 {
        return Err(io::Error::new(
            ErrorKind::Other,
            "Invalid SOCKS response version",
        ));
    }
    if header[1] != 0x00 {
        return Err(io::Error::new(
            ErrorKind::Other,
            format!("SOCKS connection failed with code {}", header[1]),
        ));
    }

    let addr_len = match header[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            len[0] as usize
        }
        _ => {
            return Err(io::Error::new(
                ErrorKind::Other,
                "Unknown SOCKS address type",
            ))
        }
    };

    let mut skip = vec![0u8; addr_len];
    stream.read_exact(&mut skip)?;
    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf)?;

    Ok(stream)
}

fn send_socks_auth(stream: &mut TcpStream, creds: &SocksAuth) -> io::Result<()> {
    let username = creds.username.as_bytes();
    let password = creds.password.as_bytes();
    if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "SOCKS credentials too long",
        ));
    }

    let mut auth_req = Vec::with_capacity(3 + username.len() + password.len());
    auth_req.push(0x01);
    auth_req.push(username.len() as u8);
    auth_req.extend_from_slice(username);
    auth_req.push(password.len() as u8);
    auth_req.extend_from_slice(password);

    stream.write_all(&auth_req)?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp)?;
    if resp[1] != 0x00 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "SOCKS authentication failed",
        ));
    }
    Ok(())
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
