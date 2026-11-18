//! The Web toolset's tools, as Dave runs them: `web_search` and `web_fetch`
//! (declared in [`agentium_core::agent_spec::web`]).
//!
//! Only a spec with no private data is offered these (a sub-agent, or a
//! spec of only web tools; [`AgentSpec::validate`] refuses one beside
//! private data). What that leaves to guard is the network the host itself
//! can reach: a page that tells the agent to fetch `http://127.0.0.1:…`
//! would read the user's local services. So a fetch goes only to a public
//! http(s) URL, checked on every redirect hop too
//! ([`HyperHttpClient::req_guarded`]), and connects only to public
//! addresses as the name *resolves* ([`HyperHttpClient::with_addr_check`]):
//! the URL check reads the host as written, which a name pointing at
//! `127.0.0.1`, or an address in a form only the system resolver reads
//! (`127.1`, `0x7f.0.0.1`), would get past.
//!
//! ## Search is a SearXNG instance
//!
//! `web_search` asks the SearXNG instance named by [`SEARCH_URL_VAR`], through
//! its JSON API (`/search?q=…&format=json`). SearXNG takes no key, can be run
//! by the user, and answers every engine it fronts in one shape. With the
//! variable unset, a search answers an error that says so; fetching still
//! works.
//!
//! [`AgentSpec::validate`]: agentium_core::agent_spec::AgentSpec::validate

use std::net::IpAddr;

use agentium_core::agent_spec::web::{WEB_FETCH, WEB_SEARCH};
use hyper::Uri;
use notedeck::network::HyperHttpClient;
use serde_json::{json, Value};

use crate::tool_round::{block_on_own_runtime, ToolJob, CANCELLED};
use crate::tools::{AppToolCall, AppToolResponse};
use crate::ToolResponses;

/// The process env var naming the SearXNG instance `web_search` asks, e.g.
/// `https://searx.example.org`.
pub(crate) const SEARCH_URL_VAR: &str = "NOTEDECK_SEARXNG_URL";

/// Most characters of a page's text a fetch answers; the rest is cut, and
/// the answer says so.
const MAX_TEXT_CHARS: usize = 20_000;

/// Results a search answers when the call names no count, and the most it
/// may name.
const DEFAULT_RESULTS: usize = 8;
const MAX_RESULTS: usize = 10;

/// Runs web tool calls. Build one per job: its HTTP client's connections
/// belong to the runtime the job runs on.
pub(crate) struct WebClient {
    /// Asks the search instance, which may be local.
    http: HyperHttpClient,
    /// Fetches pages: connects only to public addresses.
    public_http: HyperHttpClient,
    /// The SearXNG instance, from [`SEARCH_URL_VAR`].
    search_url: Option<String>,
}

impl WebClient {
    /// A client searching the instance `search_url`, if any.
    pub(crate) fn new(search_url: Option<String>) -> Self {
        Self {
            http: HyperHttpClient::new(),
            public_http: HyperHttpClient::with_addr_check(is_public),
            search_url: search_url.filter(|url| !url.trim().is_empty()),
        }
    }

