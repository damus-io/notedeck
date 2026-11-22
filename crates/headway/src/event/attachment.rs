//! Encrypted file attachments on cover notes and comments.
//!
//! A card's description (kind 1624) or a comment (kind 1111) can carry
//! files, usually screenshots, encrypted on the writer's device and uploaded
//! as ciphertext. Each one is a NIP-92 `imeta` tag holding where the
//! ciphertext lives and the key that opens it:
//!
//! ```text
//! ["imeta", "url https://server/<x>", "m image/png", "dim 640x480",
//!  "x <sha256 of ciphertext>", "ox <sha256 of plaintext>",
//!  "encryption-algorithm aes-gcm",
//!  "decryption-key <hex>", "decryption-nonce <hex>"]
//! ```
//!
//! This is the same tag `notedeck::media::encrypted::EncryptedImeta` writes
//! and reads (AES-256-GCM in Amethyst's parameters). `headway` has no
//! `notedeck` dependency, so it carries its own [`Attachment`] in the same
//! shape; `notedeck_headway`'s tests pin that the two read each other's
//! tags. Headway never encrypts or decrypts: the bytes are the UI's
//! business, the tag is the board's.
//!
//! On a shared board the attachment rides inside the sealed rumor like every
//! other tag, so the key is only visible to team members.

use nostrdb::{Note, NoteBuilder, Tag};

/// The only `encryption-algorithm` an [`Attachment`] carries.
pub const ATTACHMENT_ALGORITHM: &str = "aes-gcm";

/// An image's pixel size, from an imeta `dim WxH` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dims {
    pub width: u32,
    pub height: u32,
}

/// A GCM nonce in one of the two lengths an attachment can carry. Amethyst
/// and notedeck write 16 bytes; some clients write the standard 12.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AttachmentNonce {
    /// The standard 96-bit nonce.
    Gcm96([u8; 12]),
    /// Amethyst's 128-bit nonce.
    Gcm128([u8; 16]),
}

impl AttachmentNonce {
    /// The nonce's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Gcm96(n) => n,
            Self::Gcm128(n) => n,
        }
    }

    /// Decode a 12- or 16-byte nonce from hex; `None` for any other length.
    pub fn from_hex(hex_str: &str) -> Option<Self> {
        match hex_str.len() {
            24 => decode_hex_array(hex_str).map(Self::Gcm96),
            32 => decode_hex_array(hex_str).map(Self::Gcm128),
            _ => None,
        }
    }
}

/// One encrypted file on a cover note or comment: where its ciphertext lives,
/// what it decrypts to, and the key that opens it. Owned, since it lives in
/// the folded [`CardView`](super::CardView) long after the note's transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Attachment {
    /// The ciphertext's URL, usually `https://<blossom server>/<x>`.
    pub url: String,
    /// The plaintext's MIME type (`m`), e.g. `image/png`.
    pub mime: String,
    /// The image's pixel size, when known.
    pub dims: Option<Dims>,
    /// SHA-256 of the ciphertext.
    pub x: [u8; 32],
    /// SHA-256 of the plaintext, when the writer gave one.
    pub ox: Option<[u8; 32]>,
    /// The AES-256 key (`decryption-key`).
    pub key: [u8; 32],
    /// The GCM nonce (`decryption-nonce`).
    pub nonce: AttachmentNonce,
}

