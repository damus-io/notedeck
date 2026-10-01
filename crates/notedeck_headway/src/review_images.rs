//! The changed images in a review commit, read and decoded on the review
//! loader's worker so the pane can show them before and after instead of
//! "Binary file not shown".
//!
//! [`fetch`] runs on the worker thread next to `git::commit_patch`: it reads
//! each side of every binary image file with [`git::blob_bytes`] and decodes it
//! through notedeck's own image pipeline
//! ([`process_image`](notedeck::media::images::process_image), which caps the
//! longest edge at 2048px). [`upload`] runs once on the UI thread when the load
//! lands, turning each decoded side into a texture for
//! [`GitPatchState::set_file_images`]. Nothing here runs per frame.

use std::path::Path;

use egui::ColorImage;
use headway::git::{self, Blob};
use notedeck::Localization;
use notedeck::media::images::{ImageType, process_image};
use notedeck::media::load_texture_checked;
use notedeck_ui::diff::PatchImage;
use notedeck_ui::diff::{FileImages, FilePatch, FileStatus, GitPatch, GitPatchState, ImageSide};

/// Most image files one commit shows; the sides of any past it say they were
/// left out, unread.
const MAX_IMAGES: usize = 12;

/// Largest side read, in bytes; a bigger one is named by its size, unread.
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Extensions (compared ignoring case) of the binary files worth decoding.
/// SVG isn't one: git diffs it as text. A GIF shows its first frame.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

/// One file's sides as the worker left them. A side the commit doesn't have
/// (the before of an added file) is `None`.
pub(crate) struct FetchedImages {
    /// Index into the patch's files.
    file: usize,
    old: Option<FetchedSide>,
    new: Option<FetchedSide>,
}

/// One side, decoded or the reason it wasn't.
enum FetchedSide {
    Decoded {
        image: ColorImage,
        /// The stored image's size, before any downscale.
        width: u32,
        height: u32,
        bytes: u64,
    },
    TooLarge(u64),
    Unreadable,
    Omitted,
}

/// Whether `path` names an image [`fetch`] decodes.
fn is_image(path: &str) -> bool {
    has_extension(path, &IMAGE_EXTENSIONS)
}

/// Whether `path`'s extension is one of `extensions`, ignoring case.
pub(crate) fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Read and decode both sides of every binary image file `patch` (commit
/// `sha` in `repo_dir`) changes, up to [`MAX_IMAGES`] files. Worker thread
/// only: it runs git and decodes.
pub(crate) fn fetch(repo_dir: &Path, sha: &str, patch: &GitPatch) -> Vec<FetchedImages> {
    let parent = format!("{sha}^");
    patch
        .files()
        .iter()
        .enumerate()
        .filter(|(_, file)| file.binary && is_image(file.path()))
        .enumerate()
        .map(|(n, (file, patch_file))| {
            let omitted = n >= MAX_IMAGES;
            let side = |rev: &str, path: &str| {
                if omitted {
                    Some(FetchedSide::Omitted)
                } else {
                    read_side(repo_dir, rev, path)
                }
            };
            let (has_old, has_new) = sides_of(patch_file);
            FetchedImages {
                file,
                old: has_old
                    .then(|| side(&parent, &patch_file.old_path))
                    .flatten(),
                new: has_new.then(|| side(sha, &patch_file.new_path)).flatten(),
            }
        })
        .collect()
}

/// Which sides `file` has: an added file no before, a deleted one no after.
pub(crate) fn sides_of(file: &FilePatch) -> (bool, bool) {
    (
        file.status != FileStatus::Added,
        file.status != FileStatus::Deleted,
    )
}

/// `path` at `rev`, decoded; `None` when the rev turns out not to have it
/// (a root commit's parent).
fn read_side(repo_dir: &Path, rev: &str, path: &str) -> Option<FetchedSide> {
    match git::blob_bytes(repo_dir, rev, path, MAX_IMAGE_BYTES) {
        Ok(Blob::Missing) => None,
        Ok(Blob::TooLarge(bytes)) => Some(FetchedSide::TooLarge(bytes)),
        Ok(Blob::Bytes(bytes)) => Some(decode(&bytes)),
        Err(e) => {
            tracing::debug!("reading {rev}:{path} for the review diff: {e:?}");
            Some(FetchedSide::Unreadable)
        }
    }
}

/// `bytes` decoded through notedeck's image pipeline, or why not.
#[profiling::function]
fn decode(bytes: &[u8]) -> FetchedSide {
    match image::load_from_memory(bytes) {
        Ok(image) => FetchedSide::Decoded {
            width: image.width(),
            height: image.height(),
            bytes: bytes.len() as u64,
            image: process_image(ImageType::Content(None), image),
        },
        Err(e) => {
            tracing::debug!("decoding an image for the review diff: {e}");
            FetchedSide::Unreadable
        }
    }
}

/// Upload `fetched`'s decoded sides as textures and hand every file's pair
/// to `state`. UI thread, once, when the load lands; `sha` names the
/// textures.
pub(crate) fn upload(
    fetched: Vec<FetchedImages>,
    sha: &str,
    state: &mut GitPatchState,
    ctx: &egui::Context,
    i18n: &mut Localization,
) {
    for FetchedImages { file, old, new } in fetched {
        let side = |which: &str, side: FetchedSide| match side {
            FetchedSide::Decoded {
                image,
                width,
                height,
                bytes,
            } => ImageSide::Shown(PatchImage {
                texture: load_texture_checked(
                    ctx,
                    format!("review-image:{sha}:{file}:{which}"),
                    image,
                    Default::default(),
                ),
                width,
                height,
                bytes,
            }),
            FetchedSide::TooLarge(bytes) => ImageSide::TooLarge { bytes },
            FetchedSide::Unreadable => ImageSide::Unreadable,
            FetchedSide::Omitted => ImageSide::Omitted,
        };
        let images = FileImages {
            old: old.map(|s| side("old", s)),
            new: new.map(|s| side("new", s)),
        };
        state.set_file_images(file, images, i18n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_matches_raster_extensions_ignoring_case() {
        for path in ["a/shot.png", "b.JPG", "c.jpeg", "d.WebP", "e.gif"] {
            assert!(is_image(path), "{path}");
        }
        for path in ["logo.svg", "data.bin", "png", "a.png.txt"] {
            assert!(!is_image(path), "{path}");
        }
    }
}
