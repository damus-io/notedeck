//! Screenshots pasted or dropped into the card detail.
//!
//! An image comes in two ways: Cmd/Ctrl+V while the description editor or the
//! comment composer has focus (notedeck's egui-winit turns a paste with no
//! text on the clipboard into [`egui::Event::PasteImage`]), or a file dropped
//! anywhere on the detail pane (routed to the description editor when it is
//! open, otherwise to the comment composer).
//!
//! Each image then goes through three steps, none of them on the frame:
//!
//! 1. **Seal**, on the [`JobPool`](notedeck::JobPool): read the dropped file
//!    or PNG-encode the pasted pixels, then encrypt it
//!    ([`encrypted::encrypt`]) under a fresh key.
//! 2. **Upload** the ciphertext to the first server on the account's kind-10063
//!    Blossom list, through the [`BlobUploader`] ([`blossom::upload`] in the
//!    app, a stub in tests).
//! 3. **Land**: the server's URL goes into the draft at the cursor, and the
//!    [`Attachment`] that opens it rides with the draft until the description
//!    commits or the comment posts (see `detail.rs`).
//!
//! While an image is on its way, the field it is for shows a chip with a
//! spinner; a failure shows an inline error instead.

use std::sync::Arc;

use egui::DroppedFileHandle;
use headway::event::{Attachment, AttachmentNonce, Dims};
use nostrdb_net::NoteId;
use notedeck::media::PixelDimensions;
use notedeck::media::blossom::{self, BlobDescriptor, BlossomError, UploadBlob};
use notedeck::media::encrypted::{self, BlobNonce, EncryptedBlob, EncryptedImeta};
use poll_promise::Promise;
use tokio::sync::oneshot;

/// What the account sees when it has no Blossom server to upload to.
pub const NO_SERVER: &str = "Add a Blossom server (kind 10063) to attach images";

/// What a watch-only account sees: an upload has to be signed.
const NO_SIGNER: &str = "Sign in with a private key to attach images";

/// The name a pasted screenshot goes by in its chip.
const PASTED_NAME: &str = "screenshot.png";

/// Uploads a sealed blob to a Blossom server: [`blossom::upload`] in the app.
/// A test swaps in a stub with [`crate::Headway::set_blob_uploader`] so it
/// makes no network call.
pub type BlobUploader =
    Box<dyn Fn(&[u8; 32], &str, UploadBlob) -> Promise<Result<BlobDescriptor, BlossomError>>>;

/// Which field an image is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachTarget {
    /// The card's description (cover note).
    Description,
    /// The comment composer's draft.
    Comment,
}

/// An image on its way into [`seal`].
enum ImageSource {
    /// Pixels from the clipboard, PNG-encoded as they are sealed.
    Pasted(Arc<egui::ColorImage>),
    /// A dropped file, read as it is sealed, and its MIME type.
    Dropped {
        file: DroppedFileHandle,
        mime: String,
    },
}

/// One image pasted or dropped this frame, before it is sealed.
struct Incoming {
    target: AttachTarget,
    /// The file's name, for its chip.
    name: String,
    source: ImageSource,
}

/// An image's plaintext, before [`seal`] encrypts it.
struct Plain {
    bytes: Vec<u8>,
    mime: String,
    dims: Option<Dims>,
}

/// An image after [`seal`]: the encrypted blob and what its imeta says about
/// the plaintext.
pub(crate) struct Sealed {
    pub blob: EncryptedBlob,
    pub mime: String,
    pub dims: Option<Dims>,
}

/// Where the account uploads to, and the key that signs the upload.
pub(crate) struct BlossomTarget {
    pub seckey: [u8; 32],
    pub server: String,
}

/// How far an upload has got.
enum Stage {
    /// Being read and encrypted on the job pool.
    Sealing(oneshot::Receiver<Result<Sealed, String>>),
    /// The ciphertext is on its way to the server.
    Uploading {
        /// Boxed: the key, nonce and hashes outweigh the sealing variant.
        sealed: Box<Sealed>,
        promise: Promise<Result<BlobDescriptor, BlossomError>>,
    },
}

/// One image between the paste or drop and its URL landing in the draft.
struct Upload {
    /// The card whose draft the image is for. An upload that finishes after
    /// the detail has moved to another card is dropped.
    card: NoteId,
    target: AttachTarget,
    /// The chip's text, built once rather than every frame.
    chip: String,
    blossom: BlossomTarget,
    /// `None` only while [`AttachState::poll`] steps it.
    stage: Option<Stage>,
}

