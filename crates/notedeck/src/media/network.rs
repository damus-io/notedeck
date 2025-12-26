use std::{error::Error, fmt};

use http_body_util::{BodyExt, Empty};
use hyper::{
    body::{Bytes, Incoming},
    header::{self},
    Request, Response, Uri,
};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use url::Url;

const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

pub async fn http_req(url: &str) -> Result<HyperHttpResponse, HyperHttpError> {
    let mut current_uri: Uri = url.parse().map_err(|_| HyperHttpError::Uri)?;

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

        builder.https_or_http().enable_http1().build()
    };

    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    const MAX_REDIRECTS: usize = 5;
    let mut redirects = 0;

    // Follow redirects until we get a non-redirect response
    let (content_type, body) = loop {
        let authority = current_uri.authority().ok_or(HyperHttpError::Host)?.clone();

        let req = Request::builder()
            .uri(current_uri.clone())
            .header(hyper::header::HOST, authority.as_str())
            .body(Empty::<Bytes>::new())
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        let res: Response<Incoming> = client
            .request(req)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        if !res.status().is_redirection() {
            // Extract what we need before consuming the response
            let content_type = res
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|t: &hyper::header::HeaderValue| t.to_str().ok())
                .map(|s: &str| s.to_string());

            let content_length: Option<usize> = res
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|hv: &hyper::header::HeaderValue| hv.to_str().ok())
                .and_then(|s: &str| s.parse().ok());

            if let Some(len) = content_length {
                if len > MAX_BODY_BYTES {
                    return Err(HyperHttpError::BodyTooLarge);
                }
            }

            break (content_type, res.into_body());
        }

        // Handle redirect
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

        // Drain redirect response body before following redirect
        let redirect_body: Incoming = res.into_body();
        let _: http_body_util::Collected<Bytes> = BodyExt::collect(redirect_body)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        current_uri = resolve_redirect(&current_uri, &location)?;
        redirects += 1;
    };

    // Consume body and collect bytes with size limit check
    let collected = BodyExt::collect(body)
        .await
        .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

    let bytes = collected.to_bytes();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(HyperHttpError::BodyTooLarge);
    }

    Ok(HyperHttpResponse {
        content_type,
        bytes: bytes.to_vec(),
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
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
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
