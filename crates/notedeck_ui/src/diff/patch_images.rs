//! Changed images in a [`git_patch_ui`](super::git_patch_ui): a binary file
//! whose sides the caller has decoded is drawn as its before and after, side
//! by side, instead of "Binary file not shown".
//!
//! The widget does no I/O and no decoding. The caller reads the blobs, decodes
//! them off the UI thread, uploads each once as a texture and hands the pair
//! over with [`GitPatchState::set_file_images`](super::GitPatchState::set_file_images).
//! Captions are formatted there, once; a frame only paints.

use egui::epaint::{Brush, RectShape};
use egui::{Color32, Rect, Role, Sense, TextureHandle, Ui, WidgetInfo};
use notedeck::{tr, Localization};
use std::sync::Arc;

/// Tallest an image is drawn, in points; wider or taller ones are scaled
/// down to fit, keeping their aspect.
const MAX_IMAGE_HEIGHT: f32 = 360.0;
/// Gap between the before and after columns.
pub(super) const IMAGE_GAP: f32 = 16.0;
/// Gap between an image and its caption.
const CAPTION_GAP: f32 = 4.0;

/// One side of a changed image file, as far as the caller got with it.
#[derive(Clone)]
pub enum ImageSide {
    /// Read, decoded and uploaded.
    Shown(PatchImage),
    /// Not read: bigger than the caller's cap.
    TooLarge {
        /// The blob's size.
        bytes: u64,
    },
    /// Read, but not something the caller could decode.
    Unreadable,
    /// Not read: the commit has more images than the caller will show.
    Omitted,
}

/// A decoded side of an image file, uploaded as a texture.
#[derive(Clone)]
pub struct PatchImage {
    pub texture: TextureHandle,
    /// The stored image's width in pixels, before any downscale for the
    /// texture.
    pub width: u32,
    /// The stored image's height in pixels.
    pub height: u32,
    /// The blob's size.
    pub bytes: u64,
}

/// A binary file's two sides. A side the commit doesn't have is `None`: the
/// before of an added file, the after of a deleted one.
#[derive(Clone, Default)]
pub struct FileImages {
    pub old: Option<ImageSide>,
    pub new: Option<ImageSide>,
}

/// A file's images as the view draws them: its sides left to right (before
/// first; one that doesn't exist leaves no gap), each with its caption
/// formatted once.
#[derive(Clone, Default)]
pub(super) struct ShownImages {
    pub(super) sides: [Option<ShownSide>; 2],
}

/// One column of a [`ShownImages`].
#[derive(Clone)]
pub(super) struct ShownSide {
    /// `None` when the side couldn't be shown; its caption says why.
    pub(super) texture: Option<SideTexture>,
    /// `before 1200×800 · 142 KB`, or `before: too large to show (9.1 MB)`.
    pub(super) caption: String,
}

/// A shown side's texture, with the brush that paints it made once: a
/// textured rect holds its brush in an `Arc`, so building one per frame
/// would allocate, and cloning this one doesn't.
#[derive(Clone)]
pub(super) struct SideTexture {
    /// Keeps the texture alive while the side is shown.
    handle: TextureHandle,
    brush: Arc<Brush>,
}

impl SideTexture {
    fn new(handle: TextureHandle) -> Self {
        let brush = Arc::new(Brush {
            fill_texture_id: handle.id(),
            uv: Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        });
        Self { handle, brush }
    }

    fn size(&self) -> egui::Vec2 {
        self.handle.size_vec2()
    }
}

impl ShownImages {
    /// `images` with their captions formatted, or `None` when the file has no
    /// side at all.
    pub(super) fn new(images: FileImages, i18n: &mut Localization) -> Option<Self> {
        let before = tr!(
            i18n,
            "before",
            "Label for the old version of a changed image in a diff"
        );
        let after = tr!(
            i18n,
            "after",
            "Label for the new version of a changed image in a diff"
        );
        let mut sides = [
            images.old.map(|s| (before, s)),
            images.new.map(|s| (after, s)),
        ]
        .into_iter()
        .flatten()
        .map(|(label, side)| ShownSide::new(&label, side, i18n));
        let shown = Self {
            sides: [sides.next(), sides.next()],
        };
        shown.sides[0].is_some().then_some(shown)
    }

