use enostr::{NoteId, Pubkey};
use nostrdb::NoteBuilder;
use notedeck::ZapError;
use poll_promise::Promise;
use serde::Deserialize;
use url::Url;

fn fetch_pay_req(url: &Url) -> Promise<Result<LNUrlPayRequest, ZapError>> {
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
    promise
}

fn fetch_pay_req_from_lud16(lud16: &str) -> Promise<Result<LNUrlPayRequest, ZapError>> {
    let url = match generate_endpoint_url(lud16) {
        Ok(url) => url,
        Err(e) => return Promise::from_ready(Err(e)),
    };

    fetch_pay_req(&url)
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
    sender_nsec: &[u8; 32],
    relays: Vec<String>,
    recipient: &Pubkey,
    event_id: Option<&NoteId>,
) -> nostrdb::Note<'a> {
    let mut builder = NoteBuilder::new().kind(9734);

    builder = builder.start_tag().tag_str("relays");

    for relay in relays {
        builder = builder.tag_str(&relay)
    }

    builder = builder
        .start_tag()
        .tag_str("amount")
        .tag_str(&msats.to_string());

    builder = builder.start_tag().tag_str("lnurl").tag_str(lnurl);

    builder = builder.start_tag().tag_str("p").tag_str(&recipient.hex());

    if let Some(id) = event_id {
        builder = builder.start_tag().tag_str("e").tag_str(&id.hex());
    }

    builder.sign(sender_nsec).build().expect("note")
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct LNUrlPayRequest {
    #[serde(rename = "allowsNostr")]
    allow_nostr: bool,

    #[serde(rename = "nostrPubkey")]
    nostr_pubkey: String,

    #[serde(rename = "callback")]
    callback_url: String,

    #[serde(rename = "minSendable")]
    min_sendable: u64,

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

#[allow(dead_code)]
pub fn fetch_invoice_lud16(
    lud16: String,
    msats: u64,
    sender_nsec: [u8; 32],
    event_id: Option<NoteId>,
    relays: Vec<String>,
) -> Promise<Result<String, ZapError>> {
    let (invoice_sender, invoice_promise) = Promise::new();
    std::thread::spawn(move || {
        let invoice =
            fetch_invoice_lud16_blocking(&lud16, msats, &sender_nsec, event_id.as_ref(), relays);
        invoice_sender.send(invoice);
    });

    invoice_promise
}

#[allow(dead_code)]
pub fn fetch_invoice_lnurl(
    lnurl: String,
    msats: u64,
    sender_nsec: [u8; 32],
    event_id: Option<NoteId>,
    relays: Vec<String>,
) -> Promise<Result<String, ZapError>> {
    let (sender, promise) = Promise::new();

    std::thread::spawn(move || {
        let pay_req = match fetch_pay_req_from_lnurl(&lnurl).block_and_take() {
            Ok(req) => req,
            Err(e) => {
                sender.send(Err(e));
                return;
            }
        };

        let invoice = fetch_invoice_lnurl_blocking(
            &lnurl,
            &pay_req,
            msats,
            &sender_nsec,
            event_id.as_ref(),
            relays,
        );
        sender.send(invoice);
    });

    promise
}

fn convert_lnurl_to_endpoint_url(lnurl: &str) -> Result<Url, ZapError> {
    let (_, data) = bech32::decode(lnurl).map_err(|e| ZapError::Bech(e.to_string()))?;

    let url_str =
        String::from_utf8(data).map_err(|e| ZapError::Bech(format!("string conversion: {e}")))?;

    Url::parse(&url_str)
        .map_err(|e| ZapError::EndpointError(format!("endpoint url from lnurl is invalid: {e}")))
}

fn fetch_pay_req_from_lnurl(lnurl: &str) -> Promise<Result<LNUrlPayRequest, ZapError>> {
    let url = match convert_lnurl_to_endpoint_url(lnurl) {
        Ok(u) => u,
        Err(e) => return Promise::from_ready(Err(e)),
    };

    fetch_pay_req(&url)
}

fn fetch_invoice_lnurl_blocking(
    lnurl: &str,
    pay_req: &LNUrlPayRequest,
    msats: u64,
    sender_nsec: &[u8; 32],
    event_id: Option<&NoteId>,
    relays: Vec<String>,
) -> Result<String, ZapError> {
    let recipient = Pubkey::from_hex(&pay_req.nostr_pubkey)
        .map_err(|e| ZapError::EndpointError(format!("invalid pubkey hex from endpoint: {e}")))?;

    let note = make_kind_9734(lnurl, msats, sender_nsec, relays, &recipient, event_id);

    let mut base_url = Url::parse(&pay_req.callback_url)
        .map_err(|e| ZapError::EndpointError(format!("invalid callback url from endpoint: {e}")))?;

    let query = endpoint_query_for_invoice(&mut base_url, msats, lnurl, note)?;

    fetch_invoice(query).block_and_take().map(|i| i.invoice)
}

fn fetch_invoice_lud16_blocking(
    lud16: &str,
    msats: u64,
    sender_nsec: &[u8; 32],
    event_id: Option<&NoteId>,
    relays: Vec<String>,
) -> Result<String, ZapError> {
    let promise = fetch_pay_req_from_lud16(lud16);
    let pay_req = promise.block_and_take()?;

    let lnurl = lud16_to_lnurl(lud16)?;

    fetch_invoice_lnurl_blocking(&lnurl, &pay_req, msats, sender_nsec, event_id, relays)
}

fn fetch_invoice(req: &Url) -> Promise<Result<LNInvoice, ZapError>> {
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
    promise
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
    use enostr::FullKeypair;

    use crate::zaps::convert_lnurl_to_endpoint_url;

    use super::{
        fetch_invoice_lnurl, fetch_invoice_lud16, fetch_pay_req_from_lud16, lud16_to_lnurl,
    };

    #[ignore] // don't run this test automatically since it sends real http
    #[test]
    fn test_get_pay_req() {
        let lud16 = "jb55@sendsats.lol";

        let promise = fetch_pay_req_from_lud16(lud16);

        let maybe_res = promise.block_and_take();
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
    #[tokio::test]
    async fn test_generate_invoice() {
        let lud16 = "jb55@sendsats.lol";
        let kp = FullKeypair::generate();
        let relay = "wss://relay.damus.io";

        let maybe_invoice = fetch_invoice_lud16(
            lud16.to_owned(),
            1000,
            kp.secret_key.to_secret_bytes(),
            None,
            [relay.to_owned()].to_vec(),
        )
        .block_and_take();

        assert!(maybe_invoice.is_ok());
        assert!(maybe_invoice.unwrap().starts_with("lnbc"));
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
        let lnurl =
            "lnurl1dp68gurn8ghj7um9dej8xct5wvhxcmmv9uh8wetvdskkkmn0wahz7mrww4excup0df3r2dg3mj444";
        let kp = FullKeypair::generate();
        let relay = "wss://relay.damus.io";

        let maybe_invoice = fetch_invoice_lnurl(
            lnurl.to_owned(),
            1000,
            kp.secret_key.to_secret_bytes(),
            None,
            [relay.to_owned()].to_vec(),
        )
        .block_and_take();
        assert!(maybe_invoice.is_ok());

        assert!(maybe_invoice.unwrap().starts_with("lnbc"));
    }
}
