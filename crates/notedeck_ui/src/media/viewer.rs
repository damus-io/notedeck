use egui::{pos2, Color32, Rect};
use notedeck::{ImageType, Images};

/// State used in the MediaViewer ui widget.
///
#[derive(Default)]
pub struct MediaViewerState {
    pub urls: Vec<String>,
}

/// A panning, scrolling, optionally fullscreen, and tiling media viewer
pub struct MediaViewer<'a> {
    state: &'a MediaViewerState,
    fullscreen: bool,
}

impl<'a> MediaViewer<'a> {
    pub fn new(state: &'a MediaViewerState) -> Self {
        let fullscreen = false;
        Self { state, fullscreen }
    }

    pub fn fullscreen(mut self, enable: bool) -> Self {
        self.fullscreen = enable;
        self
    }

    pub fn ui(&self, images: &mut Images, ui: &mut egui::Ui) {
        if self.fullscreen {
            egui::Window::new("Media Viewer")
                .title_bar(false)
                .fixed_size(ui.ctx().screen_rect().size())
                .fixed_pos(ui.ctx().screen_rect().min)
                .frame(egui::Frame::NONE)
                .show(ui.ctx(), |ui| self.ui_content(images, ui));
        } else {
            self.ui_content(images, ui);
        }
    }

    fn ui_content(&self, images: &mut Images, ui: &mut egui::Ui) {
        let avail_rect = ui.available_rect_before_wrap();

        // TODO: id_salt
        let id = ui.id().with("media_viewer");
        let mut scene_rect = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(avail_rect);
        let prev = scene_rect;

        // Draw background
        ui.painter()
            .rect_filled(avail_rect, 0.0, egui::Color32::from_black_alpha(128));

        egui::Scene::new()
            .zoom_range(0.0..=10.0) // enhance 🔬
            .show(ui, &mut scene_rect, |ui| {
                self.render_image_tiles(images, ui);
            });

        if scene_rect != prev {
            ui.ctx().data_mut(|d| d.insert_temp(id, scene_rect));
        }
    }

    ///
    /// Tile a scene with images.
    ///
    /// TODO(jb55): Let's improve image tiling over time, spiraling outward. We
    /// should have a way to click "next" and have the scene smoothly transition and
    /// focus on the next image
    fn render_image_tiles(&self, images: &mut Images, ui: &mut egui::Ui) {
        for url in &self.state.urls {
            // fetch image texture
            let Some(texture) = images.latest_texture(ui, url, ImageType::Content(None)) else {
                continue;
            };

            // the area the next image will be put in.
            let mut img_rect = ui.available_rect_before_wrap();
            if !ui.is_rect_visible(img_rect) {
                // just stop rendering images if we're going out of the scene
                // basic culling when we have lots of images
                break;
            }

            {
                let size = texture.size_vec2();
                img_rect.set_height(size.y);
                img_rect.set_width(size.x);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

                // image actions
                /*
                let response = ui.interact(
                    render_rect,
                    carousel_id.with("img"),
                    Sense::click(),
                );

                if response.clicked() {
                    ui.data_mut(|data| {
                        data.insert_temp(carousel_id.with("show_popup"), true);
                    });
                } else if background_response.clicked() || response.clicked_elsewhere() {
                    ui.data_mut(|data| {
                        data.insert_temp(carousel_id.with("show_popup"), false);
                    });
                }
                */

                // Paint image
                ui.painter()
                    .image(texture.id(), img_rect, uv, Color32::WHITE);

                ui.advance_cursor_after_rect(img_rect);
            }
        }
    }
}
