use enostr::{NoteId, Pubkey};
use image::EncodableLayout;
use lightning_invoice::Bolt11Invoice;
use secp256k1::{schnorr::Signature, Message, Secp256k1, XOnlyPublicKey};
use sha2::Digest;

#[allow(dead_code)]
#[derive(Debug)]
pub enum ZapTarget {
    Profile(Pubkey),
    Note(NoteZapTarget),
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct NoteZapTarget {
    pub note_id: NoteId,
    pub author: Pubkey,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct Zap {
    pub sender: Pubkey,
    pub target: ZapTarget,
    pub invoice: Bolt11Invoice,
}

#[allow(dead_code)]
impl Zap {
    pub fn from_zap_event(zap_event: nostrdb::Note, sender: &Pubkey) -> Option<Self> {
        if sender.bytes() != zap_event.pubkey() {
            // Make sure that we only create a zap event if it is authorized by the profile or event
            return None;
        }

        let zap_tags = get_zap_tags(zap_event)?;
        let invoice = zap_tags.bolt11.parse::<Bolt11Invoice>().ok()?;

        // invoice must be specific
        invoice.amount_milli_satoshis()?;

        if let Some(preimage) = zap_tags.preimage {
            if !preimage_matches_invoice(&invoice, preimage) {
                return None;
            }
        }

        let Ok(zap_req) = enostr::Note::from_json(zap_tags.description) else {
            return None;
        };

        if !valid_zap_request(zap_req) {
            return None;
        }

        let zap_target = determine_zap_target(&zap_tags)?;

        Some(Zap {
            sender: *sender,
            target: zap_target,
            invoice,
        })
    }
}

#[allow(dead_code)]
pub fn event_tag<'a>(ev: nostrdb::Note<'a>, name: &str) -> Option<&'a str> {
    ev.tags().iter().find_map(|tag| {
        if tag.count() < 2 {
            return None;
        }

        let cur_name = tag.get_str(0)?;

        if cur_name != name {
            return None;
        }

        tag.get_str(1)
    })
}

fn determine_zap_target(tags: &ZapTags) -> Option<ZapTarget> {
    if let Some(note_zapped) = tags.note_zapped {
        Some(ZapTarget::Note(NoteZapTarget {
            note_id: NoteId::new(*note_zapped),
            author: Pubkey::new(*tags.recipient),
        }))
    } else {
        Some(ZapTarget::Profile(Pubkey::new(*tags.recipient)))
    }
}

pub fn event_commitment(
    pubkey: Pubkey,
    created_at: u64,
    kind: u64,
    tags: Vec<Vec<String>>,
    content: String,
) -> String {
    // Serialize the content and tags into JSON strings.
    let content_json = serde_json::to_string(&content).expect("Failed to serialize content");
    let tags_json = serde_json::to_string(&tags).expect("Failed to serialize tags");

    format!(
        "[0,\"{}\",{},{},{},{}]",
        pubkey.hex(),
        created_at,
        kind,
        tags_json,
        content_json
    )
}

// TODO(kernelkind): i think we may be able to validate just with the nostrdb::Note. Not exactly sure yet how though
fn valid_zap_request(note: enostr::Note) -> bool {
    let sig = note.sig.clone();

    let commitment = event_commitment(
        note.pubkey,
        note.created_at,
        note.kind,
        note.tags,
        note.content,
    );

    let commitment_bytes = commitment.as_bytes();
    let hash = sha256(commitment_bytes);
    let check_noteid = NoteId::new(hash);

    if note.id != check_noteid {
        return false;
    }

    let Ok(sig_bytes) = hex::decode(sig) else {
        return false;
    };

    let sig_bytes: Option<[u8; 64]> = sig_bytes.try_into().ok();

    let Some(sig_bytes) = sig_bytes else {
        return false;
    };

    if !verify_schnorr_signature(&note.pubkey, &sig_bytes, note.id.bytes()) {
        return false;
    }

    true
}

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(input);
    let result = hasher.finalize();
    result.into()
}

pub fn verify_schnorr_signature(
    pubkey_bytes: &[u8; 32],
    sig_bytes: &[u8; 64],
    msg_bytes: &[u8; 32],
) -> bool {
    let secp = Secp256k1::verification_only();

    let Ok(xonly_pubkey) = XOnlyPublicKey::from_slice(pubkey_bytes) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(sig_bytes) else {
        return false;
    };

    let msg = Message::from_digest(*msg_bytes);

    secp.verify_schnorr(&sig, msg.as_ref(), &xonly_pubkey)
        .is_ok()
}

fn preimage_matches_invoice(invoice: &Bolt11Invoice, preimage: &str) -> bool {
    let Ok(preimage_bytes) = hex::decode(preimage.as_bytes()) else {
        return false;
    };

    invoice.payment_secret().0 == preimage_bytes.as_bytes()
}

struct ZapTags<'a> {
    pub bolt11: &'a str,
    pub preimage: Option<&'a str>,
    pub description: &'a str,
    pub recipient: &'a [u8; 32],
    pub note_zapped: Option<&'a [u8; 32]>,
}
fn get_zap_tags(ev: nostrdb::Note) -> Option<ZapTags> {
    let mut bolt11 = None;
    let mut preimage = None;
    let mut description = None;
    let mut recipient = None;
    let mut note_zapped = None;

    for tag in ev.tags() {
        // Only process tags with at least two elements.
        if tag.count() < 2 {
            continue;
        }

        let Some(cur_name) = tag.get_str(0) else {
            continue;
        };

        if cur_name == "bolt11" {
            bolt11 = tag.get_str(1);
        } else if cur_name == "preimage" {
            preimage = tag.get_str(1);
        } else if cur_name == "description" {
            description = tag.get_str(1);
        } else if cur_name == "p" {
            recipient = tag.get_id(1);
        } else if cur_name == "e" {
            note_zapped = tag.get_id(1);
        }

        if bolt11.is_some()
            && preimage.is_some()
            && description.is_some()
            && recipient.is_some()
            && note_zapped.is_some()
        {
            break;
        }
    }

    Some(ZapTags {
        bolt11: bolt11?,
        preimage,
        description: description?,
        recipient: recipient?,
        note_zapped,
    })
}

