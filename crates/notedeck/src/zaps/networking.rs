use crate::{
    error::EndpointError,
    zaps::{cache::PayCache, ZapAddress, ZapTargetOwned},
    ZapError,
};
use enostr::{NoteId, Pubkey};
use nostrdb::NoteBuilder;
use poll_promise::Promise;
use serde::Deserialize;
use tokio::task::JoinError;
use url::Url;

pub struct FetchedInvoice {
    pub invoice: String,
    pub request_noteid: NoteId, // note id of kind 9734 request
}

pub struct FetchedInvoiceResponse {
    pub invoice: Result<FetchedInvoice, ZapError>,
    pub pay_entry: Option<PayEntry>,
}

pub type FetchingInvoice = Promise<Result<FetchedInvoiceResponse, JoinError>>;

async fn fetch_pay_req_async(url: &Url) -> Result<LNUrlPayResponseRaw, ZapError> {
    let (sender, promise) = Promise::new();

    let on_done = move |response: Result<ehttp::Response, String>| {
        let handle = response.map_err(ZapError::endpoint_error).and_then(|resp| {
            if !resp.ok {
                return Err(ZapError::endpoint_error(format!(
                    "bad http response: {}",
                    resp.status_text
                )));
            }

            serde_json::from_slice(&resp.bytes).map_err(|e| ZapError::Serialization(e.to_string()))
        });

        sender.send(handle);
    };

    let request = ehttp::Request::get(url);
    ehttp::fetch(request, on_done);
    tokio::task::block_in_place(|| promise.block_and_take())
}

static HRP_LNURL: bech32::Hrp = bech32::Hrp::parse_unchecked("lnurl");

fn endpoint_url_to_lnurl(endpoint_url: &Url) -> Result<String, ZapError> {
    let url_str = endpoint_url.to_string();
    let data = url_str.as_bytes();

    bech32::encode::<bech32::Bech32>(HRP_LNURL, data).map_err(|e| ZapError::Bech(e.to_string()))
}

fn make_kind_9734<'a>(
    lnurl: &str,
    msats: u64,
    comment: &str,
    sender_nsec: &[u8; 32],
    relays: Vec<String>,
    target: ZapTargetOwned,
) -> nostrdb::Note<'a> {
    let mut builder = NoteBuilder::new().kind(9734);

    builder = builder.content(comment).start_tag().tag_str("relays");

    for relay in relays {
        builder = builder.tag_str(&relay)
    }

    builder = builder
        .start_tag()
        .tag_str("amount")
        .tag_str(&msats.to_string());

    builder = builder.start_tag().tag_str("lnurl").tag_str(lnurl);

    match target {
        ZapTargetOwned::Profile(pubkey) => {
            builder = builder.start_tag().tag_str("p").tag_str(&pubkey.hex());
        }

        ZapTargetOwned::Note(note_target) => {
            builder = builder
                .start_tag()
                .tag_str("p")
                .tag_str(&note_target.zap_recipient.hex());
            builder = builder
                .start_tag()
                .tag_str("e")
                .tag_str(&note_target.note_id.hex());
        }
    }

    builder.sign(sender_nsec).build().expect("note")
}

#[derive(Debug, Deserialize)]
pub struct LNUrlPayResponseRaw {
    #[allow(dead_code)]
    #[serde(rename = "allowsNostr")]
    allow_nostr: bool,

    #[allow(dead_code)]
    #[serde(rename = "nostrPubkey")]
    nostr_pubkey: String,

    #[serde(rename = "callback")]
    callback_url: String,

    #[allow(dead_code)]
    #[serde(rename = "minSendable")]
    min_sendable: u64,

    #[allow(dead_code)]
    #[serde(rename = "maxSendable")]
    max_sendable: u64,
}

