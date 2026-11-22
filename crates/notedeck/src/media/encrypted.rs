//! Encrypted media blobs, in the AES-GCM file scheme of NIP-17's kind-15
//! file messages.
//!
//! A blob is encrypted on this device and uploaded as ciphertext, so the
//! server that stores it never sees the file. The key travels in the note
//! that points at the blob, in one NIP-92 `imeta` tag per attachment:
//!
//! ```text
//! ["imeta", "url https://server/<x>", "m image/png", "dim 640x480",
//!  "x <sha256 of ciphertext>", "ox <sha256 of plaintext>",
//!  "encryption-algorithm aes-gcm",
//!  "decryption-key <hex>", "decryption-nonce <hex>"]
//! ```
//!
//! The parameters match Amethyst's `AESGCM` (vitorpamplona/crypto-kmp), so a
//! blob either client writes the other can read. That is AES-256 with a
//! 32-byte key, a 16-byte nonce and a 16-byte tag appended to the
//! ciphertext, with key and nonce written as lowercase hex. Amethyst accepts
//! any nonce length when decrypting. This module reads the standard 12-byte
//! nonce as well as the 16-byte one, and always writes 16.
//!
//! NIP-44 is not an option here because it caps plaintext at 64 KB, and a
//! screenshot is larger than that.

use std::borrow::Cow;
use std::collections::HashMap;

use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::aes::Aes256;
use aes_gcm::AesGcm;
use nostrdb::{Note, NoteBuilder, Tag};
use sha2::{Digest, Sha256};

use crate::media::PixelDimensions;
use crate::MediaCacheType;

/// The `encryption-algorithm` value for this scheme.
pub const ALGORITHM: &str = "aes-gcm";

/// AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;

/// The nonce length written by [`encrypt`], which is Amethyst's default.
pub const NONCE_LEN: usize = 16;

/// AES-256-GCM with Amethyst's 16-byte nonce.
type Aes256Gcm128 = AesGcm<Aes256, U16>;

/// AES-256-GCM with the standard 12-byte nonce.
type Aes256Gcm96 = AesGcm<Aes256, U12>;

/// A blob's AES-256 key.
pub type BlobKey = [u8; KEY_LEN];

/// A SHA-256 digest: the `x` (ciphertext) or `ox` (plaintext) of a blob.
pub type Sha256Hash = [u8; 32];

/// A GCM nonce in one of the two lengths a blob can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobNonce {
    /// The standard 96-bit nonce, which some other clients write.
    Gcm96([u8; 12]),
    /// Amethyst's 128-bit nonce, which [`encrypt`] writes.
    Gcm128([u8; NONCE_LEN]),
}

impl BlobNonce {
    /// The nonce's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Gcm96(bytes) => bytes,
            Self::Gcm128(bytes) => bytes,
        }
    }

    /// Parse a nonce from hex: 24 characters for a 12-byte nonce, 32 for a
    /// 16-byte one. Any other length is rejected.
    pub fn from_hex(hex_str: &str) -> Option<Self> {
        match hex_str.len() {
            24 => decode_hex_array(hex_str).map(Self::Gcm96),
            32 => decode_hex_array(hex_str).map(Self::Gcm128),
            _ => None,
        }
    }
}

/// Why a blob could not be encrypted or decrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlobCryptError {
    /// AES-GCM refused the input. On decryption this means the key or the
    /// nonce is wrong or the ciphertext was altered, and the three can't
    /// be told apart. On encryption it means the plaintext is longer than
    /// GCM allows.
    #[error("aes-gcm failed: wrong key or nonce, or the blob was altered")]
    Aead,
}

/// A blob after [`encrypt`]: the ciphertext to upload, the secrets that
/// open it, and the two hashes its `imeta` tag carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedBlob {
    /// The bytes to upload. The 16-byte GCM tag is on the end.
    pub ciphertext: Vec<u8>,
    /// The freshly generated key.
    pub key: BlobKey,
    /// The freshly generated nonce. Always [`BlobNonce::Gcm128`].
    pub nonce: BlobNonce,
    /// SHA-256 of `ciphertext`. A Blossom server names the blob by it.
    pub x: Sha256Hash,
    /// SHA-256 of the plaintext.
    pub ox: Sha256Hash,
}

