//! The changed 3D models (`.glb`) in a review commit, shown before and after
//! in the diff as views the reviewer can turn by dragging.
//!
//! The split follows [`review_images`](crate::review_images):
//!
//! - [`fetch`] runs on the review loader's worker. It reads each side with
//!   [`git::blob_bytes`] and parses and uploads it through the host's shared
//!   renderer ([`Renderer3d::model_uploader`], which also builds that renderer
//!   the first time, slowly, here rather than in a frame). Nothing is
//!   rendered yet.
//! - [`ReviewModels::upload`] runs once on the UI thread when the load lands:
//!   each model joins the renderer, gets an offscreen
//!   [`ModelView`](renderbud::ModelView) registered as an egui texture and is
//!   rendered once, then handed to the patch view as an
//!   [`ImageSide::Model`].
//! - [`ReviewModels::apply`] turns a view on a drag the patch view reports and
//!   renders it again: only on frames with a drag, never at rest.
//!
//! The renderer's lock is held only in those last two, on the UI thread, and
//! by the worker for the moment it takes to clone an uploader. With no
//! renderer (headless, or a window without wgpu) the models keep "Binary
//! file not shown".

use std::path::Path;

use headway::git::{self, Blob};
use notedeck::{Localization, Renderer3d};
use notedeck_ui::diff::{
    DiffSide, FileImages, GitPatch, GitPatchState, ImageSide, MediaKind, ModelGesture, ModelInput,
    PatchModel,
};
use renderbud::{ModelData, ModelUploader, ModelView};

use crate::review_images::{has_extension, sides_of};

/// Most model files one commit shows (as many as images); the sides of any
/// past it say they were left out, unread.
const MAX_MODELS: usize = 12;

/// Largest side read, in bytes; a bigger one is named by its size, unread.
/// Also the cap on each file a model refers to (a texture beside it).
const MAX_MODEL_BYTES: u64 = 32 * 1024 * 1024;

/// Extensions (compared ignoring case) of the binary files shown as models.
/// (A `.gltf` is JSON, which git diffs as text.) Files a model refers to,
/// such as the one texture atlas a kit keeps beside many `.glb`s, are read
/// from the same commit, relative to it.
const MODEL_EXTENSIONS: [&str; 1] = ["glb"];

/// The size a model is drawn at, in points, before the patch view fits it
/// to its column.
const MODEL_SIZE: egui::Vec2 = egui::vec2(480.0, 360.0);

/// One file's sides as the worker left them. A side the commit doesn't have
/// (the before of an added file) is `None`.
pub(crate) struct FetchedModels {
    /// Index into the patch's files.
    file: usize,
    old: Option<FetchedModel>,
    new: Option<FetchedModel>,
}

/// One side, uploaded or the reason it wasn't.
enum FetchedModel {
    Uploaded { data: ModelData, bytes: u64 },
    TooLarge(u64),
    Unreadable,
    Omitted,
}

/// Read and upload both sides of every changed `.glb` in `patch` (commit
/// `sha` in `repo_dir`), up to [`MAX_MODELS`] files, through `renderer`.
/// Worker thread only: it runs git, parses glTF and may build the renderer.
/// Empty without a renderer, or when the commit changes no model.
pub(crate) fn fetch(
    repo_dir: &Path,
    sha: &str,
    patch: &GitPatch,
    renderer: Option<&Renderer3d>,
) -> Vec<FetchedModels> {
    let Some(renderer) = renderer else {
        return Vec::new();
    };
    let models = || {
        patch
            .files()
            .iter()
            .enumerate()
            .filter(|(_, file)| file.binary && has_extension(file.path(), &MODEL_EXTENSIONS))
    };
    if models().next().is_none() {
        return Vec::new();
    }
    let uploader = renderer.model_uploader();
    let parent = format!("{sha}^");
    models()
        .enumerate()
        .map(|(n, (file, patch_file))| {
            let omitted = n >= MAX_MODELS;
            let side = |rev: &str, path: &str| {
                if omitted {
                    Some(FetchedModel::Omitted)
                } else {
                    read_side(&uploader, repo_dir, rev, path)
                }
            };
            let (has_old, has_new) = sides_of(patch_file);
            FetchedModels {
                file,
                old: has_old
                    .then(|| side(&parent, &patch_file.old_path))
                    .flatten(),
                new: has_new.then(|| side(sha, &patch_file.new_path)).flatten(),
            }
        })
        .collect()
}

