use std::{error::Error, fmt};

use http_body_util::{BodyExt, Empty, Limited};
use hyper::{
    body::Bytes,
    header::{self},
    Request, Uri,
};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use url::Url;

const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

pub async fn http_req(url: &str) -> Result<HyperHttpResponse, HyperHttpError> {
    let mut current_uri: Uri = url.parse().map_err(|_| HyperHttpError::Uri)?;

    let https = {
        let builder = HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("Failed to load native root certificates");

        builder.https_or_http().enable_http1().build()
    };

    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    const MAX_REDIRECTS: usize = 5;
    let mut redirects = 0;

    let res = loop {
        let authority = current_uri.authority().ok_or(HyperHttpError::Host)?.clone();

        // Fetch the url...
        let req = Request::builder()
            .uri(current_uri.clone())
            .header(hyper::header::HOST, authority.as_str())
            .body(Empty::<Bytes>::new())
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        let res = client
            .request(req)
            .await
            .map_err(|e: hyper_util::client::legacy::Error| HyperHttpError::Hyper(Box::new(e)))?;

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

            // Consume the redirect body
            let _ = BodyExt::collect(res.into_body()).await;

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
        .and_then(|t: &hyper::header::HeaderValue| t.to_str().ok())
        .map(|s: &str| s.to_string());

    let content_length = res
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|s: &hyper::header::HeaderValue| s.to_str().ok())
        .and_then(|s: &str| s.parse::<usize>().ok());

    if let Some(len) = content_length {
        if len > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }
    }

    // Limit body size and collect all bytes
    let limited_body = Limited::new(res.into_body(), MAX_BODY_BYTES);
    let collected = BodyExt::collect(limited_body)
        .await
        .map_err(|e| HyperHttpError::Hyper(e))?;
    let bytes = collected.to_bytes().to_vec();

    Ok(HyperHttpResponse {
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
}

#[derive(Debug)]
pub struct HyperHttpResponse {
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
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