    /// Run the web tool `call`: its answer for the model, or an error.
    pub(crate) async fn call(&self, call: &AppToolCall) -> Result<Value, String> {
        let arg = |name: &str| call.args.get(name).and_then(Value::as_str);
        match call.name.as_str() {
            WEB_SEARCH => {
                let query = arg("query").ok_or("web_search needs a query")?;
                let count = call
                    .args
                    .get("count")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_RESULTS, |n| (n as usize).clamp(1, MAX_RESULTS));
                self.search(query, count).await
            }
            WEB_FETCH => self.fetch(arg("url").ok_or("web_fetch needs a url")?).await,
            other => Err(format!("'{other}' isn't a web tool")),
        }
    }

    /// Ask the SearXNG instance for `query`'s top `count` results.
    async fn search(&self, query: &str, count: usize) -> Result<Value, String> {
        let Some(base) = &self.search_url else {
            return Err(format!(
                "web search isn't set up on this host: set {SEARCH_URL_VAR} to a SearXNG \
                 instance. web_fetch still works if you know a URL."
            ));
        };
        let url = search_request_url(base, query)?;
        // The instance is the user's own choice and may well be local, so
        // the public-URL check doesn't apply to it.
        let response = self
            .http
            .req_guarded(&url, "application/json", &|_| Ok(()))
            .await
            .map_err(|err| format!("search failed: {}", err.with_causes()))?;
        if response.status_code >= 400 {
            return Err(format!("search failed: HTTP {}", response.status_code));
        }
        let body: Value = serde_json::from_slice(&response.bytes)
            .map_err(|err| format!("the search instance didn't answer JSON: {err}"))?;
        Ok(json!({ "query": query, "results": search_results(&body, count) }))
    }

    /// Fetch the public URL `url` and answer its text.
    async fn fetch(&self, url: &str) -> Result<Value, String> {
        let uri: Uri = url.parse().map_err(|_| format!("not a URL: {url}"))?;
        public_url(&uri)?;
        let response = self
            .public_http
            .req_guarded(
                url,
                "text/html,text/plain,application/json;q=0.9,*/*;q=0.5",
                &|uri| public_url(uri),
            )
            .await
            .map_err(|err| format!("fetch failed: {}", err.with_causes()))?;
        if response.status_code >= 400 {
            return Err(format!("fetch failed: HTTP {}", response.status_code));
        }
        let content_type = response.content_type.as_deref().unwrap_or("");
        let body = String::from_utf8_lossy(&response.bytes);
        let text = match page_kind(content_type, &body) {
            PageKind::Html => html_to_text(&body),
            PageKind::Text => body.into_owned(),
            PageKind::Other => {
                return Err(format!(
                    "{url} is {content_type}, not a page web_fetch can read"
                ))
            }
        };
        let cut = truncate_chars(text, MAX_TEXT_CHARS);
        Ok(json!({
            "url": url,
            "content_type": content_type,
            "text": cut.text,
            "truncated": cut.truncated,
        }))
    }
}

/// The job that runs the web tool `call` off the frame, searching
/// `search_url`; the round's stop ends it.
pub(crate) fn job(call: AppToolCall, search_url: Option<String>) -> ToolJob {
    Box::new(move |ctx| {
        let value = block_on_own_runtime(async {
            let web = WebClient::new(search_url);
            tokio::select! {
                value = web.call(&call) => value,
                () = ctx.cancel().cancelled() => Err(CANCELLED.to_owned()),
            }
        })?;
        Ok(ToolResponses::App(AppToolResponse {
            tool: call.name,
            value,
        }))
    })
}

/// `base`'s JSON search endpoint for `query`.
fn search_request_url(base: &str, query: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base)
        .map_err(|err| format!("{SEARCH_URL_VAR} isn't a URL ({base}): {err}"))?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    let mut url = url
        .join("search")
        .map_err(|err| format!("{SEARCH_URL_VAR} isn't a base URL: {err}"))?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json");
    Ok(url.into())
}

