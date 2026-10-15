use std::time::Duration;

use egui::{Context, Vec2};

use crate::timed_serializer::TimedSerializer;
use crate::{DataPath, DataPathType};

/// The largest window side we restore, in points.
///
/// The restored size goes straight into the window builder, and wgpu panics
/// on launch if the surface is wider than the GPU's max texture size (8192px
/// on Apple silicon). At a 2x scale factor this is the most a restored window
/// can be, and it is more than any window we'd want to reopen.
const MAX_RESTORED_SIDE: f32 = 4096.0;

pub struct AppSizeHandler {
    serializer: TimedSerializer<Vec2>,
}

impl AppSizeHandler {
    pub fn new(path: &DataPath) -> Self {
        let serializer =
            TimedSerializer::new(path, DataPathType::Setting, "app_size.json".to_owned())
                .with_delay(Duration::from_millis(500));

        Self { serializer }
    }

    /// Save the current window size, unless it's bigger than the monitor:
    /// a window can't really be that big, so it's a layout gone wrong and
    /// reopening at that size would be worse.
    pub fn try_save_app_size(&mut self, ctx: &Context) {
        // There doesn't seem to be a way to check if user is resizing window, so if the rect is different than last saved, we'll wait DELAY before saving again to avoid spamming io
        let (cur_size, monitor_size) =
            ctx.input(|i| (i.content_rect().size(), i.viewport().monitor_size));

        if monitor_size.is_some_and(|monitor| cur_size.x > monitor.x || cur_size.y > monitor.y) {
            return;
        }

        self.serializer.try_save(cur_size);
    }

    /// The saved window size, if there is one we can safely reopen at.
    pub fn get_app_size(&self) -> Option<Vec2> {
        self.serializer.get_item().and_then(restorable_size)
    }
}

/// A saved size made safe to hand to the window builder: dropped if it isn't
/// a real size, and clamped to [`MAX_RESTORED_SIDE`] on each side.
fn restorable_size(size: Vec2) -> Option<Vec2> {
    if !size.x.is_finite() || !size.y.is_finite() || size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }

    Some(size.min(Vec2::splat(MAX_RESTORED_SIDE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_size_restores_unchanged() {
        let size = Vec2::new(1280.0, 800.0);
        assert_eq!(restorable_size(size), Some(size));
    }

    #[test]
    fn oversized_window_is_clamped() {
        // the size a garbage note block once blew the window up to, which
        // asked wgpu for an 11766px surface on a 2x display
        let size = Vec2::new(5882.3525, 5882.3525);
        assert_eq!(restorable_size(size), Some(Vec2::splat(MAX_RESTORED_SIDE)));
    }

    #[test]
    fn nonsense_sizes_are_dropped() {
        assert_eq!(restorable_size(Vec2::new(f32::NAN, 800.0)), None);
        assert_eq!(restorable_size(Vec2::new(1280.0, f32::INFINITY)), None);
        assert_eq!(restorable_size(Vec2::new(0.0, 800.0)), None);
        assert_eq!(restorable_size(Vec2::new(1280.0, -1.0)), None);
    }
}
