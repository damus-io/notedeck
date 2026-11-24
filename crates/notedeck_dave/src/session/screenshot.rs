//! Screenshots pasted into a local session's composer, saved to a file and
//! staged as a path chip.
//!
//! A local session's agent reads this machine's disk, so a pasted image
//! needs no upload: it is written to notedeck's cache as
//! `dave-attachments/<sha256>.png` and the message names that path like any
//! other attached file (see [`super::attachments`]). Naming the file by its
//! content dedupes repeat pastes of one image.
//!
//! Encoding and writing take a while for a screenshot, so they run on the
//! [`JobPool`] ([`ScreenshotSave::start`]); the composer shows a spinner chip
//! for each save in flight and stages its path once it is written
//! ([`land_saves`]). A remote session's agent can't read this disk, so its
//! composer never starts a save; it keeps sending the image bytes.
//!
//! Nothing prunes `dave-attachments/` yet: it grows with every distinct
//! screenshot pasted.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use notedeck::{DataPath, DataPathType, JobPool};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

/// The directory under notedeck's cache that pasted screenshots are saved in.
pub const SCREENSHOT_DIR: &str = "dave-attachments";

/// Where pasted screenshots are saved: `<cache>/dave-attachments`.
pub fn screenshot_dir(path: &DataPath) -> PathBuf {
    path.path(DataPathType::Cache).join(SCREENSHOT_DIR)
}

/// Write `png` into `dir` as `<sha256 of png>.png` and return the file's
/// absolute path. The same bytes give the same path, and a file already
/// there is left alone.
///
/// The bytes go to a temporary file renamed into place, so a reader (the
/// agent) never sees half a PNG.
pub fn save_png(dir: &Path, png: &[u8]) -> std::io::Result<PathBuf> {
    let dir = std::path::absolute(dir)?;
    std::fs::create_dir_all(&dir)?;
    let name = hex::encode(Sha256::digest(png));
    let path = dir.join(format!("{name}.png"));
    if path.is_file() {
        return Ok(path);
    }
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(png)?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}

/// PNG-encode `image` and save it into `dir` ([`save_png`]). Runs on the job
/// pool.
fn save_image(dir: &Path, image: &egui::ColorImage) -> Result<PathBuf, String> {
    let png = notedeck::media::png::encode_color_image(image)?;
    save_png(dir, &png).map_err(|e| format!("could not save the screenshot: {e}"))
}

/// A pasted screenshot being encoded and written on the job pool.
pub struct ScreenshotSave {
    rx: oneshot::Receiver<Result<PathBuf, String>>,
}

impl ScreenshotSave {
    /// Start saving `image` into `dir` on `job_pool`.
    pub fn start(job_pool: &JobPool, dir: PathBuf, image: Arc<egui::ColorImage>) -> Self {
        let rx = job_pool.schedule_receivable(move || save_image(&dir, &image));
        Self { rx }
    }
}

/// Stage the path of every save in `saves` that has finished onto `paths`,
/// and drop it from `saves`. A save that failed is logged and dropped.
/// Returns whether any is still running, so the caller can ask for a
/// repaint: the job pool doesn't wake the UI when a save finishes.
pub fn land_saves(saves: &mut Vec<ScreenshotSave>, paths: &mut Vec<PathBuf>) -> bool {
    saves.retain_mut(|save| match save.rx.try_recv() {
        Err(oneshot::error::TryRecvError::Empty) => true,
        Err(oneshot::error::TryRecvError::Closed) => {
            tracing::warn!("screenshot save: the job was dropped");
            false
        }
        Ok(Err(err)) => {
            tracing::warn!("screenshot save: {err}");
            false
        }
        Ok(Ok(path)) => {
            super::stage_path(paths, path);
            false
        }
    });
    !saves.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(color: egui::Color32) -> Vec<u8> {
        let image = egui::ColorImage::new([4, 3], vec![color; 12]);
        notedeck::media::png::encode_color_image(&image).unwrap()
    }

    /// The same bytes give the same absolute path, named by their sha256,
    /// and the file is a PNG that decodes to the image.
    #[test]
    fn same_bytes_same_path_and_it_decodes() {
        let dir = tempfile::TempDir::new().unwrap();
        let png = png_of(egui::Color32::RED);

        let first = save_png(dir.path(), &png).unwrap();
        let again = save_png(dir.path(), &png).unwrap();
        assert_eq!(first, again);
        assert!(first.is_absolute());
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            format!("{}.png", hex::encode(Sha256::digest(&png)))
        );

        let bytes = std::fs::read(&first).unwrap();
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 3));

        let other = save_png(dir.path(), &png_of(egui::Color32::BLUE)).unwrap();
        assert_ne!(first, other, "different bytes, different file");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
