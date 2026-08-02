use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use super::enostr_api::NormRelayUrl;

pub type CapturedTextFrames = Arc<Mutex<Vec<String>>>;
pub type CaptureNotify = Arc<Notify>;
pub type CaptureRelay = (
    tokio::task::JoinHandle<()>,
    NormRelayUrl,
    CapturedTextFrames,
    CaptureNotify,
);

/// Text replies and connection action returned by a capture relay handler.
#[derive(Default)]
pub struct CaptureRelayResponse {
    pub send_text: Vec<String>,
    pub close: bool,
}

impl CaptureRelayResponse {
    /// Capture the inbound frame without sending a reply.
    pub fn none() -> Self {
        Self::default()
    }
}

/// Create a local websocket relay that captures matching text frames and lets
/// each accepted connection compute optional text replies.
pub async fn create_filtered_capture_relay_with_handler<Factory, Handler>(
    should_capture: fn(&str) -> bool,
    handler_factory: Factory,
) -> CaptureRelay
where
    Factory: Fn() -> Handler + Clone + Send + Sync + 'static,
    Handler: FnMut(&str) -> CaptureRelayResponse + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind capture relay");
    let addr = listener.local_addr().expect("capture relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid capture relay url");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_task = Arc::clone(&captured);
    let notify = Arc::new(Notify::new());
    let notify_task = Arc::clone(&notify);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let captured_task = Arc::clone(&captured_task);
            let notify_task = Arc::clone(&notify_task);
            let handler_factory = handler_factory.clone();
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };
                let mut handle_text = handler_factory();

                while let Some(msg) = websocket.next().await {
                    let Ok(Message::Text(text)) = msg else {
                        continue;
                    };
                    let text = text.to_string();

                    if should_capture(&text) {
                        captured_task
                            .lock()
                            .expect("lock captured text frames")
                            .push(text.clone());
                        notify_task.notify_one();
                    }

                    let response = handle_text(&text);
                    for reply in response.send_text {
                        websocket
                            .send(Message::Text(reply))
                            .await
                            .expect("send capture relay response");
                    }
                    if response.close {
                        let _ = websocket.close(None).await;
                        break;
                    }
                }
            });
        }
    });

    (handle, url, captured, notify)
}

pub async fn create_filtered_capture_relay(should_capture: fn(&str) -> bool) -> CaptureRelay {
    create_filtered_capture_relay_with_handler(should_capture, || {
        |_: &str| CaptureRelayResponse::none()
    })
    .await
}

pub async fn create_text_capture_relay() -> CaptureRelay {
    create_filtered_capture_relay(|_| true).await
}

pub async fn create_req_capture_relay() -> CaptureRelay {
    create_filtered_capture_relay(|text| text.starts_with("[\"REQ\",")).await
}