/// An inline error under one field.
pub(crate) struct AttachError {
    pub card: NoteId,
    pub target: AttachTarget,
    pub message: String,
}

/// An upload that finished: its attachment, ready to go into the draft.
pub(crate) struct Landed {
    pub card: NoteId,
    pub target: AttachTarget,
    pub attachment: Attachment,
}

/// The detail pane's uploads and the attachments each draft carries, kept in
/// [`BoardUiState`](super::BoardUiState).
pub(crate) struct AttachState {
    uploads: Vec<Upload>,
    /// Uploads that finished and wait for `detail.rs` to put them in a draft.
    landed: Vec<Landed>,
    /// Attachments added to the description since it was last committed.
    pub desc_draft: Vec<Attachment>,
    /// Attachments added to the comment draft.
    pub comment_draft: Vec<Attachment>,
    /// The newest failure, shown under its field until dismissed or replaced.
    pub error: Option<AttachError>,
    uploader: BlobUploader,
}

impl Default for AttachState {
    fn default() -> Self {
        Self {
            uploads: Vec::new(),
            landed: Vec::new(),
            desc_draft: Vec::new(),
            comment_draft: Vec::new(),
            error: None,
            uploader: Box::new(blossom::upload),
        }
    }
}

impl AttachState {
    /// Upload through `uploader` from now on.
    pub fn set_uploader(&mut self, uploader: BlobUploader) {
        self.uploader = uploader;
    }

    /// Forget the drafts' attachments and any error: the detail opened
    /// another card. Uploads still running finish and are dropped.
    pub fn reseed(&mut self) {
        self.desc_draft.clear();
        self.comment_draft.clear();
        self.error = None;
    }

    /// Whether an upload for `target` on `card` is still running.
    pub fn uploading(&self, card: NoteId, target: AttachTarget) -> bool {
        self.uploads
            .iter()
            .any(|u| u.card == card && u.target == target)
    }

    /// The chips for `target` on `card`, oldest first.
    pub fn chips(&self, card: NoteId, target: AttachTarget) -> impl Iterator<Item = &str> {
        self.uploads
            .iter()
            .filter(move |u| u.card == card && u.target == target)
            .map(|u| u.chip.as_str())
    }

    /// The error to show under `target` on `card`, if any.
    pub fn error_for(&self, card: NoteId, target: AttachTarget) -> Option<&str> {
        self.error
            .as_ref()
            .filter(|e| e.card == card && e.target == target)
            .map(|e| e.message.as_str())
    }

    /// Take the uploads that finished for `card`, dropping any for another
    /// card. Allocates nothing when none did.
    pub fn take_landed(&mut self, card: NoteId) -> Vec<Landed> {
        self.landed.retain(|l| l.card == card);
        std::mem::take(&mut self.landed)
    }

    /// Put back a finished upload `detail.rs` could not land this frame.
    pub fn defer(&mut self, landed: Landed) {
        self.landed.push(landed);
    }

    /// Start sealing and uploading the images pasted into the focused field or
    /// dropped on the pane this frame. `editing_desc` routes a drop to the
    /// description editor when it is open.
    pub fn intake(
        &mut self,
        egui_ctx: &egui::Context,
        app_ctx: &mut notedeck::AppContext,
        card: NoteId,
        editing_desc: bool,
    ) {
        let focused = egui_ctx.memory(|m| m.focused());
        let paste_target = if focused == Some(super::detail::comment_field_id(&card)) {
            Some(AttachTarget::Comment)
        } else if editing_desc && focused == Some(super::detail::desc_field_id(&card)) {
            Some(AttachTarget::Description)
        } else {
            None
        };
        let drop_target = if editing_desc {
            AttachTarget::Description
        } else {
            AttachTarget::Comment
        };

        // Both lists are empty on almost every frame, and an empty Vec's clone
        // allocates nothing.
        let mut incoming: Vec<Incoming> = Vec::new();
        egui_ctx.input(|i| {
            if let Some(target) = paste_target {
                for event in &i.events {
                    if let egui::Event::PasteImage(image) = event {
                        incoming.push(Incoming {
                            target,
                            name: PASTED_NAME.to_owned(),
                            source: ImageSource::Pasted(image.clone()),
                        });
                    }
                }
            }
            for file in &i.raw.dropped_files {
                let name = file
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_owned();
                let Ok(mime) = image_mime(file.path()) else {
                    self.error = Some(AttachError {
                        card,
                        target: drop_target,
                        message: format!("{name} is not an image"),
                    });
                    continue;
                };
                incoming.push(Incoming {
                    target: drop_target,
                    name,
                    source: ImageSource::Dropped {
                        file: file.clone(),
                        mime,
                    },
                });
            }
        });

        for image in incoming {
            self.start(app_ctx, card, image);
        }
    }