/// Encrypt `plaintext` under a new random key and nonce.
///
/// Each call draws its own key, so no key and nonce pair is ever used
/// twice.
pub fn encrypt(plaintext: &[u8]) -> Result<EncryptedBlob, BlobCryptError> {
    let mut key = [0u8; KEY_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut key);
    OsRng.fill_bytes(&mut nonce);

    let ciphertext = Aes256Gcm128::new(&key.into())
        .encrypt(&nonce.into(), plaintext)
        .map_err(|_| BlobCryptError::Aead)?;

    Ok(EncryptedBlob {
        x: sha256(&ciphertext),
        ox: sha256(plaintext),
        ciphertext,
        key,
        nonce: BlobNonce::Gcm128(nonce),
    })
}

/// Decrypt a blob's `ciphertext` with its key and nonce and return the
/// plaintext. The GCM tag must be on the end of the ciphertext, which is
/// where [`encrypt`] and Amethyst put it.
pub fn decrypt(
    ciphertext: &[u8],
    key: &BlobKey,
    nonce: &BlobNonce,
) -> Result<Vec<u8>, BlobCryptError> {
    let plain = match nonce {
        BlobNonce::Gcm96(n) => Aes256Gcm96::new(key.into()).decrypt(n.into(), ciphertext),
        BlobNonce::Gcm128(n) => Aes256Gcm128::new(key.into()).decrypt(n.into(), ciphertext),
    };
    plain.map_err(|_| BlobCryptError::Aead)
}

/// SHA-256 of `bytes`.
fn sha256(bytes: &[u8]) -> Sha256Hash {
    Sha256::digest(bytes).into()
}

/// One encrypted attachment as a NIP-92 `imeta` tag: where the ciphertext
/// lives, what it decrypts to, and the key that opens it.
///
/// The strings are a [`Cow`], so a tag read from a note borrows from the
/// transaction and copies nothing, and [`Self::into_owned`] detaches it
/// for a cache that outlives the transaction. Hashes, key and nonce are
/// decoded from hex as the tag is read, so a malformed one never parses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedImeta<'a> {
    /// The ciphertext's URL, usually `https://<blossom server>/<x>`.
    pub url: Cow<'a, str>,
    /// The plaintext's MIME type (`m`), e.g. `image/png`.
    pub mime: Cow<'a, str>,
    /// The image's pixel size (`dim WxH`), when known.
    pub dims: Option<PixelDimensions>,
    /// SHA-256 of the ciphertext.
    pub x: Sha256Hash,
    /// SHA-256 of the plaintext, when the writer gave one.
    pub ox: Option<Sha256Hash>,
    /// The AES-256 key (`decryption-key`).
    pub key: BlobKey,
    /// The GCM nonce (`decryption-nonce`).
    pub nonce: BlobNonce,
}

impl<'a> EncryptedImeta<'a> {
    /// Describe `blob`, which was uploaded to `url` and decrypts to a file of
    /// type `mime`.
    pub fn new(
        url: impl Into<Cow<'a, str>>,
        mime: impl Into<Cow<'a, str>>,
        dims: Option<PixelDimensions>,
        blob: &EncryptedBlob,
    ) -> Self {
        Self {
            url: url.into(),
            mime: mime.into(),
            dims,
            x: blob.x,
            ox: Some(blob.ox),
            key: blob.key,
            nonce: blob.nonce,
        }
    }

    /// The cache that draws this attachment: the static image cache when
    /// its `m` is an image, nothing otherwise. A GIF draws its first frame,
    /// since only static images are decrypted.
    pub fn cache_type(&self) -> Option<MediaCacheType> {
        self.mime
            .starts_with("image/")
            .then_some(MediaCacheType::Image)
    }