/// `path` at `rev`, uploaded; `None` when the rev turns out not to have it
/// (a root commit's parent).
fn read_side(
    uploader: &ModelUploader,
    repo_dir: &Path,
    rev: &str,
    path: &str,
) -> Option<FetchedModel> {
    match git::blob_bytes(repo_dir, rev, path, MAX_MODEL_BYTES) {
        Ok(Blob::Missing) => None,
        Ok(Blob::TooLarge(bytes)) => Some(FetchedModel::TooLarge(bytes)),
        Ok(Blob::Bytes(bytes)) => {
            let mut resolve = |uri: &str| beside(repo_dir, rev, path, uri);
            Some(upload(uploader, &bytes, &mut resolve))
        }
        Err(e) => {
            tracing::debug!("reading {rev}:{path} for the review diff: {e:?}");
            Some(FetchedModel::Unreadable)
        }
    }
}

/// The file `uri` names, relative to the model at `model_path`, as of
/// `rev`: how a model's texture beside it is found. `None` when the commit
/// hasn't got it (or it's over [`MAX_MODEL_BYTES`]).
fn beside(repo_dir: &Path, rev: &str, model_path: &str, uri: &str) -> Option<Vec<u8>> {
    let path = resolve_relative(model_path, uri)?;
    match git::blob_bytes(repo_dir, rev, &path, MAX_MODEL_BYTES) {
        Ok(Blob::Bytes(bytes)) => Some(bytes),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("reading {rev}:{path} for a review model: {e:?}");
            None
        }
    }
}

/// The repo path `uri` names relative to the file at `from`, with `.` and
/// `..` folded away; `None` for an absolute URI or one that climbs out of
/// the repo.
fn resolve_relative(from: &str, uri: &str) -> Option<String> {
    if uri.starts_with('/') || uri.contains("://") {
        return None;
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop(); // the model's own name
    for part in uri.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// `bytes` parsed and uploaded, asking `resolve` for any file it refers to,
/// or why not. The file is whatever someone committed, so a panic in the
/// glTF stack is caught and reads as unreadable rather than taking the load
/// down with it.
#[profiling::function]
fn upload(
    uploader: &ModelUploader,
    bytes: &[u8],
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> FetchedModel {
    let uploaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        uploader.upload_gltf_slice_with(bytes, resolve)
    }));
    match uploaded {
        Ok(Ok(data)) => FetchedModel::Uploaded {
            data,
            bytes: bytes.len() as u64,
        },
        Ok(Err(e)) => {
            tracing::debug!("loading a model for the review diff: {e}");
            FetchedModel::Unreadable
        }
        Err(_) => {
            tracing::debug!("loading a model for the review diff panicked");
            FetchedModel::Unreadable
        }
    }
}

/// One drawn side: its view and the egui texture showing it.
struct ShownModel {
    file: usize,
    side: DiffSide,
    view: ModelView,
    texture: egui::TextureId,
}

/// A loaded review's model views. Dropping it frees their textures and
/// takes their models out of the shared renderer.
pub(crate) struct ReviewModels {
    renderer: Renderer3d,
    shown: Vec<ShownModel>,
}

impl ReviewModels {
    /// Give `fetched`'s uploaded models to the shared renderer, make and
    /// render a view of each at `pixels_per_point`, and hand every file's
    /// pair to `state`. UI thread, once, when the load lands. `None` when
    /// there is nothing to keep (no renderer, or no model fetched).
    pub(crate) fn upload(
        fetched: Vec<FetchedModels>,
        state: &mut GitPatchState,
        renderer: Option<&Renderer3d>,
        pixels_per_point: f32,
        i18n: &mut Localization,
    ) -> Option<Self> {
        let renderer = renderer?;
        if fetched.is_empty() {
            return None;
        }
        let mut models = Self {
            renderer: renderer.clone(),
            shown: Vec::new(),
        };
        let size = (
            (MODEL_SIZE.x * pixels_per_point).round() as u32,
            (MODEL_SIZE.y * pixels_per_point).round() as u32,
        );
        for FetchedModels { file, old, new } in fetched {
            let images = FileImages {
                old: old.map(|s| models.side(file, DiffSide::Old, s, size)),
                new: new.map(|s| models.side(file, DiffSide::New, s, size)),
                kind: MediaKind::Model,
            };
            state.set_file_images(file, images, i18n);
        }
        Some(models)
    }

