//! Blossom blob storage: the BUD-02 upload and the BUD-03 server list.
//!
//! A blob is uploaded with `PUT <server>/upload`, the raw bytes as the body,
//! and an `Authorization: Nostr <base64 event>` header. The event is a
//! kind-24242 authorization (BUD-11) signed on this device:
//!
//! ```text
//! kind 24242, content "Upload screenshot"
//! ["t", "upload"], ["x", "<sha256 hex of the body>"],
//! ["expiration", "<now + 5 minutes>"]
//! ```
//!
//! The server answers with a blob descriptor ([`BlobDescriptor`]), whose
//! `url` is where the blob can be fetched from.
//!
//! Which server to use comes from the account's kind-10063 list: its
//! `server` tags, in the user's order of preference. [`server_list`] reads
//! the newest one out of nostrdb.
//!
//! Nothing here blocks: [`upload`] returns a [`Promise`] that the render
//! loop polls.

use base64::{prelude::BASE64_STANDARD, Engine};
use nostrdb::{Filter, Ndb, Note, NoteBuilder, Transaction};
use poll_promise::Promise;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Kind of a Blossom authorization event (BUD-11).
pub const AUTH_KIND: u32 = 24242;

/// Kind of a user's Blossom server list (BUD-03).
pub const SERVER_LIST_KIND: u32 = 10063;

/// How long an upload authorization stays valid, in seconds.
pub const AUTH_TTL_SECS: u64 = 5 * 60;

/// What a Blossom server says it stored (BUD-02's blob descriptor).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BlobDescriptor {
    /// Where the blob can be fetched from.
    pub url: String,
    /// Lowercase hex sha256 of the stored bytes.
    pub sha256: String,
    /// Size of the stored bytes.
    pub size: u64,
    /// The MIME type the server recorded, if it gave one.
    #[serde(rename = "type", default)]
    pub mime_type: Option<String>,
}

/// Why an upload failed.
#[derive(Debug, thiserror::Error)]
pub enum BlossomError {
    /// The authorization event could not be built or serialized.
    #[error("could not build the upload authorization: {0}")]
    Auth(String),

    /// The request never got a response.
    #[error("upload request failed: {0}")]
    Network(String),

    /// The server refused the upload. `reason` is its `X-Reason` header,
    /// when it sent one.
    #[error("server refused the upload ({status} {status_text}){}", reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    Refused {
        status: u16,
        status_text: String,
        reason: Option<String>,
    },

    /// The server answered 2xx with something that is not a blob descriptor.
    #[error("server reply is not a blob descriptor: {0}")]
    BadReply(#[from] serde_json::Error),

    /// The server says it stored bytes other than the ones sent.
    #[error("server stored sha256 {got}, expected {expected}")]
    HashMismatch { expected: String, got: String },
}

/// A blob ready to upload: its bytes and the `Content-Type` to send them as.
///
/// For an encrypted blob the type is `application/octet-stream`: the server
/// only ever sees ciphertext.
pub struct UploadBlob {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Lowercase hex sha256 of `bytes`, the form Blossom uses for blob ids.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The kind-24242 event authorizing the upload of the blob whose sha256 is
/// `sha256_hex`, signed with `seckey`, created at `now` and valid for
/// [`AUTH_TTL_SECS`].
pub fn upload_auth_note(
    seckey: &[u8; 32],
    sha256_hex: &str,
    now: u64,
) -> Result<Note<'static>, BlossomError> {
    let expiration = (now + AUTH_TTL_SECS).to_string();
    NoteBuilder::new()
        .kind(AUTH_KIND)
        .content("Upload screenshot")
        .created_at(now)
        .start_tag()
        .tag_str("t")
        .tag_str("upload")
        .start_tag()
        .tag_str("x")
        .tag_str(sha256_hex)
        .start_tag()
        .tag_str("expiration")
        .tag_str(&expiration)
        .sign(seckey)
        .build()
        .ok_or_else(|| BlossomError::Auth("note builder failed".to_owned()))
}

