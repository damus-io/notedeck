//! `headway_attach`: encrypt image files and upload them to the account's
//! Blossom server, as a screenshot pasted into the card detail is, and answer
//! what a comment or description needs to carry them.
//!
//! It is a [`SlowTool`] because the upload is a network round trip, and the
//! comment and description tools are not: they write on the render loop with
//! the [`ToolContext`], which a slow tool's job never gets back. So the two
//! halves are two tools. `nd headway comment <card> --attach <paths>` chains
//! them: the comment tool's `attach` argument is marked
//! [`via`](notedeck::ToolArg::via) this tool, so `nd` uploads first and sends
//! the comment with the `attachments` this tool answered. A model calls the
//! two itself.
//!
//! An attachment crosses between them as text, its `imeta` entries one per
//! line ([`Attachment::imeta_text`]): the shape the event stores it in, so
//! nothing invents a second encoding of the key.

use std::path::PathBuf;
use std::sync::Arc;

use headway::event::Attachment;
use notedeck::media::blossom::{self, BlobDescriptor, BlossomError, UploadBlob};
use notedeck::{SlowTool, ToolArg, ToolArgType, ToolContext, ToolJob, ToolSpec};
use poll_promise::Promise;
use serde::{Deserialize, Serialize};

use crate::ui::attach::{self as seal, AttachTarget};

/// Uploads a sealed blob to a Blossom server, as
/// [`BlobUploader`](crate::ui::BlobUploader) does for the detail pane, but
/// shareable across the threads a slow tool's job runs on.
pub(crate) type SharedUploader = Arc<
    dyn Fn(&[u8; 32], &str, UploadBlob) -> Promise<Result<BlobDescriptor, BlossomError>>
        + Send
        + Sync,
>;

/// `headway_attach`: upload image files, answer their attachments.
pub(crate) struct Attach {
    upload: SharedUploader,
}

impl Default for Attach {
    /// Uploading with [`blossom::upload`].
    fn default() -> Self {
        Self::with_uploader(Arc::new(blossom::upload))
    }
}

impl Attach {
    /// Upload through `upload`: a stub, in a test, so it makes no network
    /// call.
    pub(crate) fn with_uploader(upload: SharedUploader) -> Self {
        Self { upload }
    }
}

/// Arguments for [`Attach`].
#[derive(Deserialize)]
pub(crate) struct AttachArgs {
    paths: Vec<String>,
}

/// What [`Attach`] answers: each file's attachment as text
/// ([`Attachment::imeta_text`]), in the order the paths were given, which
/// is the `attachments` argument the comment and description tools take.
#[derive(Serialize)]
pub(crate) struct Attached {
    attachments: Vec<String>,
    /// Each attachment's URL, for the text `nd headway attach` prints.
    #[serde(skip)]
    urls: Vec<String>,
}

/// One file to upload, checked in `prepare`.
struct Upload {
    path: PathBuf,
    mime: String,
}

impl SlowTool for Attach {
    type Args = AttachArgs;
    type Output = Attached;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_attach",
            "Encrypt image files (screenshots) and upload them to the first server on the account's kind-10063 Blossom list. Answers `{attachments}`: one per file, its `imeta` entries a line each, to pass as `attachments` to `headway_comment_card` or `headway_set_description`, which put the image in the text and its key on the event. Fails when the account has no Blossom list.",
            vec![
                ToolArg::new("paths", ToolArgType::Paths, "The image files to upload.")
                    .required(true)
                    .rest(),
            ],
        )
        .titled("Upload attachments")
        .command(&["headway", "attach"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    /// Checks the files are images and finds the server and the signing
    /// key, so a bad call fails before anything is read; the job reads,
    /// encrypts and uploads each file in turn.
    fn prepare(&self, cx: &mut ToolContext, args: AttachArgs) -> Result<ToolJob<Attached>, String> {
        if args.paths.is_empty() {
            return Err("name at least one image file to attach".to_owned());
        }
        let uploads = args
            .paths
            .into_iter()
            .map(|path| {
                let path = PathBuf::from(path);
                let mime = seal::image_mime(&path)?;
                Ok(Upload { path, mime })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let target = seal::blossom_target(cx.ndb, cx.accounts).map_err(str::to_owned)?;
        let upload = Arc::clone(&self.upload);
        Ok(Box::new(move || {
            let mut attached = Attached {
                attachments: Vec::with_capacity(uploads.len()),
                urls: Vec::with_capacity(uploads.len()),
            };
            for Upload { path, mime } in uploads {
                let attachment = upload_one(&upload, &target, &path, mime)?;
                attached.attachments.push(attachment.imeta_text());
                attached.urls.push(attachment.url);
            }
            Ok(attached)
        }))
    }

    /// One URL a line.
    fn render(&self, out: &Attached) -> Option<String> {
        Some(out.urls.iter().map(|url| format!("{url}\n")).collect())
    }
}

/// Seal the file at `path` and upload it to `target`, waiting for the
/// server: its attachment.
fn upload_one(
    upload: &SharedUploader,
    target: &seal::BlossomTarget,
    path: &std::path::Path,
    mime: String,
) -> Result<Attachment, String> {
    let mut sealed = seal::seal_path(path, mime)?;
    let bytes = std::mem::take(&mut sealed.blob.ciphertext);
    let blob = UploadBlob {
        bytes,
        content_type: "application/octet-stream".to_owned(),
    };
    let descriptor = upload(&target.seckey, &target.server, blob)
        .block_and_take()
        .map_err(|err| format!("uploading {}: {err}", path.display()))?;
    Ok(seal::attachment(sealed, descriptor.url))
}

/// The attachments a comment or description tool was given, read back from
/// their text, or an error naming the one that doesn't parse.
pub(crate) fn parse_attachments(texts: &[String]) -> Result<Vec<Attachment>, String> {
    texts
        .iter()
        .map(|text| {
            Attachment::from_imeta_text(text).ok_or_else(|| {
                format!(
                    "an attachment isn't the imeta text headway_attach answers: {:?}",
                    text.lines().next().unwrap_or_default()
                )
            })
        })
        .collect()
}

/// `body` with a line pointing at each of `attachments` appended, as
/// [`reference`](seal::reference) writes it for `target`: the bare URL in a
/// comment, a markdown image in a description, each on a paragraph of its own.
pub(crate) fn with_references(
    body: String,
    attachments: &[Attachment],
    target: AttachTarget,
) -> String {
    attachments.iter().fold(body, |mut body, attachment| {
        let trimmed = body.trim_end_matches('\n').len();
        body.truncate(trimmed);
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&seal::reference(target, &attachment.url));
        body
    })
}

/// The error for a call that named files in `attach` without `nd` to upload
/// them first.
pub(crate) const ATTACH_NEEDS_UPLOAD: &str = "`attach` names files for a command line to upload \
     first; call headway_attach with them and pass what it answers as `attachments`";
