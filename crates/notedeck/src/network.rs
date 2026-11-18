use std::{
    error::Error,
    fmt,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
};

use http_body_util::{BodyExt, Empty};
use hyper::{
    body::Bytes,
    header::{self},
    Request, Uri,
};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::connect::dns::{GaiResolver, Name},
    client::legacy::connect::HttpConnector,
    client::legacy::Client,
    rt::TokioExecutor,
};
use url::Url;

const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

type HyperClient = Client<HttpsConnector<HttpConnector<CheckedResolver>>, Empty<Bytes>>;

/// A check of each address a client is about to connect to: `false` refuses
/// it (see [`HyperHttpClient::with_addr_check`]).
pub type AddrCheck = fn(IpAddr) -> bool;

/// The system resolver (getaddrinfo), keeping only the addresses an
/// [`AddrCheck`] allows. The connector dials only what a resolver answers,
/// so a refused address is never connected to, whatever name led to it.
#[derive(Clone)]
struct CheckedResolver {
    gai: GaiResolver,
    /// `None` keeps every address.
    allow: Option<AddrCheck>,
}

impl tower_service::Service<Name> for CheckedResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.gai.poll_ready(cx)
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolving = self.gai.call(name.clone());
        let allow = self.allow;
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = resolving
                .await?
                .filter(|addr| allow.is_none_or(|allow| allow(addr.ip())))
                .collect();
            if addrs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} resolves only to refused addresses", name.as_str()),
                ));
            }
            Ok(addrs.into_iter())
        })
    }
}

/// A guarded request's check of each URL it is about to request: `Err`
/// with the reason refuses it (see [`HyperHttpClient::req_guarded`]).
pub type UrlCheck = dyn Fn(&Uri) -> Result<(), String> + Sync;

/// Reusable HTTP client for background network jobs.
#[derive(Clone)]
pub struct HyperHttpClient {
    client: HyperClient,
}

impl HyperHttpClient {
    /// Build a client once so repeated relay metadata fetches do not reload
    /// TLS roots and rebuild connector state per request.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// A client that connects only to the addresses `allow` accepts, as the
    /// name it was asked for *resolves*: a name pointing at a refused
    /// address fails to connect, and the connection goes to the very
    /// address that was checked, so a name can't be re-pointed between the
    /// check and the connect (DNS rebinding). An IP literal in strict
    /// dotted or IPv6 form skips resolution and isn't checked here; check
    /// those per URL with [`HyperHttpClient::req_guarded`].
    pub fn with_addr_check(allow: AddrCheck) -> Self {
        Self::build(Some(allow))
    }

    fn build(allow: Option<AddrCheck>) -> Self {
        let mut http = HttpConnector::new_with_resolver(CheckedResolver {
            gai: GaiResolver::new(),
            allow,
        });
        // https URLs pass through this connector to the TLS layer.
        http.enforce_http(false);
        let https = {
            let builder = match HttpsConnectorBuilder::new().with_native_roots() {
                Ok(builder) => builder,
                Err(err) => {
                    tracing::warn!(
                        "Failed to load native root certificates ({err}). Falling back to WebPKI store."
                    );
                    HttpsConnectorBuilder::new().with_webpki_roots()
                }
            };

            builder.https_or_http().enable_http1().wrap_connector(http)
        };

        Self {
            client: Client::builder(TokioExecutor::new()).build(https),
        }
    }

    /// Perform an HTTP GET and set an explicit `Accept` header.
    pub async fn req_accept(
        &self,
        url: &str,
        accept: &str,
    ) -> Result<HyperHttpResponse, HyperHttpError> {
        self.req_with_accept(url, Some(accept)).await
    }

    /// Perform an HTTP GET for an agent, checking every hop: `allow` is
    /// asked about the URL and about each redirect's target before it is
    /// requested, and a refusal ends the request with
    /// [`HyperHttpError::Refused`]. A redirect is where a public URL turns
    /// into a private one, so checking only the first URL checks nothing.
    pub async fn req_guarded(
        &self,
        url: &str,
        accept: &str,
        allow: &UrlCheck,
    ) -> Result<HyperHttpResponse, HyperHttpError> {
        http_req_with_client(&self.client, url, Some(accept), Some(allow)).await
    }