    /// Seal `source` on the job pool, to upload once it is sealed.
    fn start(&mut self, app_ctx: &mut notedeck::AppContext, card: NoteId, image: Incoming) {
        let Incoming {
            target,
            name,
            source,
        } = image;
        let blossom = match blossom_target(app_ctx.ndb, app_ctx.accounts) {
            Ok(blossom) => blossom,
            Err(message) => {
                self.error = Some(AttachError {
                    card,
                    target,
                    message: message.to_owned(),
                });
                return;
            }
        };
        if self
            .error
            .as_ref()
            .is_some_and(|e| e.card == card && e.target == target)
        {
            self.error = None;
        }
        let rx = app_ctx.job_pool.schedule_receivable(move || seal(source));
        self.uploads.push(Upload {
            card,
            target,
            chip: format!("Uploading {name}…"),
            blossom,
            stage: Some(Stage::Sealing(rx)),
        });
    }

    /// Step every running upload: start the upload of a blob that finished
    /// sealing, and land or fail one the server answered. Asks for a repaint
    /// while any is running, since neither the job pool nor the upload wakes
    /// the UI when it finishes.
    pub fn poll(&mut self, egui_ctx: &egui::Context) {
        if self.uploads.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.uploads.len() {
            let upload = &mut self.uploads[i];
            let stage = upload.stage.take().expect("a stage between polls");
            match step(stage, &upload.blossom, &self.uploader) {
                Step::Running(stage) => {
                    upload.stage = Some(stage);
                    i += 1;
                }
                Step::Landed(attachment) => {
                    let upload = self.uploads.remove(i);
                    self.landed.push(Landed {
                        card: upload.card,
                        target: upload.target,
                        attachment,
                    });
                }
                Step::Failed(message) => {
                    let upload = self.uploads.remove(i);
                    self.error = Some(AttachError {
                        card: upload.card,
                        target: upload.target,
                        message,
                    });
                }
            }
        }
        if !self.uploads.is_empty() {
            egui_ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

/// What one [`step`] of an upload came to.
enum Step {
    Running(Stage),
    Landed(Attachment),
    Failed(String),
}

/// Move one upload on, if its job or request has finished.
fn step(stage: Stage, blossom: &BlossomTarget, uploader: &BlobUploader) -> Step {
    match stage {
        Stage::Sealing(mut rx) => match rx.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => Step::Running(Stage::Sealing(rx)),
            Err(oneshot::error::TryRecvError::Closed) => {
                Step::Failed("The image could not be prepared".to_owned())
            }
            Ok(Err(message)) => Step::Failed(message),
            Ok(Ok(mut sealed)) => {
                let bytes = std::mem::take(&mut sealed.blob.ciphertext);
                let promise = uploader(
                    &blossom.seckey,
                    &blossom.server,
                    UploadBlob {
                        bytes,
                        content_type: "application/octet-stream".to_owned(),
                    },
                );
                Step::Running(Stage::Uploading {
                    sealed: Box::new(sealed),
                    promise,
                })
            }
        },
        Stage::Uploading { sealed, promise } => match promise.try_take() {
            Err(promise) => Step::Running(Stage::Uploading { sealed, promise }),
            Ok(Err(err)) => Step::Failed(format!("Upload failed: {err}")),
            Ok(Ok(descriptor)) => Step::Landed(attachment(*sealed, descriptor.url)),
        },
    }
}

/// The selected account's upload key and the first server on its Blossom
/// list, or what to tell the user when it has neither.
pub(crate) fn blossom_target(
    ndb: &nostrdb::Ndb,
    accounts: &notedeck::Accounts,
) -> Result<BlossomTarget, &'static str> {
    let Some(filled) = accounts.selected_filled() else {
        return Err(NO_SIGNER);
    };
    let seckey = filled.secret_key.secret_bytes();
    let pubkey = *filled.pubkey.bytes();
    let txn = nostrdb::Transaction::new(ndb).map_err(|_| NO_SERVER)?;
    let server = blossom::server_list(ndb, &txn, &pubkey)
        .and_then(|list| list.first().map(str::to_owned))
        .ok_or(NO_SERVER)?;
    Ok(BlossomTarget { seckey, server })
}

/// Read or encode `source` and encrypt it. Runs on the job pool.
fn seal(source: ImageSource) -> Result<Sealed, String> {
    let Plain { bytes, mime, dims } = plaintext(source)?;
    let blob = encrypted::encrypt(&bytes).map_err(|e| e.to_string())?;
    Ok(Sealed { blob, mime, dims })
}

/// The bytes `source` uploads as: the dropped file as it is, or the pasted
/// pixels as a PNG.
fn plaintext(source: ImageSource) -> Result<Plain, String> {
    match source {
        ImageSource::Pasted(image) => {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|c| c.to_srgba_unmultiplied())
                .collect();
            let png = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                .ok_or("The pasted image has the wrong size")?;
            let mut bytes = Vec::new();
            png.write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .map_err(|e| format!("The pasted image could not be encoded: {e}"))?;
            let dims = Dims {
                width: w as u32,
                height: h as u32,
            };
            Ok(Plain {
                bytes,
                mime: "image/png".to_owned(),
                dims: Some(dims),
            })
        }
        ImageSource::Dropped { file, mime } => {
            let bytes = file.bytes()?;
            let dims = dims_of(&bytes);
            Ok(Plain { bytes, mime, dims })
        }
    }
}

