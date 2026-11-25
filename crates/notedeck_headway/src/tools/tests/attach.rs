//! `headway_attach` and the comment and description tools' `attachments`:
//! the upload runs off the loop with the Blossom server stubbed, and what it
//! answers, passed on as `nd headway comment --attach` passes it, puts the
//! image's URL in the text and its key in an `imeta` tag.

use super::live::{Live, live};
use super::*;
use headway::event::{Attachment, all_cards};
use nostrdb::NoteBuilder;
use notedeck::Started;
use notedeck::media::blossom::{self, BlobDescriptor, SERVER_LIST_KIND};
use std::sync::{Arc, Mutex};

/// The server the test account lists in its kind 10063.
const SERVER: &str = "https://blossom.test";

/// What the stub server was sent: each blob's bytes.
type Received = Arc<Mutex<Vec<Vec<u8>>>>;

/// The attach tool under test and what its stub uploader was sent.
struct StubAttach {
    tool: RegisteredTool,
    received: Received,
}

/// An [`attach::Attach`] whose uploads answer at once, stored at
/// `<server>/<sha256>` as a Blossom server stores them, and the bytes each
/// was sent.
fn stub_attach() -> StubAttach {
    let received = Received::default();
    let sent = Arc::clone(&received);
    let tool =
        crate::tools::attach::Attach::with_uploader(Arc::new(move |_seckey, server, blob| {
            let sha256 = blossom::sha256_hex(&blob.bytes);
            let size = blob.bytes.len() as u64;
            sent.lock().unwrap().push(blob.bytes);
            poll_promise::Promise::from_ready(Ok(BlobDescriptor {
                url: format!("{server}/{sha256}"),
                sha256,
                size,
                mime_type: Some("application/octet-stream".to_owned()),
            }))
        }));
    StubAttach {
        tool: RegisteredTool::slow(tool),
        received,
    }
}

/// Run `tool` with `args` as the socket does: prepared on the caller's
/// thread, its job left pending and run apart.
fn run_slow(
    live: &mut Live,
    tool: &RegisteredTool,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut app = live.notedeck.app_context();
    let mut cx = app.tool_context();
    match tool.start_rendered(&mut cx, &args) {
        Started::Pending(pending) => pending.run().result,
        Started::Done(done) => match done.outcome {
            notedeck::ToolOutcome::Error(err) => Err(err),
            _ => panic!("headway_attach answered without running its upload off the loop"),
        },
    }
}

/// List [`SERVER`] as the account's Blossom server, and wait until nostrdb
/// answers it.
fn seed_server_list(live: &mut Live) {
    let ndb = live.notedeck.app_context().ndb.clone();
    let secret = live.account.secret_key.secret_bytes();
    store::ingest_signed(
        &ndb,
        NoteBuilder::new()
            .kind(SERVER_LIST_KIND)
            .content("")
            .start_tag()
            .tag_str("server")
            .tag_str(SERVER),
        &store::Signer::new(&secret, None),
        &mut store::NoPublish,
    );
    let pubkey = *live.account.pubkey.bytes();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if blossom::server_list(&ndb, &txn, &pubkey).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "the server list never landed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A PNG on disk: where it is and what's in it.
struct Screenshot {
    path: String,
    png: Vec<u8>,
}

/// A 3x2 PNG written to `dir`: its path and bytes.
fn screenshot(dir: &TempDir) -> Screenshot {
    let image = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 140, 200, 255]));
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("png");
    let path = dir.path().join("shot.png");
    std::fs::write(&path, &bytes).expect("write png");
    Screenshot {
        path: path.to_string_lossy().into_owned(),
        png: bytes,
    }
}

/// A card as the tools address it: its `headway:` ref and its id.
struct CardRef {
    card: String,
    id: NoteId,
}

/// The demo board's first card.
fn first_card(live: &mut Live) -> CardRef {
    let card = &live.cards()[0];
    let id = NoteId::from_hex(card["id"].as_str().unwrap()).unwrap();
    CardRef {
        card: card["ref"].as_str().unwrap().to_owned(),
        id,
    }
}

/// What `pick` reads off the demo board's card `id`, once it reads
/// something.
fn card_until<T>(
    live: &mut Live,
    id: NoteId,
    what: &str,
    pick: impl Fn(&event::CardView) -> Option<T>,
) -> T {
    let ndb = live.notedeck.app_context().ndb.clone();
    let found = |v: &BoardView| all_cards(v).find(|c| c.id == id).and_then(&pick);
    let view = settled_view(&ndb, &live.account.pubkey, store::BOARD_ID, what, |v| {
        found(v).is_some()
    });
    found(&view).unwrap()
}