/// The first `count` results of a SearXNG JSON answer, as the model gets
/// them: each one's title, URL and snippet.
fn search_results(body: &Value, count: usize) -> Vec<Value> {
    let Some(results) = body.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let field = |result: &Value, name: &str| {
        result
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    results
        .iter()
        .filter(|r| r.get("url").and_then(Value::as_str).is_some())
        .take(count)
        .map(|r| {
            json!({
                "title": field(r, "title"),
                "url": field(r, "url"),
                "snippet": field(r, "content"),
            })
        })
        .collect()
}

/// Refuse a URL that isn't public http(s): another scheme, a loopback,
/// private, link-local or otherwise local address, an IPv4 address written
/// in any form but `a.b.c.d`, or a name that only means something on this
/// machine or network.
///
/// Names are checked as written, not resolved: a public name that resolves
/// to a private address passes here, and is refused by the fetch client's
/// address check when it connects ([`HyperHttpClient::with_addr_check`]).
fn public_url(uri: &Uri) -> Result<(), String> {
    match uri.scheme_str() {
        Some("http" | "https") => {}
        _ => {
            return Err(format!(
                "only http and https URLs can be fetched, not {uri}"
            ))
        }
    }
    let host = uri
        .host()
        .ok_or_else(|| format!("{uri} names no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    if host.parse::<IpAddr>().is_err() && numeric_label(host.rsplit('.').next().unwrap_or("")) {
        // The system resolver reads `127.1`, `0x7f.0.0.1` or `2130706433`
        // as an address, so it can't be checked as a name. No public name
        // ends in a numeric label.
        return Err(format!(
            "{host} is an IPv4 address in a non-standard form; write it as a.b.c.d"
        ));
    }
    let local_name = host == "localhost"
        || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        || !host.contains('.') && host.parse::<IpAddr>().is_err();
    if local_name {
        return Err(format!("{host} is a local name, not a public site"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public(ip) {
            return Err(format!("{ip} is a local or private address"));
        }
    }
    Ok(())
}

/// Whether `label` is a number as `inet_aton` reads one: decimal, octal
/// or `0x` hex digits.
fn numeric_label(label: &str) -> bool {
    match label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"))
    {
        Some(hex) => hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => !label.is_empty() && label.chars().all(|c| c.is_ascii_digit()),
    }
}

/// Whether `ip` is reachable on the public internet, as opposed to this
/// machine, a private network, or a reserved range.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // Carrier-grade NAT, 100.64.0.0/10: Tailscale lives here.
                || (a == 100 && (64..128).contains(&b))
                || a == 0)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local, fc00::/7.
                || (first & 0xfe00) == 0xfc00
                // Link-local, fe80::/10.
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// How a fetched body is read.
#[derive(Debug, PartialEq, Eq)]
enum PageKind {
    /// Reduced to its readable text.
    Html,
    /// Answered as it is.
    Text,
    /// Not text: an image, a PDF, an archive.
    Other,
}

/// What `body`, served as `content_type`, is. A body served with no type
/// is read by a look at its start.
fn page_kind(content_type: &str, body: &str) -> PageKind {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "text/html" | "application/xhtml+xml" => PageKind::Html,
        "" if body.trim_start().starts_with('<') => PageKind::Html,
        "" => PageKind::Text,
        m if m.starts_with("text/") || m.ends_with("json") || m.ends_with("xml") => PageKind::Text,
        _ => PageKind::Other,
    }
}

/// Elements whose content is never text a reader sees.
const SKIPPED: [&str; 6] = ["script", "style", "noscript", "svg", "template", "head"];

/// Elements that start a new line in the text.
const BLOCKS: [&str; 22] = [
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
];

/// The readable text of an HTML page: the tags dropped, scripts and styles
/// with their content, block elements on lines of their own, the common
/// entities decoded, and whitespace collapsed. Not a parser; good enough
/// for a model to read a page.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if !closing && SKIPPED.contains(&name.as_str()) && !tag.ends_with('/') {
            rest = skip_element(rest, &name);
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            out.push('\n');
        } else {
            out.push(' ');
        }
    }
    push_text(&mut out, rest);
    collapse_whitespace(&out)
}

/// `rest` past the end tag of the `name` element it is inside.
fn skip_element<'a>(rest: &'a str, name: &str) -> &'a str {
    let close = format!("</{name}");
    let lower = rest.to_ascii_lowercase();
    match lower.find(&close) {
        Some(at) => {
            let after = &rest[at..];
            after.find('>').map_or("", |gt| &after[gt + 1..])
        }
        None => "",
    }
}

/// Push `text` with its entities decoded.
fn push_text(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| Some((decode_entity(&rest[1..semi])?, semi)));
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

/// The character an entity's name (between `&` and `;`) stands for.
fn decode_entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "rsquo" | "lsquo" => '\'',
        "rdquo" | "ldquo" => '"',
        _ => return None,
    })
}

/// Collapse each run of whitespace in a line to one space, trim every line,
/// and drop the empty ones.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut words = line.split_whitespace().peekable();
        if words.peek().is_none() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        for (i, word) in words.enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(word);
        }
    }
    out
}

/// A page's text, cut to the length a fetch answers.
#[derive(Debug, PartialEq, Eq)]
struct Cut {
    text: String,
    /// Whether anything was cut off.
    truncated: bool,
}