impl From<LNUrlPayResponseRaw> for LNUrlPayResponse {
    fn from(value: LNUrlPayResponseRaw) -> Self {
        let nostr_pubkey = Pubkey::from_hex(&value.nostr_pubkey)
            .map_err(|e: enostr::Error| EndpointError(e.to_string()));

        let callback_url = Url::parse(&value.callback_url)
            .map_err(|e| EndpointError(format!("invalid callback url: {e}")));

        Self {
            allow_nostr: value.allow_nostr,
            nostr_pubkey,
            callback_url,
            min_sendable: value.min_sendable,
            max_sendable: value.max_sendable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LNUrlPayResponse {
    pub allow_nostr: bool,
    pub nostr_pubkey: Result<Pubkey, EndpointError>,
    pub callback_url: Result<Url, EndpointError>,
    pub min_sendable: u64,
    pub max_sendable: u64,
}

#[derive(Clone, Debug)]
pub struct PayEntry {
    pub url: Url,
    pub response: LNUrlPayResponse,
}

#[derive(Debug, Deserialize)]
struct LNInvoice {
    #[serde(rename = "pr")]
    invoice: String,
}

fn endpoint_query_for_invoice(
    endpoint_base_url: &Url,
    msats: u64,
    lnurl: &str,
    note: nostrdb::Note,
) -> Result<Url, ZapError> {
    let mut new_url = endpoint_base_url.clone();
    let nostr = note
        .json()
        .map_err(|e| ZapError::Serialization(format!("failed note to json: {e}")))?;

    new_url
        .query_pairs_mut()
        .append_pair("amount", &msats.to_string())
        .append_pair("lnurl", lnurl)
        .append_pair("nostr", &nostr)
        .finish();

    Ok(new_url)
}

pub fn fetch_invoice_promise(
    cache: &PayCache,
    zap_address: ZapAddress,
    msats: u64,
    sender_nsec: [u8; 32],
    target: ZapTargetOwned,
    relays: Vec<String>,
) -> Result<FetchingInvoice, ZapError> {
    let (url, lnurl) = match zap_address {
        ZapAddress::Lud16(lud16) => {
            let url = generate_endpoint_url(&lud16)?;
            let lnurl = endpoint_url_to_lnurl(&url)?;
            (url, lnurl)
        }
        ZapAddress::Lud06(lnurl) => (convert_lnurl_to_endpoint_url(&lnurl)?, lnurl),
    };

    match cache.get_response(&url) {
        Some(endpoint_resp) => {
            tracing::info!("Using existing endpoint response for {url}");
            let response = endpoint_resp.clone();
            Ok(Promise::spawn_async(tokio::spawn(async move {
                fetch_invoice_lnurl_async(
                    &lnurl,
                    PayEntry { url, response },
                    msats,
                    &sender_nsec,
                    relays,
                    target,
                )
                .await
            })))
        }
        None => Ok(Promise::spawn_async(tokio::spawn(async move {
            tracing::info!("querying ln endpoint: {url}");
            let pay_req = match fetch_pay_req_async(&url).await {
                Ok(p) => PayEntry {
                    url,
                    response: p.into(),
                },
                Err(e) => {
                    return FetchedInvoiceResponse {
                        invoice: Err(e),
                        pay_entry: None,
                    }
                }
            };

            fetch_invoice_lnurl_async(&lnurl, pay_req, msats, &sender_nsec, relays, target).await
        }))),
    }
}

fn convert_lnurl_to_endpoint_url(lnurl: &str) -> Result<Url, ZapError> {
    let (_, data) = bech32::decode(lnurl).map_err(|e| ZapError::Bech(e.to_string()))?;

    let url_str =
        String::from_utf8(data).map_err(|e| ZapError::Bech(format!("string conversion: {e}")))?;

    Url::parse(&url_str)
        .map_err(|e| ZapError::endpoint_error(format!("endpoint url from lnurl is invalid: {e}")))
}

async fn fetch_invoice_lnurl_async(
    lnurl: &str,
    pay_entry: PayEntry,
    msats: u64,
    sender_nsec: &[u8; 32],
    relays: Vec<String>,
    target: ZapTargetOwned,
) -> FetchedInvoiceResponse {
    let base_url = match &pay_entry.response.callback_url {
        Ok(url) => url.clone(),
        Err(error) => {
            return FetchedInvoiceResponse {
                invoice: Err(ZapError::EndpointError(error.clone())),
                pay_entry: None,
            };
        }
    };

    let (query, noteid) = {
        let comment: &str = "";
        let note = make_kind_9734(lnurl, msats, comment, sender_nsec, relays, target);
        let noteid = NoteId::new(*note.id());
        let query = match endpoint_query_for_invoice(&base_url, msats, lnurl, note) {
            Ok(u) => u,
            Err(e) => {
                return FetchedInvoiceResponse {
                    invoice: Err(e),
                    pay_entry: Some(pay_entry),
                }
            }
        };
        (query, noteid)
    };

    let res = fetch_ln_invoice(&query).await;
    FetchedInvoiceResponse {
        invoice: res.map(|r| FetchedInvoice {
            invoice: r.invoice,
            request_noteid: noteid,
        }),
        pay_entry: Some(pay_entry),
    }
}

async fn fetch_ln_invoice(req: &Url) -> Result<LNInvoice, ZapError> {
    let request = ehttp::Request::get(req);
    let (sender, promise) = Promise::new();
    let on_done = move |response: Result<ehttp::Response, String>| {
        let handle = response.map_err(ZapError::endpoint_error).and_then(|resp| {
            if !resp.ok {
                return Err(ZapError::endpoint_error(format!(
                    "invalid http response: {}",
                    resp.status_text
                )));
            }

            serde_json::from_slice(&resp.bytes).map_err(|e| ZapError::Serialization(e.to_string()))
        });

        sender.send(handle);
    };

    ehttp::fetch(request, on_done);

    tokio::task::block_in_place(|| promise.block_and_take())
}

fn generate_endpoint_url(lud16: &str) -> Result<Url, ZapError> {
    let (user, domain, use_http) = {
        let mut split = lud16.split('@');
        let user = split
            .next()
            .ok_or_else(|| ZapError::InvalidLud16("lud16 did not have username".to_owned()))?;

        let domain = split
            .next()
            .ok_or_else(|| ZapError::InvalidLud16("lud16 did not have domain".to_owned()))?;

        let mut domain_split = domain.split('.');

        let _ = domain_split
            .next()
            .ok_or_else(|| ZapError::InvalidLud16("lud16 domain is invalid".to_owned()))?;

        let tld = domain_split.next().ok_or_else(|| {
            ZapError::InvalidLud16("lud16 domain does not include tld".to_owned())
        })?;

        let use_http = tld == "onion";

        (user, domain, use_http)
    };

    let url_str = format!(
        "http{}://{domain}/.well-known/lnurlp/{user}",
        if use_http { "" } else { "s" }
    );

    Url::parse(&url_str).map_err(|e| ZapError::endpoint_error(e.to_string()))
}

#[cfg(test)]
mod tests {
    use enostr::{FullKeypair, NoteId};

    use crate::zaps::{
        cache::PayCache,
        networking::{
            convert_lnurl_to_endpoint_url, endpoint_url_to_lnurl, fetch_pay_req_async,
            generate_endpoint_url,
        },
    };

    use super::fetch_invoice_promise;

    #[ignore] // don't run this test automatically since it sends real http
    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_pay_req() {
        let lud16 = "jb55@sendsats.lol";

        let url = generate_endpoint_url(lud16);
        assert!(url.is_ok());

        let maybe_res = fetch_pay_req_async(&url.unwrap()).await;

        assert!(maybe_res.is_ok());

        let res = maybe_res.unwrap();

        assert!(res.allow_nostr);
        assert_eq!(
            res.nostr_pubkey,
            "9630f464cca6a5147aa8a35f0bcdd3ce485324e732fd39e09233b1d848238f31"
        );
        assert_eq!(res.callback_url, "https://sendsats.lol/@jb55");
        assert_eq!(res.min_sendable, 1);
        assert_eq!(res.max_sendable, 10000000000);
    }

    #[test]
    fn test_lnurl() {
        let lud16 = "jb55@sendsats.lol";

        let url = generate_endpoint_url(lud16);
        assert!(url.is_ok());

        let maybe_lnurl = endpoint_url_to_lnurl(&url.unwrap());
        assert!(maybe_lnurl.is_ok());

        let lnurl = maybe_lnurl.unwrap();
        assert_eq!(
            lnurl,
            "lnurl1dp68gurn8ghj7um9dej8xct5wvhxcmmv9uh8wetvdskkkmn0wahz7mrww4excup0df3r2dg3mj444"
        );
    }

    #[ignore] // don't run test automatically since it sends real http
    #[test]
    fn test_generate_invoice() {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");

        let kp = FullKeypair::generate();
        let mut cache = PayCache::default();
        let maybe_invoice = rt.block_on(async {
            fetch_invoice_promise(
                &mut cache,
                crate::zaps::ZapAddress::Lud16("jb55@sendsats.lol".to_owned()),
                1000,
                FullKeypair::generate().secret_key.to_secret_bytes(),
                crate::zaps::ZapTargetOwned::Note(crate::NoteZapTargetOwned {
                    note_id: NoteId::new([0; 32]),
                    zap_recipient: kp.pubkey,
                }),
                vec!["wss://relay.damus.io".to_owned()],
            )
            .map(|p| p.block_and_take())
        });

        assert!(maybe_invoice.is_ok());
        let inner = maybe_invoice.unwrap();
        assert!(inner.is_ok());
        let inner = inner.unwrap().invoice;
        assert!(inner.is_ok());

        let inner = inner.unwrap();

        assert!(inner.invoice.starts_with("lnbc"));
    }

    #[test]
    fn test_convert_lnurl() {
        let lnurl =
            "lnurl1dp68gurn8ghj7um9dej8xct5wvhxcmmv9uh8wetvdskkkmn0wahz7mrww4excup0df3r2dg3mj444";

        let maybe_url = convert_lnurl_to_endpoint_url(lnurl);
        println!("{:?}", maybe_url);
        assert!(maybe_url.is_ok());
    }

    #[ignore]
    #[test]
    fn test_generate_lnurl_invoice() {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
        let lnurl =
            "lnurl1dp68gurn8ghj7um9dej8xct5wvhxcmmv9uh8wetvdskkkmn0wahz7mrww4excup0df3r2dg3mj444";
        let relay = "wss://relay.damus.io";

        let kp = FullKeypair::generate();

        let mut cache = PayCache::default();
        let maybe_invoice = rt.block_on(async {
            fetch_invoice_promise(
                &mut cache,
                crate::zaps::ZapAddress::Lud06(lnurl.to_owned()),
                1000,
                kp.secret_key.to_secret_bytes(),
                crate::zaps::ZapTargetOwned::Note(crate::NoteZapTargetOwned {
                    note_id: NoteId::new([0; 32]),
                    zap_recipient: kp.pubkey,
                }),
                [relay.to_owned()].to_vec(),
            )
            .map(|p| p.block_and_take())
        });

        assert!(maybe_invoice.is_ok());
        let inner = maybe_invoice.unwrap();
        assert!(inner.is_ok());
        let inner = inner.unwrap().invoice;
        assert!(inner.is_ok());

        let inner = inner.unwrap();

        assert!(inner.invoice.starts_with("lnbc"));
    }
}