    /// One side as the patch view shows it, making and rendering its view
    /// when it was uploaded.
    fn side(
        &mut self,
        file: usize,
        side: DiffSide,
        fetched: FetchedModel,
        size: (u32, u32),
    ) -> ImageSide {
        let (data, bytes) = match fetched {
            FetchedModel::Uploaded { data, bytes } => (data, bytes),
            FetchedModel::TooLarge(bytes) => return ImageSide::TooLarge { bytes },
            FetchedModel::Unreadable => return ImageSide::Unreadable,
            FetchedModel::Omitted => return ImageSide::Omitted,
        };
        let triangles = data.triangles();
        let renderer = &self.renderer;
        let (device, queue) = (renderer.device(), renderer.queue());
        let view = {
            let mut scene = renderer
                .scene()
                .renderer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let model = scene.insert_model(data);
            let view = scene.create_model_view(device, model, size);
            match view {
                Some(mut view) => {
                    scene.render_model_view(device, queue, &mut view);
                    Some(view)
                }
                None => {
                    scene.remove_model(model);
                    None
                }
            }
        };
        let Some(view) = view else {
            return ImageSide::Unreadable;
        };
        let texture = renderer.register_native_texture(view.texture_view());
        let shown = PatchModel {
            texture,
            size: MODEL_SIZE,
            triangles,
            bytes,
        };
        self.shown.push(ShownModel {
            file,
            side,
            view,
            texture,
        });
        ImageSide::Model(shown)
    }

    /// Turn the view `input` is on and render it again. UI thread, on a
    /// frame the patch view reported a drag or double-click.
    pub(crate) fn apply(&mut self, input: ModelInput) {
        let Some(shown) = self
            .shown
            .iter_mut()
            .find(|s| s.file == input.file && s.side == input.side)
        else {
            return;
        };
        match input.gesture {
            ModelGesture::Orbit(delta) => shown.view.orbit(delta.x, delta.y),
            ModelGesture::Reset => shown.view.reset(),
        }
        let renderer = &self.renderer;
        let scene = renderer
            .scene()
            .renderer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scene.render_model_view(renderer.device(), renderer.queue(), &mut shown.view);
    }
}

impl Drop for ReviewModels {
    fn drop(&mut self) {
        let mut scene = self
            .renderer
            .scene()
            .renderer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for shown in &self.shown {
            self.renderer.free_texture(shown.texture);
            scene.remove_model(shown.view.model());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.glb` is a model whatever its case; look-alikes aren't.
    #[test]
    fn model_extensions_are_glb_ignoring_case() {
        for path in ["a/cube.glb", "B.GLB"] {
            assert!(has_extension(path, &MODEL_EXTENSIONS), "{path}");
        }
        for path in ["cube.glb.txt", "glb", "scene.gltf"] {
            assert!(!has_extension(path, &MODEL_EXTENSIONS), "{path}");
        }
    }

    /// A model's URIs resolve against its own directory, folding `.` and
    /// `..`; one that's absolute or climbs out of the repo resolves to
    /// nothing.
    #[test]
    fn uris_resolve_beside_the_model() {
        let at = "assets/castle/flag.glb";
        assert_eq!(
            resolve_relative(at, "Textures/colormap.png").as_deref(),
            Some("assets/castle/Textures/colormap.png")
        );
        assert_eq!(
            resolve_relative(at, "./../shared/./atlas.png").as_deref(),
            Some("assets/shared/atlas.png")
        );
        assert_eq!(
            resolve_relative("top.glb", "tex.png").as_deref(),
            Some("tex.png")
        );
        assert_eq!(resolve_relative(at, "../../../etc/passwd"), None);
        assert_eq!(resolve_relative(at, "/etc/passwd"), None);
        assert_eq!(resolve_relative(at, "https://example.com/t.png"), None);
    }
}