    /// Whether any side has an image to draw. When none does, the file keeps
    /// its one note row, saying why (see [`Self::note`]).
    pub(super) fn any_shown(&self) -> bool {
        self.sides.iter().flatten().any(|s| s.texture.is_some())
    }

    /// What the note row says in place of "Binary file not shown" when no
    /// side can be drawn: the first side's reason.
    pub(super) fn note(&self) -> Option<&str> {
        self.sides[0].as_ref().map(|s| s.caption.as_str())
    }

    /// The tallest side's drawn size, for a column `width` wide.
    fn image_height(&self, width: f32) -> f32 {
        self.sides
            .iter()
            .flatten()
            .filter_map(|s| s.texture.as_ref())
            .map(|t| fit(t.size(), width).y)
            .fold(0.0, f32::max)
    }

    /// Rows the images and their captions take, each `step` tall, in columns
    /// `width` wide under captions `caption_height` tall.
    pub(super) fn rows(&self, geometry: ImageGeometry) -> usize {
        let height = self.image_height(geometry.width)
            + CAPTION_GAP
            + geometry.caption_height
            + geometry.spacing;
        ((height / geometry.step).ceil() as usize).max(1)
    }
}

impl ShownSide {
    fn new(label: &str, side: ImageSide, i18n: &mut Localization) -> Self {
        let (texture, caption) = match side {
            ImageSide::Shown(image) => {
                let caption = tr!(
                    i18n,
                    "{side} {width}×{height} · {size}",
                    "Caption under one version of a changed image in a diff: which version, its pixel size and its file size",
                    side = label,
                    width = image.width.to_string(),
                    height = image.height.to_string(),
                    size = file_size(image.bytes)
                );
                (Some(SideTexture::new(image.texture)), caption)
            }
            ImageSide::TooLarge { bytes } => (
                None,
                tr!(
                    i18n,
                    "{side}: too large to show ({size})",
                    "Caption for a version of a changed image too big to load in a diff",
                    side = label,
                    size = file_size(bytes)
                ),
            ),
            ImageSide::Unreadable => (
                None,
                tr!(
                    i18n,
                    "{side}: not an image that could be read",
                    "Caption for a version of a changed image that failed to decode in a diff",
                    side = label
                ),
            ),
            ImageSide::Omitted => (
                None,
                tr!(
                    i18n,
                    "{side}: not shown, the commit changes too many images",
                    "Caption for a version of a changed image skipped because the commit has too many images",
                    side = label
                ),
            ),
        };
        Self { texture, caption }
    }
}

impl std::fmt::Debug for ShownImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let captions = self.sides.iter().flatten().map(|s| &s.caption);
        f.debug_list().entries(captions).finish()
    }
}

/// What an image row's size depends on, measured once a pass: every file's
/// row count and every drawn image come from the same numbers, so the rows
/// laid out and the rows drawn agree.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ImageGeometry {
    /// One column's width.
    pub(super) width: f32,
    /// A caption's height.
    pub(super) caption_height: f32,
    /// A row's height plus the item spacing below it.
    pub(super) step: f32,
    /// The item spacing below a row.
    pub(super) spacing: f32,
}

impl ImageGeometry {
    /// Columns for a view whose rows start at `left` and end at `right`,
    /// past an indent of `indent`.
    pub(super) fn new(ui: &Ui, left: f32, right: f32, indent: f32, step: f32) -> Self {
        let room = (right - left - indent - IMAGE_GAP).max(2.0);
        Self {
            width: room / 2.0,
            caption_height: ui.text_style_height(&egui::TextStyle::Small),
            step,
            spacing: ui.spacing().item_spacing.y,
        }
    }
}