    /// The same attachment with its strings copied out, so it no longer
    /// borrows from a transaction.
    pub fn into_owned(self) -> EncryptedImeta<'static> {
        EncryptedImeta {
            url: Cow::Owned(self.url.into_owned()),
            mime: Cow::Owned(self.mime.into_owned()),
            dims: self.dims,
            x: self.x,
            ox: self.ox,
            key: self.key,
            nonce: self.nonce,
        }
    }

    /// Append this attachment to `builder` as one `imeta` tag.
    pub fn write_tag<'b>(&self, builder: NoteBuilder<'b>) -> NoteBuilder<'b> {
        let mut builder = builder
            .start_tag()
            .tag_str("imeta")
            .tag_str(&format!("url {}", self.url))
            .tag_str(&format!("m {}", self.mime));
        if let Some(dims) = self.dims {
            builder = builder.tag_str(&format!("dim {}x{}", dims.x, dims.y));
        }
        builder = builder.tag_str(&format!("x {}", hex::encode(self.x)));
        if let Some(ox) = self.ox {
            builder = builder.tag_str(&format!("ox {}", hex::encode(ox)));
        }
        builder
            .tag_str(&format!("encryption-algorithm {ALGORITHM}"))
            .tag_str(&format!("decryption-key {}", hex::encode(self.key)))
            .tag_str(&format!(
                "decryption-nonce {}",
                hex::encode(self.nonce.as_bytes())
            ))
    }

    /// Read an encrypted attachment from one tag.
    ///
    /// Returns `None` for a tag that is not an `imeta`, a plain imeta with no
    /// encryption, one whose algorithm is not `aes-gcm`, and one missing any
    /// of `url`, `m`, `x`, `decryption-key` or `decryption-nonce` or with
    /// one of them malformed. Unknown entries (`blurhash`, `size`, ...) are
    /// skipped.
    pub fn from_tag(tag: &Tag<'a>) -> Option<Self> {
        if tag.get_str(0) != Some("imeta") {
            return None;
        }

        let mut fields = ImetaFields::default();
        for index in 1..tag.count() {
            let Some(entry) = tag.get_str(index) else {
                continue;
            };
            let Some((name, value)) = entry.split_once(' ') else {
                continue;
            };
            fields.set(name, value);
        }
        fields.into_imeta()
    }
}

/// The entries of one `imeta` tag as they are found, before they are
/// checked for completeness by [`ImetaFields::into_imeta`].
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
    fn into_imeta(self) -> Option<EncryptedImeta<'a>> {
        if self.algorithm? != ALGORITHM {
            return None;
        }
        Some(EncryptedImeta {
            url: Cow::Borrowed(self.url?),
            mime: Cow::Borrowed(self.mime?),
            dims: self.dims.and_then(parse_dims),
            x: decode_hex_array(self.x?)?,
            ox: match self.ox {
                Some(ox) => Some(decode_hex_array(ox)?),
                None => None,
            },
            key: decode_hex_array(self.key?)?,
            nonce: BlobNonce::from_hex(self.nonce?)?,
        })
    }
}

/// Every encrypted attachment on `note`, in tag order, borrowing from the
/// note's transaction.
pub fn encrypted_imetas<'a>(note: &Note<'a>) -> impl Iterator<Item = EncryptedImeta<'a>> {
    note.tags()
        .into_iter()
        .filter_map(|tag| EncryptedImeta::from_tag(&tag))
}

/// The encrypted attachments the image cache can open, by ciphertext URL.
///
/// [`crate::Images`] keeps one. A URL registered here is fetched as
/// ciphertext, decrypted off the UI thread with its key, and parsed as its
/// `m` type; it is drawn as an image without a MIME probe, since a Blossom
/// URL often has no extension and its server reports the ciphertext as
/// `application/octet-stream`.
///
/// Register a URL before its image is first requested. A fetch already
/// made without the key has cached its error and is not retried.
#[derive(Default)]
pub struct EncryptedMedia {
    by_url: HashMap<String, EncryptedImeta<'static>>,
}

impl EncryptedMedia {
    /// Remember how to open `imeta.url`.
    ///
    /// Registering the same attachment again copies nothing, so a view may
    /// call this every frame. A URL registered with a different key or type
    /// is replaced.
    pub fn register(&mut self, imeta: &EncryptedImeta<'_>) {
        if self.by_url.get(imeta.url.as_ref()) == Some(imeta) {
            return;
        }
        let owned = imeta.clone().into_owned();
        self.by_url.insert(owned.url.to_string(), owned);
    }

    /// Register every encrypted attachment on `note`, as
    /// [`crate::update_imeta_blurhashes`] does for blurhashes.
    pub fn register_note(&mut self, note: &Note<'_>) {
        for imeta in encrypted_imetas(note) {
            self.register(&imeta);
        }
    }