#[cfg(test)]
mod tests {
    use enostr::{NoteId, Pubkey};

    use nostrdb::{Config, Filter, IngestMetadata, Ndb, Transaction};
    use tempfile::TempDir;

    use crate::zaps::zap::{valid_zap_request, Zap};

    // a random zap receipt
    const ZAP_RECEIPT: &str = r#"{"kind":9735,"id":"c8a5767f33cd73716cf670c9615a73ec50cb91c373100f6c0d5cc160237b58dc","pubkey":"be1d89794bf92de5dd64c1e60f6a2c70c140abac9932418fee30c5c637fe9479","created_at":1743191143,"tags":[["p","1af54955936be804f95010647ea5ada5c7627eddf0734a7f813bba0e31eed960"],["e","ec998b249a8c366358c264f0932a9b433ac60b1c2f630cb24a604560873f7030"],["bolt11","lnbc330n1pn7dlrrpp566sfk69zda849huwjw6wepw3uzxxp4mp9np54qx49ruw8cuv86ushp52te27l4jadsz0u76jvgsk5uekl04tujpjkt9cc7duu0jfzp9zdtscqzzsxqyz5vqsp5m3tzc7ryp5f9fv90v27uyrrd4qfmj5lrwv9rvmvum3v50kdph23s9qxpqysgqut2ssf0m7nmtd73cwqk7qfw4sw6zlj598sjdxmdsepmvn0ptamnhf45c425h26juzcfupegltefwsf8qav2ldell7v9fpc0y23nl0kgqtf432g"],["description","{\"id\":\"73d05cfe976bb56b139b6cd04286a801b20cc0b01070886d6e3176ff2e107833\",\"pubkey\":\"d4338b7c3306491cfdf54914d1a52b80a965685f7361311eae5f3eaff1d23a5b\",\"created_at\":1743191138,\"kind\":9734,\"tags\":[[\"e\",\"ec998b249a8c366358c264f0932a9b433ac60b1c2f630cb24a604560873f7030\"],[\"p\",\"1af54955936be804f95010647ea5ada5c7627eddf0734a7f813bba0e31eed960\"],[\"relays\",\"wss://nosdrive.app/relay\"],[\"alt\",\"Zap request\"]],\"content\":\"\",\"sig\":\"2091b7f720586d7420ea7a90406ea856378339c8b0b3f3e695ccbfebaa8c4ea20a3cb850ff18cae957aa2e0ecb06c386d0bd27aa7a13bf7a8f7425a4c2a57903\"}"],["preimage","13821fcf87afa4c3bb753d62949481969e6af8fca9867d753e3503bd45e2814e"]],"content":"","sig":"d15aecbd1d0d289f99ffbf4d0b7c77c24875ed38fed13deee4e2e1254bcd05bda8dca3bb2858b5c3167749b4afa732f4670b9df54904786614252b4ed7916e5f"}"#;

    const ZAP_REQ: &str = r#"{"id":"73d05cfe976bb56b139b6cd04286a801b20cc0b01070886d6e3176ff2e107833","pubkey":"d4338b7c3306491cfdf54914d1a52b80a965685f7361311eae5f3eaff1d23a5b","created_at":1743191138,"kind":9734,"tags":[["e","ec998b249a8c366358c264f0932a9b433ac60b1c2f630cb24a604560873f7030"],["p","1af54955936be804f95010647ea5ada5c7627eddf0734a7f813bba0e31eed960"],["relays","wss://nosdrive.app/relay"],["alt","Zap request"]],"content":"","sig":"2091b7f720586d7420ea7a90406ea856378339c8b0b3f3e695ccbfebaa8c4ea20a3cb850ff18cae957aa2e0ecb06c386d0bd27aa7a13bf7a8f7425a4c2a57903"}"#;

    #[test]
    fn test_valid_zap_req() {
        let note = enostr::Note::from_json(ZAP_REQ).unwrap();

        assert!(valid_zap_request(note));
    }

    #[tokio::test]
    async fn test_zap_event() {
        let pk =
            Pubkey::from_hex("be1d89794bf92de5dd64c1e60f6a2c70c140abac9932418fee30c5c637fe9479")
                .unwrap();

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &Config::new()).unwrap();

        let ev = format!(r#"["EVENT", "random_string", {ZAP_RECEIPT}]"#);
        let filter = Filter::new().authors([pk.bytes()]).build();
        let sub_id = ndb.subscribe(&[filter]).unwrap();
        let res = ndb.process_event_with(&ev, IngestMetadata::new());
        assert!(res.is_ok());

        let note_key = ndb.wait_for_notes(sub_id, 1).await.unwrap()[0];
        let txn = Transaction::new(&ndb).unwrap();
        let note = ndb.get_note_by_key(&txn, note_key).unwrap();

        assert!(
            note.id()
                == NoteId::from_hex(
                    "c8a5767f33cd73716cf670c9615a73ec50cb91c373100f6c0d5cc160237b58dc"
                )
                .unwrap()
                .bytes()
        );

        let zap = Zap::from_zap_event(note, &pk);

        assert!(zap.is_some());
    }
}