/// The pixel size of the image `bytes` hold, read from its header.
fn dims_of(bytes: &[u8]) -> Option<Dims> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(width, height)| Dims { width, height })
}

/// The MIME type of the image at `path`, by its extension, or why it can't
/// be attached.
pub(crate) fn image_mime(path: &std::path::Path) -> Result<String, String> {
    notedeck::platform::file::SelectedMedia::from_path(path.to_path_buf())
        .ok()
        .map(|m| m.media_type.to_mime().to_owned())
        .filter(|m| m.starts_with("image/"))
        .ok_or_else(|| format!("{} is not an image", path.display()))
}

/// Read the image at `path`, whose type is `mime`, and encrypt it, as a
/// dropped file is sealed. Blocks on the read, so run it off the render loop.
pub(crate) fn seal_path(path: &std::path::Path, mime: String) -> Result<Sealed, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let dims = dims_of(&bytes);
    let blob = encrypted::encrypt(&bytes).map_err(|e| e.to_string())?;
    Ok(Sealed { blob, mime, dims })
}

/// The attachment for `sealed`, now that the server stored it at `url`.
pub(crate) fn attachment(sealed: Sealed, url: String) -> Attachment {
    let blob = sealed.blob;
    Attachment {
        url,
        mime: sealed.mime,
        dims: sealed.dims,
        x: blob.x,
        ox: Some(blob.ox),
        key: blob.key,
        nonce: match blob.nonce {
            BlobNonce::Gcm96(n) => AttachmentNonce::Gcm96(n),
            BlobNonce::Gcm128(n) => AttachmentNonce::Gcm128(n),
        },
    }
}

/// `attachment` as the image cache's [`EncryptedImeta`], to register its URL
/// so the cache fetches it as ciphertext and decrypts it.
pub(crate) fn imeta(attachment: &Attachment) -> EncryptedImeta<'_> {
    EncryptedImeta {
        url: attachment.url.as_str().into(),
        mime: attachment.mime.as_str().into(),
        dims: attachment.dims.map(|d| PixelDimensions {
            x: d.width,
            y: d.height,
        }),
        x: attachment.x,
        ox: attachment.ox,
        key: attachment.key,
        nonce: match attachment.nonce {
            AttachmentNonce::Gcm96(n) => BlobNonce::Gcm96(n),
            AttachmentNonce::Gcm128(n) => BlobNonce::Gcm128(n),
        },
    }
}

/// Register every attachment in `attachments` with the image cache.
pub(crate) fn register<'a>(
    img_cache: &mut notedeck::Images,
    attachments: impl IntoIterator<Item = &'a Attachment>,
) {
    for attachment in attachments {
        img_cache.encrypted.register(&imeta(attachment));
    }
}

/// The text that points at `url` in `target`: a markdown image for a
/// description, the bare URL for a comment (which the note renderer turns into
/// media).
pub(crate) fn reference(target: AttachTarget, url: &str) -> String {
    match target {
        AttachTarget::Description => format!("![image]({url})"),
        AttachTarget::Comment => url.to_owned(),
    }
}

