use crate::{ZapError, zaps::ZapTargetOwned};
use enostr::NoteId;
use nostrdb::NoteBuilder;
use poll_promise::Promise;
use serde::Deserialize;
use tokio::task::JoinError;
use url::Url;

pub struct FetchedInvoice {
    pub invoice: String,
    pub request_noteid: NoteId, // note id of kind 9734 request
}

pub type FetchingInvoice = Promise<Result<Result<FetchedInvoice, ZapError>, JoinError>>;

async fn fetch_pay_req_async(url: &Url) -> Result<LNUrlPayRequest, ZapError> {
    let (sender, promise) = Promise::new();

    let on_done = move |response: Result<ehttp::Response, String>| {
        let handle = response.map_err(ZapError::EndpointError).and_then(|resp| {
            if !resp.ok {
                return Err(ZapError::EndpointError(format!(
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

async fn fetch_pay_req_from_lud16(lud16: &str) -> Result<LNUrlPayRequest, ZapError> {
    let url = match generate_endpoint_url(lud16) {
        Ok(url) => url,
        Err(e) => return Err(e),
    };

    fetch_pay_req_async(&url).await
}

static HRP_LNURL: bech32::Hrp = bech32::Hrp::parse_unchecked("lnurl");

fn lud16_to_lnurl(lud16: &str) -> Result<String, ZapError> {
    let endpoint_url = generate_endpoint_url(lud16)?;

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
pub struct LNUrlPayRequest {
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

#[derive(Debug, Deserialize)]
struct LNInvoice {
    #[serde(rename = "pr")]
    invoice: String,
}

fn endpoint_query_for_invoice<'a>(
    endpoint_base_url: &'a mut Url,
    msats: u64,
    lnurl: &str,
    note: nostrdb::Note,
) -> Result<&'a Url, ZapError> {
    let nostr = note
        .json()
        .map_err(|e| ZapError::Serialization(format!("failed note to json: {e}")))?;

    Ok(endpoint_base_url
        .query_pairs_mut()
        .append_pair("amount", &msats.to_string())
        .append_pair("lnurl", lnurl)
        .append_pair("nostr", &nostr)
        .finish())
}

pub fn fetch_invoice_lud16(
    lud16: String,
    msats: u64,
    sender_nsec: [u8; 32],
    target: ZapTargetOwned,
    relays: Vec<String>,
) -> FetchingInvoice {
    Promise::spawn_async(tokio::spawn(async move {
        fetch_invoice_lud16_async(&lud16, msats, &sender_nsec, target, relays).await
    }))
}

pub fn fetch_invoice_lnurl(
    lnurl: String,
    msats: u64,
    sender_nsec: [u8; 32],
    target: ZapTargetOwned,
    relays: Vec<String>,
) -> FetchingInvoice {
    Promise::spawn_async(tokio::spawn(async move {
        let pay_req = match fetch_pay_req_from_lnurl_async(&lnurl).await {
            Ok(req) => req,
            Err(e) => return Err(e),
        };

        fetch_invoice_lnurl_async(&lnurl, &pay_req, msats, &sender_nsec, relays, target).await
    }))
}

fn convert_lnurl_to_endpoint_url(lnurl: &str) -> Result<Url, ZapError> {
    let (_, data) = bech32::decode(lnurl).map_err(|e| ZapError::Bech(e.to_string()))?;

    let url_str =
        String::from_utf8(data).map_err(|e| ZapError::Bech(format!("string conversion: {e}")))?;

    Url::parse(&url_str)
        .map_err(|e| ZapError::EndpointError(format!("endpoint url from lnurl is invalid: {e}")))
}

async fn fetch_pay_req_from_lnurl_async(lnurl: &str) -> Result<LNUrlPayRequest, ZapError> {
    let url = match convert_lnurl_to_endpoint_url(lnurl) {
        Ok(u) => u,
        Err(e) => return Err(e),
    };

    fetch_pay_req_async(&url).await
}

async fn fetch_invoice_lnurl_async(
    lnurl: &str,
    pay_req: &LNUrlPayRequest,
    msats: u64,
    sender_nsec: &[u8; 32],
    relays: Vec<String>,
    target: ZapTargetOwned,
) -> Result<FetchedInvoice, ZapError> {
    //let recipient = Pubkey::from_hex(&pay_req.nostr_pubkey)
    //.map_err(|e| ZapError::EndpointError(format!("invalid pubkey hex from endpoint: {e}")))?;

    let mut base_url = Url::parse(&pay_req.callback_url)
        .map_err(|e| ZapError::EndpointError(format!("invalid callback url from endpoint: {e}")))?;

    let (query, noteid) = {
        let comment: &str = "";
        let note = make_kind_9734(lnurl, msats, comment, sender_nsec, relays, target);
        let noteid = NoteId::new(*note.id());
        let query = endpoint_query_for_invoice(&mut base_url, msats, lnurl, note)?;
        (query, noteid)
    };

    let res = fetch_invoice(query).await;
    res.map(|i| FetchedInvoice {
        invoice: i.invoice,
        request_noteid: noteid,
    })
}

async fn fetch_invoice_lud16_async(
    lud16: &str,
    msats: u64,
    sender_nsec: &[u8; 32],
    target: ZapTargetOwned,
    relays: Vec<String>,
) -> Result<FetchedInvoice, ZapError> {
    let pay_req = fetch_pay_req_from_lud16(lud16).await?;

    let lnurl = lud16_to_lnurl(lud16)?;

    fetch_invoice_lnurl_async(&lnurl, &pay_req, msats, sender_nsec, relays, target).await
}

async fn fetch_invoice(req: &Url) -> Result<LNInvoice, ZapError> {
    let request = ehttp::Request::get(req);
    let (sender, promise) = Promise::new();
    let on_done = move |response: Result<ehttp::Response, String>| {
        let handle = response.map_err(ZapError::EndpointError).and_then(|resp| {
            if !resp.ok {
                return Err(ZapError::EndpointError(format!(
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

    Url::parse(&url_str).map_err(|e| ZapError::EndpointError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use enostr::{FullKeypair, NoteId};

    use crate::zaps::networking::convert_lnurl_to_endpoint_url;

    use super::{
        fetch_invoice_lnurl, fetch_invoice_lud16, fetch_pay_req_from_lud16, lud16_to_lnurl,
    };

    #[ignore] // don't run this test automatically since it sends real http
    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_pay_req() {
        let lud16 = "jb55@sendsats.lol";

        let maybe_res = fetch_pay_req_from_lud16(lud16).await;

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

        let maybe_lnurl = lud16_to_lnurl(lud16);
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
        let maybe_invoice = rt.block_on(async {
            fetch_invoice_lud16(
                "jb55@sendsats.lol".to_owned(),
                1000,
                FullKeypair::generate().secret_key.to_secret_bytes(),
                crate::zaps::ZapTargetOwned::Note(crate::NoteZapTargetOwned {
                    note_id: NoteId::new([0; 32]),
                    zap_recipient: kp.pubkey,
                }),
                vec!["wss://relay.damus.io".to_owned()],
            )
            .block_and_take()
        });

        assert!(maybe_invoice.is_ok());
        let inner = maybe_invoice.unwrap();
        assert!(inner.is_ok());
        let invoice = inner.unwrap();
        assert!(invoice.invoice.starts_with("lnbc"));
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

        let maybe_invoice = rt.block_on(async {
            fetch_invoice_lnurl(
                lnurl.to_owned(),
                1000,
                kp.secret_key.to_secret_bytes(),
                crate::zaps::ZapTargetOwned::Note(crate::NoteZapTargetOwned {
                    note_id: NoteId::new([0; 32]),
                    zap_recipient: kp.pubkey,
                }),
                [relay.to_owned()].to_vec(),
            )
            .block_and_take()
        });

        assert!(maybe_invoice.is_ok());

        assert!(maybe_invoice.unwrap().unwrap().invoice.starts_with("lnbc"));
    }
}