/// The upload is sealed: what the server got is ciphertext, and the
/// attachment it answers opens it back to the PNG, typed and sized.
fn check_upload(attachment: &Attachment, received: &Received, png: &[u8]) {
    let sent = received.lock().unwrap();
    assert_eq!(sent.len(), 1, "one upload");
    assert_ne!(sent[0], png, "the server never sees the plaintext");
    assert_eq!(
        attachment.url,
        format!("{SERVER}/{}", hex::encode(attachment.x))
    );
    assert_eq!(attachment.mime, "image/png");
    let dims = attachment.dims.expect("dims");
    assert_eq!((dims.width, dims.height), (3, 2));
    let imeta = crate::ui::attach::imeta(attachment);
    let plain = notedeck::media::encrypted::decrypt(&sent[0], &imeta.key, &imeta.nonce).unwrap();
    assert_eq!(plain, png);
}

/// `headway_attach` uploads off the loop and answers the attachment as
/// text; the comment tool takes it back, appends its URL on a line of its
/// own and carries its key in an `imeta` tag.
#[tokio::test]
async fn attach_then_comment_carries_the_image() {
    let mut live = live();
    seed_server_list(&mut live);
    let CardRef { card, id } = first_card(&mut live);
    let dir = TempDir::new().unwrap();
    let Screenshot { path, png } = screenshot(&dir);
    let StubAttach { tool, received } = stub_attach();

    let answer = run_slow(&mut live, &tool, json!({ "paths": [path] })).expect("attach");
    let texts = answer["attachments"].as_array().expect("attachments");
    assert_eq!(texts.len(), 1);
    let attachment = Attachment::from_imeta_text(texts[0].as_str().unwrap()).unwrap();
    check_upload(&attachment, &received, &png);

    live.edit(
        "headway_comment_card",
        json!({ "card": card, "body": "it breaks here", "attachments": answer["attachments"] }),
    );
    let comment = card_until(&mut live, id, "the comment", |c| {
        c.comments.first().cloned()
    });
    assert_eq!(
        comment.body,
        format!("it breaks here\n\n{}", attachment.url)
    );
    assert_eq!(comment.attachments, [attachment]);
}

/// The description tool appends a markdown image and adds the attachment
/// beside the ones its text still points at.
#[tokio::test]
async fn attach_then_describe_embeds_the_image() {
    let mut live = live();
    seed_server_list(&mut live);
    let CardRef { card, id } = first_card(&mut live);
    let dir = TempDir::new().unwrap();
    let Screenshot { path, png } = screenshot(&dir);
    let StubAttach { tool, received } = stub_attach();

    let answer = run_slow(&mut live, &tool, json!({ "paths": [path] })).expect("attach");
    let attachment =
        Attachment::from_imeta_text(answer["attachments"][0].as_str().unwrap()).unwrap();
    check_upload(&attachment, &received, &png);

    live.edit(
        "headway_set_description",
        json!({ "card": card, "description": "Steps:\n", "attachments": answer["attachments"] }),
    );
    let (description, attachments) = card_until(&mut live, id, "the description", |c| {
        (!c.description_attachments.is_empty())
            .then(|| (c.description.clone(), c.description_attachments.clone()))
    });
    assert_eq!(
        description,
        format!("Steps:\n\n![image]({})", attachment.url)
    );
    assert_eq!(attachments, [attachment]);
}

/// With no kind-10063 list there is nowhere to upload: the call fails
/// before anything is read, saying what to add. A file that isn't an image
/// is refused the same way, and so is `attach` sent past a command line.
#[tokio::test]
async fn attach_refuses_without_a_server_or_an_image() {
    let mut live = live();
    let dir = TempDir::new().unwrap();
    let Screenshot { path, .. } = screenshot(&dir);
    let StubAttach { tool, received } = stub_attach();

    let err = run_slow(&mut live, &tool, json!({ "paths": [path] })).unwrap_err();
    assert_eq!(err, crate::ui::attach::NO_SERVER);

    seed_server_list(&mut live);
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "not an image").unwrap();
    let err = run_slow(&mut live, &tool, json!({ "paths": [text] })).unwrap_err();
    assert!(err.ends_with("is not an image"), "{err}");
    assert!(received.lock().unwrap().is_empty(), "nothing was uploaded");

    let CardRef { card, .. } = first_card(&mut live);
    let err = live
        .call(
            "headway_comment_card",
            json!({ "card": card, "body": "x", "attach": [path] }),
        )
        .unwrap_err();
    assert_eq!(err, crate::tools::attach::ATTACH_NEEDS_UPLOAD);
}
