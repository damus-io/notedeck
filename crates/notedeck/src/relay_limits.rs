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