/// The `Authorization` header value carrying `note`: `Nostr ` and the
/// base64 of its JSON.
pub fn auth_header(note: &Note) -> Result<String, BlossomError> {
    let json = note.json().map_err(|e| BlossomError::Auth(e.to_string()))?;
    Ok(format!("Nostr {}", BASE64_STANDARD.encode(json)))
}

/// The BUD-02 upload endpoint of `server`, a base URL such as
/// `https://blossom.example` (a trailing slash is fine).
pub fn upload_url(server: &str) -> String {
    format!("{}/upload", server.trim_end_matches('/'))
}

/// Upload `blob` to `server` (BUD-02), authorized by an event signed with
/// `seckey`.
///
/// The request runs on ehttp's thread; poll the promise from the render
/// loop. The reply's `sha256` is checked against the bytes sent, so a
/// descriptor that comes back names the blob that was uploaded.
pub fn upload(
    seckey: &[u8; 32],
    server: &str,
    blob: UploadBlob,
) -> Promise<Result<BlobDescriptor, BlossomError>> {
    let sha256 = sha256_hex(&blob.bytes);
    let request = match upload_request(seckey, server, &sha256, blob, unix_now()) {
        Ok(request) => request,
        Err(e) => return Promise::from_ready(Err(e)),
    };

    let (sender, promise) = Promise::new();
    ehttp::fetch(request, move |response| {
        sender.send(descriptor_from_response(response, &sha256));
    });
    promise
}

/// The PUT request for [`upload`], with its authorization signed at `now`.
fn upload_request(
    seckey: &[u8; 32],
    server: &str,
    sha256: &str,
    blob: UploadBlob,
    now: u64,
) -> Result<ehttp::Request, BlossomError> {
    let auth = auth_header(&upload_auth_note(seckey, sha256, now)?)?;
    Ok(ehttp::Request {
        method: "PUT".to_owned(),
        url: upload_url(server),
        body: blob.bytes,
        headers: ehttp::Headers::new(&[
            ("Authorization", auth.as_str()),
            ("Content-Type", blob.content_type.as_str()),
        ]),
    })
}

/// Turn the server's reply into the stored blob's descriptor, refusing one
/// whose hash isn't `expected_sha256`.
fn descriptor_from_response(
    response: Result<ehttp::Response, String>,
    expected_sha256: &str,
) -> Result<BlobDescriptor, BlossomError> {
    let response = response.map_err(BlossomError::Network)?;
    if !response.ok {
        return Err(BlossomError::Refused {
            status: response.status,
            status_text: response.status_text.clone(),
            reason: response.headers.get("x-reason").map(str::to_owned),
        });
    }

    let descriptor: BlobDescriptor = serde_json::from_slice(&response.bytes)?;
    if !descriptor.sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(BlossomError::HashMismatch {
            expected: expected_sha256.to_owned(),
            got: descriptor.sha256,
        });
    }
    Ok(descriptor)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A user's Blossom server list: the newest kind-10063 note they signed.
pub struct ServerList<'a> {
    note: Note<'a>,
}

impl<'a> ServerList<'a> {
    /// The `server` tag values, in the user's order of preference.
    pub fn servers(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.note.tags().iter().filter_map(|tag| {
            if tag.count() < 2 || tag.get_str(0) != Some("server") {
                return None;
            }
            tag.get_str(1).filter(|s| !s.is_empty())
        })
    }

    /// The server to upload to: the first one on the list.
    pub fn first(&self) -> Option<&'a str> {
        self.servers().next()
    }
}

