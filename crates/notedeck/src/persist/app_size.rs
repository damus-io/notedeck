use std::time::Duration;

use egui::Context;

use crate::media::MAX_SIZE_WGPU;
use crate::timed_serializer::TimedSerializer;
use crate::{DataPath, DataPathType};

/// The largest native display scale factor supported when restoring a window.
///
/// We can't know the scale factor of the monitor the window will open on until
/// the window exists, so restored sizes are conservatively bounded for displays
/// up to 4x scale.
const MAX_SUPPORTED_NATIVE_SCALE_FACTOR: f32 = 4.0;

/// The largest window size, in native logical pixels, that we are willing to
/// restore.
///
/// The GPU surface is `size * scale_factor` physical pixels and wgpu panics
/// while configuring a surface larger than [`MAX_SIZE_WGPU`] in either
/// dimension.
const MAX_RESTORED_SIZE: f32 = MAX_SIZE_WGPU as f32 / MAX_SUPPORTED_NATIVE_SCALE_FACTOR;

/// Rejects degenerate window sizes and bounds the rest to a size the GPU can
/// present.
///
/// Builds before the units fix below persisted zoomed points, so an existing
/// `app_size.json` can hold a size that panics wgpu on startup.
fn sanitize_size(size: egui::Vec2) -> Option<egui::Vec2> {
    if !size.is_finite() || size.x < 1.0 || size.y < 1.0 {
        return None;
    }

    Some(size.min(egui::Vec2::splat(MAX_RESTORED_SIZE)))
}

pub struct AppSizeHandler {
    serializer: TimedSerializer<egui::Vec2>,
}

impl AppSizeHandler {
    pub fn new(path: &DataPath) -> Self {
        let serializer =
            TimedSerializer::new(path, DataPathType::Setting, "app_size.json".to_owned())
                .with_delay(Duration::from_millis(500));

        Self { serializer }
    }

    /// Saves the current window size in native logical pixels.
    ///
    /// `screen_rect` is in egui points, which include the user's zoom factor,
    /// while [`egui::ViewportBuilder::with_inner_size`] — where the size is
    /// restored — takes native logical pixels. Multiplying the zoom back out
    /// keeps both sides in the same units; without it a non-default zoom
    /// rescales the window by `1 / zoom` on every launch, compounding until the
    /// surface outgrows the GPU's maximum texture size.
    pub fn try_save_app_size(&mut self, ctx: &Context) {
        // There doesn't seem to be a way to check if user is resizing window, so if the rect is different than last saved, we'll wait DELAY before saving again to avoid spamming io
        let cur_size = ctx.input(|i| i.screen_rect.size()) * ctx.zoom_factor();
        self.serializer.try_save(cur_size);
    }

    /// The saved window size in native logical pixels, or `None` if there is no
    /// usable saved size.
    pub fn get_app_size(&self) -> Option<egui::Vec2> {
        sanitize_size(self.serializer.get_item()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use std::fs;

    #[test]
    fn sane_size_is_kept() {
        assert_eq!(
            sanitize_size(vec2(1920.0, 1080.0)),
            Some(vec2(1920.0, 1080.0))
        );
    }

    #[test]
    fn oversized_size_is_clamped_per_axis() {
        assert_eq!(
            sanitize_size(vec2(4297.143, 1080.0)),
            Some(vec2(MAX_RESTORED_SIZE, 1080.0))
        );
    }

    #[test]
    fn restored_size_is_gpu_safe_at_supported_scale() {
        let size = sanitize_size(vec2(f32::MAX, f32::MAX)).unwrap();
        assert!(size.x * MAX_SUPPORTED_NATIVE_SCALE_FACTOR <= MAX_SIZE_WGPU as f32);
        assert!(size.y * MAX_SUPPORTED_NATIVE_SCALE_FACTOR <= MAX_SIZE_WGPU as f32);
    }

    #[test]
    fn degenerate_sizes_are_rejected() {
        assert_eq!(sanitize_size(vec2(0.0, 1080.0)), None);
        assert_eq!(sanitize_size(vec2(0.001, 1080.0)), None);
        assert_eq!(sanitize_size(vec2(1920.0, -1.0)), None);
        assert_eq!(sanitize_size(vec2(f32::NAN, 1080.0)), None);
    }

    #[test]
    fn save_restores_native_logical_size_at_non_default_zoom() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut handler = AppSizeHandler::new(&DataPath::new(tempdir.path()));
        let ctx = Context::default();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(1200.0, 900.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |_| {});
        ctx.set_zoom_factor(1.5);
        let _ = ctx.run(Default::default(), |ctx| handler.try_save_app_size(ctx));

        assert_eq!(handler.get_app_size(), Some(vec2(1200.0, 900.0)));
    }

    #[test]
    fn invalid_persisted_size_is_rejected() {
        let tempdir = tempfile::tempdir().unwrap();
        let settings = tempdir.path().join("settings");
        fs::create_dir(&settings).unwrap();
        fs::write(
            settings.join("app_size.json"),
            serde_json::to_string(&vec2(0.0, 1080.0)).unwrap(),
        )
        .unwrap();

        assert_eq!(
            AppSizeHandler::new(&DataPath::new(tempdir.path())).get_app_size(),
            None
        );
    }
}