/// Insert `snippet` into `buf` at the cursor of the text field `id` (at the
/// end when the field has no cursor), on a line of its own: a blank line
/// around it when `paragraph`, so markdown draws it as its own block. The
/// cursor moves to just after it.
pub(crate) fn insert_at_cursor(
    egui_ctx: &egui::Context,
    id: egui::Id,
    buf: &mut String,
    snippet: &str,
    paragraph: bool,
) {
    let mut state = egui::text_edit::TextEditState::load(egui_ctx, id);
    let char_at = state
        .as_ref()
        .and_then(|s| s.cursor.char_range())
        .map(|r| r.primary.index.0);
    let at = char_at
        .and_then(|c| buf.char_indices().nth(c).map(|(b, _)| b))
        .unwrap_or(buf.len());

    let (before, after) = buf.split_at(at);
    let gap = if paragraph { "\n\n" } else { "\n" };
    let lead = pad_before(before, gap);
    let trail = pad_after(after, gap);
    let inserted = format!("{lead}{snippet}{trail}");
    let cursor = before.chars().count() + lead.chars().count() + snippet.chars().count();
    buf.insert_str(at, &inserted);

    if let Some(state) = state.as_mut() {
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(cursor),
            )));
        state.clone().store(egui_ctx, id);
    }
}

/// The newlines `before` needs so what follows starts `gap` after any text.
fn pad_before<'a>(before: &str, gap: &'a str) -> &'a str {
    if before.is_empty() {
        return "";
    }
    let have = before.len() - before.trim_end_matches('\n').len();
    &gap[have.min(gap.len())..]
}

/// The newlines needed after an insertion so `after` starts `gap` below it.
fn pad_after<'a>(after: &str, gap: &'a str) -> &'a str {
    if after.is_empty() {
        return "";
    }
    let have = after.len() - after.trim_start_matches('\n').len();
    &gap[have.min(gap.len())..]
}

/// `draft`'s attachments that `text` still points at: one deleted from the
/// text before it was committed is dropped with it.
pub(crate) fn referenced<'a>(
    draft: &'a [Attachment],
    text: &'a str,
) -> impl Iterator<Item = &'a Attachment> + 'a {
    draft.iter().filter(move |a| text.contains(a.url.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The padding puts a snippet on its own line, or its own paragraph, and
    /// never doubles a newline already there.
    #[test]
    fn padding_makes_a_line_of_its_own() {
        assert_eq!(pad_before("", "\n\n"), "");
        assert_eq!(pad_before("text", "\n\n"), "\n\n");
        assert_eq!(pad_before("text\n", "\n\n"), "\n");
        assert_eq!(pad_before("text\n\n", "\n\n"), "");
        assert_eq!(pad_after("", "\n"), "");
        assert_eq!(pad_after("more", "\n"), "\n");
        assert_eq!(pad_after("\nmore", "\n"), "");
    }

    /// Without a cursor the snippet goes at the end, on its own paragraph.
    #[test]
    fn insert_without_a_cursor_appends() {
        let ctx = egui::Context::default();
        let mut buf = "Steps to reproduce".to_owned();
        insert_at_cursor(&ctx, egui::Id::unique("x"), &mut buf, "![image](u)", true);
        assert_eq!(buf, "Steps to reproduce\n\n![image](u)");
    }

    /// A sealed blob's attachment carries its key and hashes, and converts to
    /// the image cache's imeta and back without loss.
    #[test]
    fn attachment_matches_the_blob() {
        let blob = encrypted::encrypt(b"png bytes").expect("encrypt");
        let sealed = Sealed {
            blob: blob.clone(),
            mime: "image/png".to_owned(),
            dims: Some(Dims {
                width: 3,
                height: 2,
            }),
        };
        let a = attachment(sealed, "https://blossom.test/x".to_owned());
        assert_eq!(a.key, blob.key);
        assert_eq!(a.x, blob.x);
        let imeta = imeta(&a);
        assert_eq!(imeta.key, blob.key);
        assert_eq!(imeta.nonce, blob.nonce);
        assert_eq!(imeta.dims, Some(PixelDimensions { x: 3, y: 2 }));
        let plain = encrypted::decrypt(&blob.ciphertext, &imeta.key, &imeta.nonce).unwrap();
        assert_eq!(plain, b"png bytes");
    }
}