impl Attachment {
    /// Whether the plaintext is an image, so the UI can draw it inline.
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    /// Append this attachment to `builder` as one `imeta` tag.
    pub fn write_tag<'b>(&self, builder: NoteBuilder<'b>) -> NoteBuilder<'b> {
        self.entries()
            .iter()
            .fold(builder.start_tag().tag_str("imeta"), |b, e| b.tag_str(e))
    }

    /// The `imeta` tag's entries after its name, `name value` each, in the
    /// order [`write_tag`](Self::write_tag) writes them.
    pub fn entries(&self) -> Vec<String> {
        let mut entries = vec![format!("url {}", self.url), format!("m {}", self.mime)];
        if let Some(dims) = self.dims {
            entries.push(format!("dim {}x{}", dims.width, dims.height));
        }
        entries.push(format!("x {}", hex::encode(self.x)));
        if let Some(ox) = self.ox {
            entries.push(format!("ox {}", hex::encode(ox)));
        }
        entries.push(format!("encryption-algorithm {ATTACHMENT_ALGORITHM}"));
        entries.push(format!("decryption-key {}", hex::encode(self.key)));
        entries.push(format!(
            "decryption-nonce {}",
            hex::encode(self.nonce.as_bytes())
        ));
        entries
    }

    /// The attachment as text: its `imeta` entries, one per line. This is
    /// what `nd headway attach` answers and what the comment and description
    /// tools take back, so an attachment crosses from the upload to the
    /// event in the shape the event stores it.
    pub fn imeta_text(&self) -> String {
        self.entries().join("\n")
    }

    /// Read [`imeta_text`](Self::imeta_text) back, by the rules of
    /// [`from_tag`](Self::from_tag).
    pub fn from_imeta_text(text: &str) -> Option<Self> {
        let mut fields = ImetaFields::default();
        for (name, value) in text.lines().filter_map(|l| l.trim().split_once(' ')) {
            fields.set(name, value);
        }
        fields.into_attachment()
    }

    /// Read an attachment from one tag.
    ///
    /// `None` for a tag that is not an `imeta`, a plain (unencrypted) imeta,
    /// one whose algorithm is not `aes-gcm`, and one missing any of `url`,
    /// `m`, `x`, `decryption-key` or `decryption-nonce` or with one of them
    /// malformed. Unknown entries (`blurhash`, `size`, ...) are skipped, and
    /// the first of each name wins.
    pub fn from_tag(tag: &Tag<'_>) -> Option<Self> {
        if tag.get_str(0) != Some("imeta") {
            return None;
        }
        let mut fields = ImetaFields::default();
        for index in 1..tag.count() {
            let Some((name, value)) = tag.get_str(index).and_then(|e| e.split_once(' ')) else {
                continue;
            };
            fields.set(name, value);
        }
        fields.into_attachment()
    }
}

/// Every encrypted attachment on `note`, in tag order.
pub fn note_attachments(note: &Note<'_>) -> Vec<Attachment> {
    note.tags()
        .into_iter()
        .filter_map(|tag| Attachment::from_tag(&tag))
        .collect()
}

/// Append one `imeta` tag per attachment to `builder`, in order.
pub(crate) fn write_attachments<'b>(
    builder: NoteBuilder<'b>,
    attachments: &[Attachment],
) -> NoteBuilder<'b> {
    attachments.iter().fold(builder, |b, a| a.write_tag(b))
}

/// The entries of one `imeta` tag as they are found, before
/// [`ImetaFields::into_attachment`] checks them for completeness.
#[derive(Default)]
struct ImetaFields<'a> {
    url: Option<&'a str>,
    mime: Option<&'a str>,
    dims: Option<&'a str>,
    x: Option<&'a str>,
    ox: Option<&'a str>,
    algorithm: Option<&'a str>,
    key: Option<&'a str>,
    nonce: Option<&'a str>,
}

impl<'a> ImetaFields<'a> {
    /// Record one `name value` entry. The first of each name wins.
    fn set(&mut self, name: &str, value: &'a str) {
        let slot = match name {
            "url" => &mut self.url,
            "m" => &mut self.mime,
            "dim" => &mut self.dims,
            "x" => &mut self.x,
            "ox" => &mut self.ox,
            "encryption-algorithm" => &mut self.algorithm,
            "decryption-key" => &mut self.key,
            "decryption-nonce" => &mut self.nonce,
            _ => return,
        };
        slot.get_or_insert(value);
    }

