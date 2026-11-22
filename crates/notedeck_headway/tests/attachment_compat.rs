//! `headway::event::Attachment` and `notedeck::media::encrypted::EncryptedImeta`
//! are two copies of one `imeta` tag shape: `headway` has no `notedeck`
//! dependency, so it can't use notedeck's type. These tests pin that a cover
//! note or comment written by either side reads back on the other, so a
//! screenshot posted from the CLI opens in the app and the other way round.

use headway::event::{Attachment, AttachmentNonce, Dims, build_comment, note_attachments};
use nostrdb::{Note, NoteBuilder};
use nostrdb_net::{FullKeypair, NoteId};
use notedeck::media::PixelDimensions;
use notedeck::media::encrypted::{self, EncryptedImeta};

/// Sign `builder` into a note.
fn sign<'a>(builder: NoteBuilder<'a>, kp: &FullKeypair) -> Note<'a> {
    builder
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("note")
}

/// A real blob's imeta, as notedeck builds one after an upload.
fn notedeck_imeta() -> EncryptedImeta<'static> {
    let blob = encrypted::encrypt(b"\x89PNG not really").expect("encrypt");
    EncryptedImeta::new(
        "https://blossom.example/abc",
        "image/png",
        Some(PixelDimensions { x: 800, y: 600 }),
        &blob,
    )
}

/// notedeck's imeta seen as headway's [`Attachment`], field for field.
fn as_attachment(imeta: &EncryptedImeta<'_>) -> Attachment {
    Attachment {
        url: imeta.url.to_string(),
        mime: imeta.mime.to_string(),
        dims: imeta.dims.map(|d| Dims {
            width: d.x,
            height: d.y,
        }),
        x: imeta.x,
        ox: imeta.ox,
        key: imeta.key,
        nonce: AttachmentNonce::from_hex(&hex::encode(imeta.nonce.as_bytes())).expect("nonce"),
    }
}

/// A comment headway writes is an encrypted imeta notedeck's image cache
/// can register and decrypt.
#[test]
fn headway_attachment_reads_as_notedeck_imeta() {
    let kp = FullKeypair::generate();
    let imeta = notedeck_imeta();
    let attachment = as_attachment(&imeta);
    let card = NoteId::new([4; 32]);
    let note = sign(
        build_comment(&card, &kp.pubkey, None, "look", &[attachment]),
        &kp,
    );
    let read: Vec<EncryptedImeta> = encrypted::encrypted_imetas(&note).collect();
    assert_eq!(read, vec![imeta]);
}

/// An imeta notedeck writes is an attachment headway folds onto the card.
#[test]
fn notedeck_imeta_reads_as_headway_attachment() {
    let kp = FullKeypair::generate();
    let imeta = notedeck_imeta();
    let note = sign(
        imeta.write_tag(NoteBuilder::new().kind(1111).content("")),
        &kp,
    );
    assert_eq!(note_attachments(&note), vec![as_attachment(&imeta)]);
}
