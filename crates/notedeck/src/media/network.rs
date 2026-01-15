use std::{error::Error, fmt};

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use http_body_util::{BodyExt, Empty};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use hyper::{
    body::Bytes,
    header::{self},
    Request, Uri,
};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use hyper_rustls::HttpsConnectorBuilder;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use tokio_socks::tcp::Socks5Stream;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use url::Url;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

/// Fetch HTTP resource, optionally routing through SOCKS proxy.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub async fn http_fetch(
    url: &str,
    socks_proxy: Option<&str>,
) -> Result<HyperHttpResponse, HyperHttpError> {
    if let Some(proxy) = socks_proxy {
        http_req_via_socks(url, proxy).await
    } else {
        http_req(url).await
    }
}

/// Stub for Android/WASM
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn http_fetch(
    url: &str,
    _socks_proxy: Option<&str>,
) -> Result<HyperHttpResponse, HyperHttpError> {
    // On Android/WASM, SOCKS proxy not yet supported for HTTP
    http_req(url).await
}

/// Desktop implementation using hyper
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
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
        content_type,
        bytes,
    })
}

/// Desktop implementation using SOCKS5 proxy for Tor routing
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub async fn http_req_via_socks(
    url: &str,
    socks_proxy: &str,
) -> Result<HyperHttpResponse, HyperHttpError> {
    use std::sync::Arc;
    use tokio_rustls::TlsConnector;

    let parsed_url = Url::parse(url).map_err(|_| HyperHttpError::Uri)?;
    let host = parsed_url
        .host_str()
        .ok_or(HyperHttpError::Host)?
        .to_string();
    let is_https = parsed_url.scheme() == "https";
    let port = parsed_url.port().unwrap_or(if is_https { 443 } else { 80 });
    let path = if parsed_url.query().is_some() {
        format!("{}?{}", parsed_url.path(), parsed_url.query().unwrap())
    } else {
        parsed_url.path().to_string()
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };

    const MAX_REDIRECTS: usize = 5;
    let mut redirects = 0;
    let mut current_host = host;
    let mut current_port = port;
    let mut current_path = path;
    let mut current_is_https = is_https;

    loop {
        // Connect through SOCKS proxy
        let stream = Socks5Stream::connect(socks_proxy, (current_host.as_str(), current_port))
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        let mut stream = stream.into_inner();

        // If HTTPS, wrap with TLS
        if current_is_https {
            let mut root_store = rustls::RootCertStore::empty();
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            let config = rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            let connector = TlsConnector::from(Arc::new(config));
            let server_name = rustls::pki_types::ServerName::try_from(current_host.clone())
                .map_err(|_| HyperHttpError::Host)?;

            let mut tls_stream = connector
                .connect(server_name, stream)
                .await
                .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

            let (content_type, bytes, redirect) =
                do_http_request(&mut tls_stream, &current_host, &current_path).await?;

            if let Some(location) = redirect {
                if redirects >= MAX_REDIRECTS {
                    return Err(HyperHttpError::TooManyRedirects);
                }
                let (new_host, new_port, new_path, new_is_https) = parse_redirect_location(
                    &location,
                    &current_host,
                    current_port,
                    current_is_https,
                )?;
                current_host = new_host;
                current_port = new_port;
                current_path = new_path;
                current_is_https = new_is_https;
                redirects += 1;
                continue;
            }

            return Ok(HyperHttpResponse {
                content_type,
                bytes,
            });
        } else {
            let (content_type, bytes, redirect) =
                do_http_request(&mut stream, &current_host, &current_path).await?;

            if let Some(location) = redirect {
                if redirects >= MAX_REDIRECTS {
                    return Err(HyperHttpError::TooManyRedirects);
                }
                let (new_host, new_port, new_path, new_is_https) = parse_redirect_location(
                    &location,
                    &current_host,
                    current_port,
                    current_is_https,
                )?;
                current_host = new_host;
                current_port = new_port;
                current_path = new_path;
                current_is_https = new_is_https;
                redirects += 1;
                continue;
            }

            return Ok(HyperHttpResponse {
                content_type,
                bytes,
            });
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn do_http_request<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
    host: &str,
    path: &str,
) -> Result<(Option<String>, Vec<u8>, Option<String>), HyperHttpError> {
    // Send HTTP request
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: notedeck/1.0\r\n\r\n",
        path, host
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

    // Read response
    let mut reader = BufReader::new(stream);

    // Parse status line
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Parse headers
    let mut content_type = None;
    let mut content_length: Option<usize> = None;
    let mut location = None;
    let mut is_chunked = false;

    loop {
        let mut header_line = String::new();
        reader
            .read_line(&mut header_line)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        if header_line == "\r\n" || header_line.is_empty() {
            break;
        }

        let header_lower = header_line.to_lowercase();
        if header_lower.starts_with("content-type:") {
            content_type = Some(header_line[13..].trim().to_string());
        } else if header_lower.starts_with("content-length:") {
            content_length = header_line[15..].trim().parse().ok();
        } else if header_lower.starts_with("location:") {
            location = Some(header_line[9..].trim().to_string());
        } else if header_lower.starts_with("transfer-encoding:") && header_lower.contains("chunked")
        {
            is_chunked = true;
        }
    }

    // Handle redirects
    if (300..400).contains(&status_code) {
        return Ok((content_type, Vec::new(), location));
    }

    // Check content length
    if let Some(len) = content_length {
        if len > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }
    }

    // Read body
    let bytes = if is_chunked {
        read_chunked_body(&mut reader).await?
    } else if let Some(len) = content_length {
        let mut bytes = vec![0u8; len];
        reader
            .read_exact(&mut bytes)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;
        bytes
    } else {
        // Read until EOF
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }
        bytes
    };

    Ok((content_type, bytes, None))
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
async fn read_chunked_body<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, HyperHttpError> {
    let mut body = Vec::new();

    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;

        let chunk_size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);

        if chunk_size == 0 {
            break;
        }

        if body.len() + chunk_size > MAX_BODY_BYTES {
            return Err(HyperHttpError::BodyTooLarge);
        }

        let mut chunk = vec![0u8; chunk_size];
        reader
            .read_exact(&mut chunk)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;
        body.extend_from_slice(&chunk);

        // Read trailing \r\n
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .await
            .map_err(|e| HyperHttpError::Hyper(Box::new(e)))?;
    }

    Ok(body)
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn parse_redirect_location(
    location: &str,
    current_host: &str,
    current_port: u16,
    current_is_https: bool,
) -> Result<(String, u16, String, bool), HyperHttpError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        let parsed = Url::parse(location).map_err(|_| HyperHttpError::InvalidRedirectLocation)?;
        let host = parsed.host_str().ok_or(HyperHttpError::Host)?.to_string();
        let is_https = parsed.scheme() == "https";
        let port = parsed.port().unwrap_or(if is_https { 443 } else { 80 });
        let path = if parsed.query().is_some() {
            format!("{}?{}", parsed.path(), parsed.query().unwrap())
        } else {
            parsed.path().to_string()
        };
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            path
        };
        Ok((host, port, path, is_https))
    } else {
        // Relative redirect
        let path = if location.starts_with('/') {
            location.to_string()
        } else {
            format!("/{}", location)
        };
        Ok((
            current_host.to_string(),
            current_port,
            path,
            current_is_https,
        ))
    }
}