    /// The attachment these entries describe, or `None` when one it needs is
    /// missing or malformed.
    fn into_attachment(self) -> Option<Attachment> {
        if self.algorithm? != ATTACHMENT_ALGORITHM {
            return None;
        }
        let ox = match self.ox {
            Some(ox) => Some(decode_hex_array(ox)?),
            None => None,
        };
        Some(Attachment {
            url: self.url?.to_owned(),
            mime: self.mime?.to_owned(),
            dims: self.dims.and_then(parse_dims),
            x: decode_hex_array(self.x?)?,
            ox,
            key: decode_hex_array(self.key?)?,
            nonce: AttachmentNonce::from_hex(self.nonce?)?,
        })
    }
}

/// Parse a `WxH` dimension entry.
fn parse_dims(dims: &str) -> Option<Dims> {
    let (width, height) = dims.split_once('x')?;
    Some(Dims {
        width: width.parse().ok()?,
        height: height.parse().ok()?,
    })
}

/// Decode exactly `N` bytes of hex, or `None` for the wrong length or a
/// non-hex character.
fn decode_hex_array<const N: usize>(hex_str: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    hex::decode_to_slice(hex_str, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nostrdb::NoteBuilder;
    use nostrdb_net::FullKeypair;

    /// A fully populated attachment for tests across the crate.
    pub(crate) fn sample(n: u8) -> Attachment {
        Attachment {
            url: format!("https://blossom.example/{}", hex::encode([n; 32])),
            mime: "image/png".to_owned(),
            dims: Some(Dims {
                width: 640,
                height: 480,
            }),
            x: [n; 32],
            ox: Some([n.wrapping_add(1); 32]),
            key: [n.wrapping_add(2); 32],
            nonce: AttachmentNonce::Gcm128([n.wrapping_add(3); 16]),
        }
    }

    fn built(builder: NoteBuilder<'_>) -> Vec<Attachment> {
        let kp = FullKeypair::generate();
        let note = builder
            .kind(1)
            .content("")
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note");
        note_attachments(&note)
    }

    /// The text form carries everything the tag does, in either nonce length
    /// and without the optional entries, and junk reads as nothing.
    #[test]
    fn imeta_text_round_trips() {
        let a = sample(1);
        let text = a.imeta_text();
        assert!(text.starts_with("url https://blossom.example/"), "{text}");
        assert_eq!(Attachment::from_imeta_text(&text), Some(a));
        let mut b = sample(2);
        b.dims = None;
        b.ox = None;
        b.nonce = AttachmentNonce::Gcm96([9; 12]);
        assert_eq!(Attachment::from_imeta_text(&b.imeta_text()), Some(b));
        assert_eq!(Attachment::from_imeta_text("url https://x"), None);
    }

    #[test]
    fn tag_round_trips() {
        let a = sample(1);
        let mut b = sample(2);
        b.dims = None;
        b.ox = None;
        b.nonce = AttachmentNonce::Gcm96([9; 12]);
        let read = built(write_attachments(
            NoteBuilder::new(),
            &[a.clone(), b.clone()],
        ));
        assert_eq!(read, vec![a, b]);
    }

    #[test]
    fn plain_and_foreign_imetas_are_skipped() {
        let read = built(
            NoteBuilder::new()
                .start_tag()
                .tag_str("imeta")
                .tag_str("url https://x/y.png")
                .tag_str("m image/png")
                .start_tag()
                .tag_str("imeta")
                .tag_str("url https://x/z")
                .tag_str("m image/png")
                .tag_str(&format!("x {}", hex::encode([1u8; 32])))
                .tag_str("encryption-algorithm chacha")
                .tag_str(&format!("decryption-key {}", hex::encode([2u8; 32])))
                .tag_str(&format!("decryption-nonce {}", hex::encode([3u8; 16]))),
        );
        assert!(read.is_empty());
    }
}
