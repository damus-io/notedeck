use std::sync::mpsc::Sender;
use std::time::Duration;

use enostr::{Nip11FetchRequest, Nip11LimitationsRaw, NormRelayUrl};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::jobs::{JobCache, JobOutput, JobPackage, JobRun, RunType};

const NIP11_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Relay limit background job cache.
pub type RelayLimitJobs = JobCache<RelayLimitJobKind, RelayLimitJobResult>;

/// Sender for relay limit jobs.
pub type RelayLimitJobSender = Sender<JobPackage<RelayLimitJobKind, RelayLimitJobResult>>;

/// Supported relay limit job kinds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelayLimitJobKind {
    Nip11Fetch,
}

/// Result payload emitted by a completed relay limit job.
#[derive(Debug, Clone)]
pub struct RelayLimitJobResult {
    pub relay: NormRelayUrl,
    pub result: Result<Nip11LimitationsRaw, Nip11FetchError>,
}

/// Errors while downloading or parsing relay NIP-11 documents.
#[derive(Debug, Error, Clone)]
pub enum Nip11FetchError {
    #[error("invalid relay URL: {0}")]
    InvalidRelayUrl(String),
    #[error("unsupported relay URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("http request failed: {0}")]
    Http(String),
    #[error("NIP-11 fetch timed out after {0:?}")]
    Timeout(Duration),
    #[error("NIP-11 endpoint returned non-success status: {0}")]
    HttpStatus(u16),
    #[error("invalid NIP-11 json: {0}")]
    Json(String),
}

#[derive(Debug, Deserialize)]
struct Nip11Document {
    limitation: Option<Nip11LimitationsRaw>,
}

/// Queue a NIP-11 fetch job for the provided relay request.
pub fn enqueue_nip11_fetch(sender: &RelayLimitJobSender, req: Nip11FetchRequest) {
    let id = req.relay.to_string();
    let relay = req.relay;
    let run = JobRun::Async(Box::pin(async move {
        let result = fetch_nip11_raw_limits(&relay).await;
        JobOutput::complete(RelayLimitJobResult { relay, result })
    }));

    if let Err(error) = sender.send(JobPackage::new(
        id,
        RelayLimitJobKind::Nip11Fetch,
        RunType::Output(run),
    )) {
        tracing::error!("failed to enqueue relay NIP-11 job: {error}");
    }
}

async fn fetch_nip11_raw_limits(
    relay: &NormRelayUrl,
) -> Result<Nip11LimitationsRaw, Nip11FetchError> {
    let http_url = relay_url_to_http(relay)?;
    let response = tokio::time::timeout(
        NIP11_FETCH_TIMEOUT,
        crate::media::network::http_req_accept(&http_url, "application/nostr+json"),
    )
    .await
    .map_err(|_| Nip11FetchError::Timeout(NIP11_FETCH_TIMEOUT))?
    .map_err(|error| Nip11FetchError::Http(error.to_string()))?;
    if !(200..300).contains(&response.status_code) {
        return Err(Nip11FetchError::HttpStatus(response.status_code));
    }
    let parsed: Nip11Document = serde_json::from_slice(&response.bytes)
        .map_err(|error| Nip11FetchError::Json(error.to_string()))?;
    Ok(parsed.limitation.unwrap_or_default())
}