/// `size` scaled down (never up) to fit a column `width` wide and
/// [`MAX_IMAGE_HEIGHT`] tall.
fn fit(size: egui::Vec2, width: f32) -> egui::Vec2 {
    let scale = (width / size.x).min(MAX_IMAGE_HEIGHT / size.y).min(1.0);
    size * scale.max(0.0)
}

/// Draw `images` across their rows, `part` of which the cursor is at (the
/// top of the view when that's below their first row), starting `indent`
/// right of it. `captions` are the sides' laid-out captions. Paints without a
/// child `Ui` or any text building, so a steady frame allocates nothing here.
/// Advances one row, as every row does; the rest of the rows skip.
pub(super) fn images_ui(
    images: &ShownImages,
    captions: [Option<Arc<egui::Galley>>; 2],
    part: usize,
    geometry: ImageGeometry,
    indent: f32,
    ui: &mut Ui,
) {
    let height = ui.spacing().interact_size.y;
    let min = ui.cursor().min;
    let top = min.y - part as f32 * geometry.step;
    let image_height = images.image_height(geometry.width);
    let id = ui.advance_cursor_after_rect(Rect::from_min_size(min, egui::vec2(0.0, height)));

    let visuals = ui.visuals();
    let backing = visuals.extreme_bg_color;
    let border = visuals.widgets.noninteractive.bg_stroke;
    let caption_color = visuals.weak_text_color();
    for (i, (side, caption)) in images.sides.iter().zip(captions).enumerate() {
        let (Some(side), Some(caption)) = (side, caption) else {
            continue;
        };
        let x = min.x + indent + i as f32 * (geometry.width + IMAGE_GAP);
        if let Some(texture) = &side.texture {
            let rect = Rect::from_min_size(egui::pos2(x, top), fit(texture.size(), geometry.width));
            let painter = ui.painter();
            // A backing, so a transparent image reads as one.
            painter.rect_filled(rect, 0.0, backing);
            // A textured rect rather than `Painter::image`, which builds a
            // mesh every call.
            let mut image = RectShape::filled(rect, 0.0, Color32::WHITE);
            image.brush = Some(texture.brush.clone());
            painter.add(image);
            painter.rect_stroke(rect, 0.0, border, egui::StrokeKind::Outside);
            let response = ui.interact(rect, id.with(("image", i)), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Image, true, &side.caption));
        }
        let caption_min = egui::pos2(x, top + image_height + CAPTION_GAP);
        let caption_rect = Rect::from_min_size(caption_min, caption.size());
        let response = ui.interact(caption_rect, id.with(("caption", i)), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, caption.text()));
        ui.painter().galley(caption_min, caption, caption_color);
    }
}

/// `bytes` for a caption: `812 B`, `142 KB`, `9.1 MB`.
fn file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = bytes as f64;
    if n < KB {
        format!("{bytes} B")
    } else if n < MB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{:.1} MB", n / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_size_picks_a_unit() {
        assert_eq!(file_size(812), "812 B");
        assert_eq!(file_size(142 * 1024 + 100), "142 KB");
        assert_eq!(file_size(9_542_000), "9.1 MB");
    }

    /// Images shrink to fit the column and the height cap, keeping their
    /// aspect, and never grow.
    #[test]
    fn fit_scales_down_only() {
        assert_eq!(fit(egui::vec2(40.0, 20.0), 300.0), egui::vec2(40.0, 20.0));
        assert_eq!(
            fit(egui::vec2(600.0, 300.0), 300.0),
            egui::vec2(300.0, 150.0)
        );
        assert_eq!(
            fit(egui::vec2(200.0, 720.0), 300.0),
            egui::vec2(100.0, 360.0)
        );
    }
}