/// `text` cut to at most `max` characters.
fn truncate_chars(mut text: String, max: usize) -> Cut {
    let truncated = match text.char_indices().nth(max) {
        Some((at, _)) => {
            text.truncate(at);
            true
        }
        None => false,
    };
    Cut { text, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `url` may be fetched: it parses, and it is public.
    fn fetchable(url: &str) -> bool {
        url.parse::<Uri>().is_ok_and(|uri| public_url(&uri).is_ok())
    }

    /// Public sites pass; the machine, its networks and other schemes
    /// don't, by address or by name.
    #[test]
    fn only_public_http_urls_are_fetched() {
        for ok in [
            "https://example.com/pricing",
            "http://93.184.216.34/",
            "https://[2606:2800:220:1::]/",
        ] {
            assert!(fetchable(ok), "{ok}");
        }
        for refused in [
            "http://127.0.0.1:6677/",
            "http://localhost:11434/v1",
            "http://LOCALHOST./",
            "http://10.0.0.5/",
            "http://192.168.1.1/admin",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.101.102.103/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            // IPv4 forms only the system resolver reads as addresses.
            "http://127.1:6677/",
            "http://0x7f.0.0.1/",
            "http://0177.0.0.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://10.1/",
            "http://169.254.43518/",
            "http://printer.local/",
            "http://nas/",
            "ftp://example.com/file",
            "file:///etc/passwd",
        ] {
            assert!(!fetchable(refused), "{refused}");
        }
    }

    /// The fetch client checks where a name resolves, not how it is
    /// written: with the URL check off, a name that resolves locally still
    /// never reaches a local address. Nothing listens on the port; the
    /// refusal comes before any connect.
    #[tokio::test]
    async fn the_fetch_client_refuses_names_that_resolve_locally() {
        let web = WebClient::new(None);
        for url in ["http://localhost:9/", "http://127.1:9/", "http://0x7f.1:9/"] {
            let err = web
                .public_http
                .req_guarded(url, "*/*", &|_| Ok(()))
                .await
                .expect_err(url)
                .with_causes();
            assert!(err.contains("refused addresses"), "{url}: {err}");
        }
    }

    #[test]
    fn a_page_reduces_to_its_readable_text() {
        let html = r#"<!doctype html><html><head><title>Acme</title>
            <style>body { color: red }</style></head>
            <body><nav><a href="/">Home</a></nav>
            <!-- a comment <p>hidden</p> -->
            <h1>Acme&nbsp;Widgets</h1>
            <p>Plans from <b>$9</b>/mo &amp; up.</p>
            <script>var x = "<p>not text</p>";</script>
            <ul><li>Fast</li><li>Cheap &#8212; really</li></ul></body></html>"#;
        assert_eq!(
            html_to_text(html),
            "Home\nAcme Widgets\nPlans from $9 /mo & up.\nFast\nCheap — really"
        );
    }

    #[test]
    fn a_body_is_read_by_its_type() {
        assert_eq!(page_kind("text/html; charset=utf-8", ""), PageKind::Html);
        assert_eq!(page_kind("application/json", "{}"), PageKind::Text);
        assert_eq!(page_kind("text/plain", "hi"), PageKind::Text);
        assert_eq!(page_kind("", "  <html>"), PageKind::Html);
        assert_eq!(page_kind("application/pdf", "%PDF"), PageKind::Other);
        assert_eq!(page_kind("image/png", ""), PageKind::Other);
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let cut = |text: &str, truncated| Cut {
            text: text.into(),
            truncated,
        };
        assert_eq!(truncate_chars("héllo".into(), 2), cut("hé", true));
        assert_eq!(truncate_chars("hi".into(), 2), cut("hi", false));
    }

    #[test]
    fn a_search_goes_to_the_instances_json_endpoint() {
        assert_eq!(
            search_request_url("https://searx.example.org", "idea journal app").unwrap(),
            "https://searx.example.org/search?q=idea+journal+app&format=json"
        );
        assert_eq!(
            search_request_url("http://localhost:8888/searx/", "a&b").unwrap(),
            "http://localhost:8888/searx/search?q=a%26b&format=json"
        );
    }

    /// The model gets each result's title, URL and snippet, as many as it
    /// asked for; a result with no URL is skipped.
    #[test]
    fn search_results_keep_title_url_and_snippet() {
        let body = json!({
            "query": "q",
            "results": [
                { "title": "A", "url": "https://a.example", "content": "about a", "engine": "ddg" },
                { "title": "no url" },
                { "title": "B", "url": "https://b.example" },
                { "title": "C", "url": "https://c.example", "content": "about c" }
            ]
        });
        assert_eq!(
            search_results(&body, 2),
            [
                json!({ "title": "A", "url": "https://a.example", "snippet": "about a" }),
                json!({ "title": "B", "url": "https://b.example", "snippet": "" }),
            ]
        );
        assert!(search_results(&json!({}), 5).is_empty());
    }

    /// With no instance set, a search says how to set one, and runs nothing.
    #[tokio::test]
    async fn a_search_with_no_instance_says_how_to_set_one() {
        notedeck::enostr::install_crypto();
        let client = WebClient::new(None);
        let call = AppToolCall {
            name: WEB_SEARCH.into(),
            args: json!({ "query": "x" }),
        };
        let err = client.call(&call).await.unwrap_err();
        assert!(err.contains(SEARCH_URL_VAR), "{err}");
    }
}