fn relay_url_to_http(relay: &NormRelayUrl) -> Result<String, Nip11FetchError> {
    let relay_url = relay.to_string();
    let mut url =
        Url::parse(&relay_url).map_err(|_| Nip11FetchError::InvalidRelayUrl(relay_url))?;

    let replacement = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        other => return Err(Nip11FetchError::UnsupportedScheme(other.to_owned())),
    };

    url.set_scheme(replacement)
        .map_err(|_| Nip11FetchError::UnsupportedScheme(replacement.to_owned()))?;
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    /// Installs a process-level rustls crypto provider for HTTP client tests.
    fn install_crypto_provider_for_tests() {
        #[cfg(windows)]
        {
            let provider = rustls::crypto::ring::default_provider();
            let _ = provider.install_default();
        }

        #[cfg(not(windows))]
        {
            let provider = rustls::crypto::aws_lc_rs::default_provider();
            let _ = provider.install_default();
        }
    }

    async fn spawn_one_shot_http_server(
        status_line: &str,
        content_type: &str,
        body: &str,
        expected_accept: Option<&str>,
    ) -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind localhost");
        let addr = listener.local_addr().expect("local addr");
        let status_line = status_line.to_owned();
        let content_type = content_type.to_owned();
        let body_bytes = body.as_bytes().to_vec();
        let expected_accept = expected_accept.map(str::to_owned);

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request_buf = vec![0_u8; 4096];
            let read = stream.read(&mut request_buf).await.expect("read request");
            let request = String::from_utf8_lossy(&request_buf[..read]);

            if let Some(expected_accept) = expected_accept {
                let accept_header = format!("accept: {}\r\n", expected_accept.to_ascii_lowercase());
                let request = request.to_ascii_lowercase();
                assert!(
                    request.contains(&accept_header),
                    "missing Accept header in request: {request}"
                );
            }

            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body_bytes.len()
            );
            stream
                .write_all(head.as_bytes())
                .await
                .expect("write response head");
            stream
                .write_all(&body_bytes)
                .await
                .expect("write response body");
            stream.shutdown().await.expect("shutdown stream");
        });

        (addr, handle)
    }

    /// Ensures websocket relay URLs are translated to matching HTTP(S) URIs for NIP-11.
    #[test]
    fn relay_url_to_http_converts_websocket_schemes() {
        let secure = NormRelayUrl::new("wss://relay.damus.io").expect("valid relay");
        let insecure = NormRelayUrl::new("ws://localhost:7000").expect("valid relay");

        assert_eq!(
            relay_url_to_http(&secure).expect("convert secure"),
            "https://relay.damus.io/"
        );
        assert_eq!(
            relay_url_to_http(&insecure).expect("convert insecure"),
            "http://localhost:7000/"
        );
    }

    /// Ensures non-2xx NIP-11 HTTP responses map to a dedicated `HttpStatus` error.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_nip11_raw_limits_maps_non_success_status_to_http_status_error() {
        install_crypto_provider_for_tests();
        let (addr, server) = spawn_one_shot_http_server(
            "503 Service Unavailable",
            "application/nostr+json",
            r#"{"error":"unavailable"}"#,
            Some("application/nostr+json"),
        )
        .await;
        let relay = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay");

        let result = fetch_nip11_raw_limits(&relay).await;
        server.await.expect("server finished");

        assert!(matches!(result, Err(Nip11FetchError::HttpStatus(503))));
    }

    /// Ensures a valid 2xx NIP-11 document returns parsed raw limitation fields.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_nip11_raw_limits_parses_limitation_fields_on_success() {
        install_crypto_provider_for_tests();
        let (addr, server) = spawn_one_shot_http_server(
            "200 OK",
            "application/nostr+json",
            r#"{"limitation":{"max_message_length":16384,"max_subscriptions":300}}"#,
            Some("application/nostr+json"),
        )
        .await;
        let relay = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay");

        let result = fetch_nip11_raw_limits(&relay).await;
        server.await.expect("server finished");

        let raw = result.expect("successful parse");
        assert_eq!(raw.max_message_length, Some(16384));
        assert_eq!(raw.max_subscriptions, Some(300));
    }

    /// Ensures invalid JSON bodies on 2xx responses map to `Json` parse errors.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_nip11_raw_limits_maps_invalid_json_to_json_error() {
        install_crypto_provider_for_tests();
        let (addr, server) = spawn_one_shot_http_server(
            "200 OK",
            "application/nostr+json",
            "{ this is not valid json",
            Some("application/nostr+json"),
        )
        .await;
        let relay = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay");

        let result = fetch_nip11_raw_limits(&relay).await;
        server.await.expect("server finished");

        assert!(matches!(result, Err(Nip11FetchError::Json(_))));
    }

    /// Ensures NIP-11 responses without a `limitation` object produce default raw limits.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_nip11_raw_limits_defaults_when_limitation_is_missing() {
        install_crypto_provider_for_tests();
        let (addr, server) = spawn_one_shot_http_server(
            "200 OK",
            "application/nostr+json",
            r#"{"name":"relay"}"#,
            Some("application/nostr+json"),
        )
        .await;
        let relay = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay");

        let result = fetch_nip11_raw_limits(&relay).await;
        server.await.expect("server finished");

        let raw = result.expect("successful parse");
        assert_eq!(raw, Nip11LimitationsRaw::default());
    }

    /// Ensures transport-level connection failures map to the `Http` fetch error variant.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_nip11_raw_limits_maps_transport_failure_to_http_error() {
        install_crypto_provider_for_tests();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind localhost");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let relay = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay");
        let result = fetch_nip11_raw_limits(&relay).await;
        assert!(matches!(result, Err(Nip11FetchError::Http(_))));
    }
}