/// Android/WASM stub for SOCKS HTTP requests
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn http_req_via_socks(
    _url: &str,
    _socks_proxy: &str,
) -> Result<HyperHttpResponse, HyperHttpError> {
    Err(HyperHttpError::Unsupported)
}

#[derive(Debug)]
pub enum HyperHttpError {
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    Hyper(Box<dyn std::error::Error + Send + Sync>),
    Host,
    Uri,
    BodyTooLarge,
    TooManyRedirects,
    MissingRedirectLocation,
    InvalidRedirectLocation,
    /// HTTP requests not supported on this platform
    Unsupported,
}

#[derive(Debug)]
pub struct HyperHttpResponse {
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl Error for HyperHttpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            Self::Hyper(e) => Some(&**e),
            _ => None,
        }
    }
}

impl fmt::Display for HyperHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
            Self::Hyper(e) => write!(f, "Hyper error: {e}"),
            Self::Host => write!(f, "Missing host in URL"),
            Self::Uri => write!(f, "Invalid URI"),
            Self::BodyTooLarge => write!(f, "Body too large"),
            Self::TooManyRedirects => write!(f, "Too many redirect responses"),
            Self::MissingRedirectLocation => write!(f, "Redirect response missing Location header"),
            Self::InvalidRedirectLocation => write!(f, "Invalid redirect Location header"),
            Self::Unsupported => write!(f, "HTTP requests not supported on this platform"),
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
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

/// Android/WASM stub - HTTP requests not supported via hyper on these platforms
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn http_req(_url: &str) -> Result<HyperHttpResponse, HyperHttpError> {
    Err(HyperHttpError::Unsupported)
}