    /// The attachment registered at `url`, if any.
    pub fn get(&self, url: &str) -> Option<&EncryptedImeta<'static>> {
        self.by_url.get(url)
    }
}

/// Parse a `WxH` dimension, as in `dim 640x480`.
fn parse_dims(dims: &str) -> Option<PixelDimensions> {
    let (x, y) = dims.split_once('x')?;
    Some(PixelDimensions {
        x: x.parse().ok()?,
        y: y.parse().ok()?,
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
mod tests {
    use super::*;

    const PLAIN: &[u8] = b"\x89PNG\r\n\x1a\n not really a png, but bytes all the same";

    #[test]
    fn round_trip() {
        let blob = encrypt(PLAIN).expect("encrypt");
        assert_ne!(blob.ciphertext, PLAIN);
        // The 16-byte GCM tag rides on the end.
        assert_eq!(blob.ciphertext.len(), PLAIN.len() + 16);
        assert_eq!(blob.x, sha256(&blob.ciphertext));
        assert_eq!(blob.ox, sha256(PLAIN));
        assert!(matches!(blob.nonce, BlobNonce::Gcm128(_)));

        let plain = decrypt(&blob.ciphertext, &blob.key, &blob.nonce).expect("decrypt");
        assert_eq!(plain, PLAIN);
    }

    #[test]
    fn each_encryption_draws_a_new_key_and_nonce() {
        let a = encrypt(PLAIN).expect("encrypt");
        let b = encrypt(PLAIN).expect("encrypt");
        assert_ne!(a.key, b.key);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn wrong_key_fails() {
        let blob = encrypt(PLAIN).expect("encrypt");
        let mut key = blob.key;
        key[0] ^= 1;
        assert_eq!(
            decrypt(&blob.ciphertext, &key, &blob.nonce),
            Err(BlobCryptError::Aead)
        );
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let blob = encrypt(PLAIN).expect("encrypt");
        let mut ciphertext = blob.ciphertext.clone();
        ciphertext[3] ^= 0x40;
        assert_eq!(
            decrypt(&ciphertext, &blob.key, &blob.nonce),
            Err(BlobCryptError::Aead)
        );
    }

    #[test]
    fn decrypts_a_twelve_byte_nonce() {
        let key = [7u8; KEY_LEN];
        let nonce = [9u8; 12];
        let ciphertext = Aes256Gcm96::new(&key.into())
            .encrypt(&nonce.into(), PLAIN)
            .expect("encrypt");
        let plain = decrypt(&ciphertext, &key, &BlobNonce::Gcm96(nonce)).expect("decrypt");
        assert_eq!(plain, PLAIN);
    }

    /// Test Case 15 of the GCM specification (McGrew & Viega: AES-256,
    /// 96-bit IV, no AAD). It pins the `ciphertext || tag` layout, which is
    /// what Java's `AES/GCM/NoPadding` writes and Amethyst decrypts.
    #[test]
    fn matches_a_known_answer_vector() {
        let key: BlobKey =
            decode_hex_array("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308")
                .unwrap();
        let nonce = BlobNonce::from_hex("cafebabefacedbaddecaf888").unwrap();
        let plain = hex::decode(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
             1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
        )
        .unwrap();
        let expected = hex::decode(
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
             8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad\
             b094dac5d93471bdec1a502270e3cc6c",
        )
        .unwrap();

        let ciphertext = Aes256Gcm96::new(&key.into())
            .encrypt(
                &decode_hex_array::<12>("cafebabefacedbaddecaf888")
                    .unwrap()
                    .into(),
                plain.as_slice(),
            )
            .unwrap();
        assert_eq!(ciphertext, expected);
        assert_eq!(decrypt(&expected, &key, &nonce).unwrap(), plain);
    }

    #[test]
    fn imeta_tag_round_trips_through_a_note() {
        let blob = encrypt(PLAIN).expect("encrypt");
        let url = format!("https://blossom.example/{}", hex::encode(blob.x));
        let written = EncryptedImeta::new(
            url.as_str(),
            "image/png",
            Some(PixelDimensions { x: 640, y: 480 }),
            &blob,
        );

        // A plain imeta beside it is not an encrypted attachment.
        let builder = NoteBuilder::new()
            .kind(1111)
            .content("see attached")
            .start_tag()
            .tag_str("imeta")
            .tag_str("url https://example.com/a.png")
            .tag_str("m image/png");
        let note = written
            .write_tag(builder)
            .sign(&[3u8; 32])
            .build()
            .expect("note");

        let read: Vec<_> = encrypted_imetas(&note).collect();
        assert_eq!(read, vec![written.clone()]);
        assert!(matches!(read[0].url, Cow::Borrowed(_)));

        let plain = decrypt(&blob.ciphertext, &read[0].key, &read[0].nonce).unwrap();
        assert_eq!(plain, PLAIN);
    }

    /// Parse one tag built from `entries` in a note that isn't stored.
    fn parse_entries(entries: &[&str]) -> Option<EncryptedImeta<'static>> {
        let mut builder = NoteBuilder::new().kind(1).content("").start_tag();
        for entry in entries {
            builder = builder.tag_str(entry);
        }
        let note = builder.sign(&[3u8; 32]).build().expect("note");
        let tag = note.tags().into_iter().next().expect("tag");
        EncryptedImeta::from_tag(&tag).map(EncryptedImeta::into_owned)
    }

    #[test]
    fn rejects_incomplete_or_foreign_tags() {
        let key = format!("decryption-key {}", "11".repeat(32));
        let nonce = format!("decryption-nonce {}", "22".repeat(16));
        let x = format!("x {}", "33".repeat(32));
        let complete = [
            "imeta",
            "url https://b.example/f",
            "m image/png",
            x.as_str(),
            "encryption-algorithm aes-gcm",
            key.as_str(),
            nonce.as_str(),
        ];
        let parsed = parse_entries(&complete).expect("complete tag parses");
        assert_eq!(parsed.dims, None);
        assert_eq!(parsed.ox, None);
        assert_eq!(parsed.nonce, BlobNonce::Gcm128([0x22; 16]));

        // Dropping any required entry, or naming another algorithm, rejects it.
        for skip in 1..complete.len() {
            let entries: Vec<&str> = complete
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, e)| *e)
                .collect();
            assert_eq!(parse_entries(&entries), None, "missing {}", complete[skip]);
        }
        let mut other_alg = complete;
        other_alg[4] = "encryption-algorithm chacha20";
        assert_eq!(parse_entries(&other_alg), None);

        let mut short_nonce = complete;
        short_nonce[6] = "decryption-nonce 2222";
        assert_eq!(parse_entries(&short_nonce), None);

        let mut not_imeta = complete;
        not_imeta[0] = "image";
        assert_eq!(parse_entries(&not_imeta), None);
    }

    #[test]
    fn encrypted_media_registers_from_a_note_and_replaces_a_changed_key() {
        let blob = encrypt(PLAIN).expect("encrypt");
        let url = format!("https://blossom.example/{}", hex::encode(blob.x));
        let imeta = EncryptedImeta::new(url.as_str(), "image/png", None, &blob);
        let note = imeta
            .write_tag(NoteBuilder::new().kind(1111).content("see attached"))
            .sign(&[3u8; 32])
            .build()
            .expect("note");

        let mut media = EncryptedMedia::default();
        assert!(media.get(&url).is_none());
        media.register_note(&note);
        assert_eq!(media.get(&url), Some(&imeta.clone().into_owned()));

        // Registering it again changes nothing; a new key replaces the old.
        media.register(&imeta);
        let mut rekeyed = imeta.clone();
        rekeyed.key[0] ^= 1;
        media.register(&rekeyed);
        assert_eq!(media.get(&url).map(|m| m.key), Some(rekeyed.key));
    }

    #[test]
    fn only_an_image_type_is_drawn() {
        let blob = encrypt(PLAIN).expect("encrypt");
        let image = EncryptedImeta::new("https://b.example/x", "image/png", None, &blob);
        let gif = EncryptedImeta::new("https://b.example/y", "image/gif", None, &blob);
        let video = EncryptedImeta::new("https://b.example/z", "video/mp4", None, &blob);
        assert_eq!(image.cache_type(), Some(MediaCacheType::Image));
        // Only static images are decrypted, so a GIF draws its first frame.
        assert_eq!(gif.cache_type(), Some(MediaCacheType::Image));
        assert_eq!(video.cache_type(), None);
    }
}