    async fn req_with_accept(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> Result<HyperHttpResponse, HyperHttpError> {
        http_req_with_client(&self.client, url, accept, None).await
    }
}

impl Default for HyperHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn http_req(url: &str) -> Result<HyperHttpResponse, HyperHttpError> {
    http_req_with_accept(url, None).await
}

/// Perform an HTTP GET and set an explicit `Accept` header.
pub async fn http_req_accept(url: &str, accept: &str) -> Result<HyperHttpResponse, HyperHttpError> {
    http_req_with_accept(url, Some(accept)).await
}

async fn http_req_with_accept(
    url: &str,
    accept: Option<&str>,
) -> Result<HyperHttpResponse, HyperHttpError> {
    let client = HyperHttpClient::new();
    client.req_with_accept(url, accept).await
}

async fn http_req_with_client(
    client: &HyperClient,
    url: &str,
    accept: Option<&str>,
    allow: Option<&UrlCheck>,
) -> Result<HyperHttpResponse, HyperHttpError> {
    let mut current_uri: Uri = url.parse().map_err(|_| HyperHttpError::Uri)?;

    const MAX_REDIRECTS: usize = 5;
    let mut redirects = 0;

    let res = loop {
        if let Some(allow) = allow {
            allow(&current_uri).map_err(HyperHttpError::Refused)?;
        }
        let authority = current_uri.authority().ok_or(HyperHttpError::Host)?.clone();

        // Fetch the url...
        let mut req_builder = Request::builder()
            .uri(current_uri.clone())
            .header(hyper::header::HOST, authority.as_str());
        if let Some(accept) = accept {
            req_builder = req_builder.header(hyper::header::ACCEPT, accept);
        }
        let req = req_builder
            .body(Empty::<Bytes>::new())
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        let res = client
            .request(req)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        if res.status().is_redirection() {
            if redirects >= MAX_REDIRECTS {
                return Err(HyperHttpError::TooManyRedirects);
            }

            let location_header = res
                .headers()
                .get(header::LOCATION)
                .ok_or(HyperHttpError::MissingRedirectLocation)?
                .clone();

            let location = location_header
                .to_str()
                .map_err(|_| HyperHttpError::InvalidRedirectLocation)?
                .to_string();

            res.into_body()
                .collect()
                .await
                .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

            current_uri = resolve_redirect(&current_uri, &location)?;
            redirects += 1;
            continue;
        } else {
            break res;
        }
    };

    let content_type = res
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|t| t.to_str().ok())
        .map(|s| s.to_string());
    let status_code = res.status().as_u16();

    let content_length = res
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());

    if let Some(len) = content_length {
        if len > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }
    }

    let mut body = res.into_body();
    let mut bytes = Vec::with_capacity(content_length.unwrap_or(0).min(MAX_BODY_BYTES));

    while let Some(frame_result) = body.frame().await {
        let frame = frame_result.map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;
        let Ok(chunk) = frame.into_data() else {
            continue;
        };

        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }

        bytes.extend_from_slice(&chunk);
    }

    Ok(HyperHttpResponse {
        status_code,
        content_type,
        bytes,
    })
}

#[derive(Debug)]
pub enum HyperHttpError {
    Hyper(Box<dyn std::error::Error + Send + Sync>),
    Host,
    Uri,
    BodyTooLarge,
    TooManyRedirects,
    MissingRedirectLocation,
    InvalidRedirectLocation,
    /// A guarded request's check refused a URL it would have requested
    /// (see [`HyperHttpClient::req_guarded`]), with the reason.
    Refused(String),
}

#[derive(Debug)]
pub struct HyperHttpResponse {
    pub status_code: u16,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl HyperHttpError {
    /// This error and every cause under it, joined by `: `. hyper's own
    /// message for a failed connect is only "client error (Connect)"; the
    /// reason (a refused address, a DNS failure) is a cause further down.
    pub fn with_causes(&self) -> String {
        let mut message = self.to_string();
        let mut cause = self.source();
        while let Some(err) = cause {
            message.push_str(": ");
            message.push_str(&err.to_string());
            cause = err.source();
        }
        message
    }
}

impl Error for HyperHttpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hyper(e) => Some(&**e),
            _ => None,
        }
    }
}

impl fmt::Display for HyperHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hyper(e) => write!(f, "Hyper error: {e}"),
            Self::Host => write!(f, "Missing host in URL"),
            Self::Uri => write!(f, "Invalid URI"),
            Self::BodyTooLarge => write!(f, "Body too large"),
            Self::TooManyRedirects => write!(f, "Too many redirect responses"),
            Self::MissingRedirectLocation => write!(f, "Redirect response missing Location header"),
            Self::InvalidRedirectLocation => write!(f, "Invalid redirect Location header"),
            Self::Refused(why) => write!(f, "Refused: {why}"),
        }
    }
}

fn resolve_redirect(current: &Uri, location: &str) -> Result<Uri, HyperHttpError> {
    if let Ok(uri) = location.parse::<Uri>() {
        if uri.scheme().is_some() {
            return Ok(uri);
        }
    }

    let base = Url::parse(&current.to_string()).map_err(|_| HyperHttpError::Uri)?;
    let joined = base
        .join(location)
        .map_err(|_| HyperHttpError::InvalidRedirectLocation)?;

    joined
        .as_str()
        .parse::<Uri>()
        .map_err(|_| HyperHttpError::InvalidRedirectLocation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A server on 127.0.0.1 answering every request `200 ok`; its port.
    async fn local_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });
        port
    }

    /// A name is checked as it resolves, not as it is written: `127.1` and
    /// `localhost` both reach the local server through the system resolver,
    /// and a client refusing loopback refuses both.
    #[tokio::test]
    async fn an_address_check_refuses_names_that_resolve_to_refused_addresses() {
        enostr::install_crypto();
        let port = local_server().await;
        let open = HyperHttpClient::new();
        let checked = HyperHttpClient::with_addr_check(|ip| !ip.is_loopback());
        for host in ["127.1", "localhost"] {
            let url = format!("http://{host}:{port}/");
            let reached = open.req_accept(&url, "*/*").await.expect(&url);
            assert_eq!(reached.bytes, b"ok", "{url}");

            let refused = checked.req_accept(&url, "*/*").await;
            let err = refused.expect_err(&url);
            let chain = err.with_causes();
            assert!(chain.contains("refused addresses"), "{url}: {chain}");
        }
    }
}