/// The newest kind-10063 server list `pubkey` has published, if nostrdb has
/// one.
///
/// Kind 10063 is replaceable without a `d` tag, so nostrdb_net's
/// `query_replaceable` (which groups by `d` and skips notes without one)
/// never matches it. Like the kind-3 and kind-10002 readers in
/// `crate::account`, this asks nostrdb for the one newest note instead.
pub fn server_list<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    pubkey: &[u8; 32],
) -> Option<ServerList<'a>> {
    let filter = Filter::new()
        .authors([pubkey])
        .kinds([SERVER_LIST_KIND as u64])
        .limit(1)
        .build();
    let note = ndb.query(txn, &[filter], 1).ok()?.into_iter().next()?.note;
    Some(ServerList { note })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndb_testing::TempNdb;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    const SECKEY: [u8; 32] = [7u8; 32];

    fn pubkey_of(seckey: &[u8; 32]) -> [u8; 32] {
        let note = NoteBuilder::new().kind(1).sign(seckey).build().unwrap();
        *note.pubkey()
    }

    /// Decode an `Authorization: Nostr …` header back to the event JSON.
    fn decode_header(header: &str) -> serde_json::Value {
        let b64 = header.strip_prefix("Nostr ").expect("Nostr scheme");
        let json = BASE64_STANDARD.decode(b64).expect("base64");
        serde_json::from_slice(&json).expect("event json")
    }

    /// The auth event is a kind-24242 with the upload verb, the body's hash
    /// and a five-minute expiration, and the header carries exactly it.
    #[test]
    fn auth_event_round_trips_through_header() {
        let hash = sha256_hex(b"ciphertext");
        let note = upload_auth_note(&SECKEY, &hash, 1_700_000_000).unwrap();
        let ev = decode_header(&auth_header(&note).unwrap());

        assert_eq!(ev["kind"], AUTH_KIND);
        assert_eq!(ev["created_at"], 1_700_000_000u64);
        assert_eq!(ev["pubkey"], hex::encode(pubkey_of(&SECKEY)));
        assert_eq!(ev["id"], hex::encode(note.id()));
        assert_eq!(
            ev["tags"],
            serde_json::json!([
                ["t", "upload"],
                ["x", hash],
                ["expiration", (1_700_000_000 + AUTH_TTL_SECS).to_string()],
            ])
        );
        assert!(!ev["sig"].as_str().unwrap().is_empty());
    }

    #[test]
    fn upload_url_tolerates_trailing_slash() {
        assert_eq!(upload_url("https://b.example"), "https://b.example/upload");
        assert_eq!(upload_url("https://b.example/"), "https://b.example/upload");
    }

    fn ingest_server_list(ndb: &Ndb, created_at: u64, servers: &[&str]) {
        let mut builder = NoteBuilder::new()
            .kind(SERVER_LIST_KIND)
            .content("")
            .created_at(created_at);
        for server in servers {
            builder = builder.start_tag().tag_str("server").tag_str(server);
        }
        let note = builder.sign(&SECKEY).build().unwrap();
        let json = format!("[\"EVENT\",\"s\",{}]", note.json().unwrap());
        ndb.process_event(&json).expect("ingest");
    }

    /// The newest 10063 wins, its `server` tags read in order, and an
    /// account without one has no list.
    #[tokio::test]
    async fn server_list_reads_newest_in_order() {
        let tmp = TempNdb::new();
        let me = pubkey_of(&SECKEY);
        let sub = tmp
            .ndb
            .subscribe(&[Filter::new().kinds([SERVER_LIST_KIND as u64]).build()])
            .unwrap();

        {
            let txn = Transaction::new(&tmp.ndb).unwrap();
            assert!(server_list(&tmp.ndb, &txn, &me).is_none());
        }

        ingest_server_list(&tmp.ndb, 100, &["https://old.example"]);
        ingest_server_list(
            &tmp.ndb,
            200,
            &["https://first.example", "https://second.example/"],
        );
        tmp.ndb
            .wait_for_all_notes_within(sub, 2, Duration::from_secs(10))
            .await
            .expect("server lists ingested");

        let txn = Transaction::new(&tmp.ndb).unwrap();
        let list = server_list(&tmp.ndb, &txn, &me).expect("list");
        assert_eq!(
            list.servers().collect::<Vec<_>>(),
            ["https://first.example", "https://second.example/"]
        );
        assert_eq!(list.first(), Some("https://first.example"));

        let other = [9u8; 32];
        assert!(server_list(&tmp.ndb, &txn, &other).is_none());
    }

    /// One HTTP request as the stub saw it.
    struct SeenRequest {
        request_line: String,
        authorization: String,
        content_type: String,
        body: Vec<u8>,
    }

    /// Serve one request on a local port, answer it with `status` and
    /// `reply`, and hand back what was sent.
    fn stub_server(
        status: &'static str,
        reply: String,
    ) -> (String, std::thread::JoinHandle<SeenRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut authorization = String::new();
            let mut content_type = String::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value.trim().to_owned();
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => authorization = value,
                    "content-type" => content_type = value,
                    "content-length" => len = value.parse().unwrap(),
                    _ => {}
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Reason: nope\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
            SeenRequest {
                request_line: request_line.trim_end().to_owned(),
                authorization,
                content_type,
                body,
            }
        });
        (base, handle)
    }

    /// A PUT to a local stub carries the body and a valid auth header, and
    /// the stub's descriptor comes back.
    #[test]
    fn upload_round_trip_against_stub() {
        let body = b"not really ciphertext".to_vec();
        let hash = sha256_hex(&body);
        let reply = serde_json::json!({
            "url": format!("https://cdn.example/{hash}"),
            "sha256": hash,
            "size": body.len(),
            "type": "application/octet-stream",
            "uploaded": 1_700_000_000u64,
        })
        .to_string();
        let (base, server) = stub_server("200 OK", reply);

        let result = upload(
            &SECKEY,
            &format!("{base}/"),
            UploadBlob {
                bytes: body.clone(),
                content_type: "application/octet-stream".to_owned(),
            },
        )
        .block_and_take()
        .expect("upload");

        assert_eq!(result.url, format!("https://cdn.example/{hash}"));
        assert_eq!(result.size, body.len() as u64);
        assert_eq!(
            result.mime_type.as_deref(),
            Some("application/octet-stream")
        );

        let seen = server.join().unwrap();
        assert_eq!(seen.request_line, "PUT /upload HTTP/1.1");
        assert_eq!(seen.content_type, "application/octet-stream");
        assert_eq!(seen.body, body);
        let ev = decode_header(&seen.authorization);
        assert_eq!(ev["kind"], AUTH_KIND);
        assert_eq!(ev["tags"][1], serde_json::json!(["x", hash]));
    }

    /// A refusal surfaces the status and the server's `X-Reason`.
    #[test]
    fn upload_refusal_carries_reason() {
        let (base, server) = stub_server("401 Unauthorized", String::new());
        let err = upload(
            &SECKEY,
            &base,
            UploadBlob {
                bytes: b"x".to_vec(),
                content_type: "image/png".to_owned(),
            },
        )
        .block_and_take()
        .unwrap_err();
        server.join().unwrap();

        match err {
            BlossomError::Refused { status, reason, .. } => {
                assert_eq!(status, 401);
                assert_eq!(reason.as_deref(), Some("nope"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// A descriptor naming other bytes than the ones sent is an error.
    #[test]
    fn descriptor_hash_mismatch_is_refused() {
        let response = ehttp::Response {
            url: "http://x/upload".to_owned(),
            ok: true,
            status: 200,
            status_text: "OK".to_owned(),
            headers: ehttp::Headers::default(),
            bytes: br#"{"url":"u","sha256":"00","size":1}"#.to_vec(),
        };
        let err = descriptor_from_response(Ok(response), &sha256_hex(b"x")).unwrap_err();
        assert!(matches!(err, BlossomError::HashMismatch { .. }));
    }
}
