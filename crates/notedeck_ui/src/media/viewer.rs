use bitflags::bitflags;
use egui::{emath::TSTransform, pos2, Color32, Rangef, Rect};
use notedeck::media::{MediaInfo, ViewMediaInfo};
use notedeck::{ImageType, Images};

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct MediaViewerFlags: u64 {
        /// Open the media viewer fullscreen
        const Fullscreen = 1 << 0;

        /// Enable a transition animation
        const Transition = 1 << 1;

        /// Are we open or closed?
        const Open = 1 << 2;
    }
}

/// State used in the MediaViewer ui widget.
pub struct MediaViewerState {
    /// When
    pub media_info: ViewMediaInfo,
    pub scene_rect: Option<Rect>,
    pub flags: MediaViewerFlags,
    pub anim_id: egui::Id,
}

impl Default for MediaViewerState {
    fn default() -> Self {
        Self {
            anim_id: egui::Id::new("notedeck-fullscreen-media-viewer"),
            media_info: Default::default(),
            scene_rect: None,
            flags: MediaViewerFlags::Transition | MediaViewerFlags::Fullscreen,
        }
    }
}

impl MediaViewerState {
    pub fn new(anim_id: egui::Id) -> Self {
        Self {
            anim_id,
            ..Default::default()
        }
    }

    /// How much is our media viewer open
    pub fn open_amount(&self, ui: &mut egui::Ui) -> f32 {
        ui.ctx().animate_bool_with_time_and_easing(
            self.anim_id,
            self.flags.contains(MediaViewerFlags::Open),
            0.3,
            egui::emath::easing::cubic_out,
        )
    }

    /// Should we show the control even if we're closed?
    /// Needed for transition animation
    pub fn should_show(&self, ui: &mut egui::Ui) -> bool {
        if self.flags.contains(MediaViewerFlags::Open) {
            return true;
        }

        // we are closing
        self.open_amount(ui) > 0.0
    }
}

/// A panning, scrolling, optionally fullscreen, and tiling media viewer
pub struct MediaViewer<'a> {
    state: &'a mut MediaViewerState,
}

impl<'a> MediaViewer<'a> {
    pub fn new(state: &'a mut MediaViewerState) -> Self {
        Self { state }
    }

    /// Is this
    pub fn fullscreen(self, enable: bool) -> Self {
        self.state.flags.set(MediaViewerFlags::Fullscreen, enable);
        self
    }

    /// Enable open transition animation
    pub fn transition(self, enable: bool) -> Self {
        self.state.flags.set(MediaViewerFlags::Transition, enable);
        self
    }

    pub fn ui(&mut self, images: &mut Images, ui: &mut egui::Ui) -> egui::Response {
        if self.state.flags.contains(MediaViewerFlags::Fullscreen) {
            egui::Window::new("Media Viewer")
                .title_bar(false)
                .fixed_size(ui.ctx().screen_rect().size())
                .fixed_pos(ui.ctx().screen_rect().min)
                .frame(egui::Frame::NONE)
                .show(ui.ctx(), |ui| self.ui_content(images, ui))
                .unwrap() // SAFETY: we are always open
                .inner
                .unwrap()
        } else {
            self.ui_content(images, ui)
        }
    }

    fn ui_content(&mut self, images: &mut Images, ui: &mut egui::Ui) -> egui::Response {
        let avail_rect = ui.available_rect_before_wrap();

        let scene_rect = if let Some(scene_rect) = self.state.scene_rect {
            scene_rect
        } else {
            self.state.scene_rect = Some(avail_rect);
            avail_rect
        };

        let zoom_range: egui::Rangef = (0.0..=10.0).into();

        let is_open = self.state.flags.contains(MediaViewerFlags::Open);
        let can_transition = self.state.flags.contains(MediaViewerFlags::Transition);
        let open_amount = self.state.open_amount(ui);
        let transitioning = if !can_transition {
            false
        } else if is_open {
            open_amount < 1.0
        } else {
            open_amount > 0.0
        };

        let mut trans_rect = if transitioning {
            let clicked_img = &self.state.media_info.clicked_media();
            let src_pos = &clicked_img.original_position;
            let in_scene_pos = Self::first_image_rect(ui, clicked_img, images);
            transition_scene_rect(
                &avail_rect,
                &zoom_range,
                &in_scene_pos,
                src_pos,
                open_amount,
            )
        } else {
            scene_rect
        };

        // Draw background
        ui.painter().rect_filled(
            avail_rect,
            0.0,
            egui::Color32::from_black_alpha((200.0 * open_amount) as u8),
        );

        let scene = egui::Scene::new().zoom_range(zoom_range);

        // We are opening, so lock controls
        /* TODO(jb55): 0.32
        if transitioning {
            scene = scene.sense(egui::Sense::hover());
        }
        */

        let resp = scene.show(ui, &mut trans_rect, |ui| {
            Self::render_image_tiles(&self.state.media_info.medias, images, ui, open_amount);
        });

        self.state.scene_rect = Some(trans_rect);

        resp.response
    }

    /// The rect of the first image to be placed.
    /// This is mainly used for the transition animation
    ///
    /// TODO(jb55): replace this with a "placed" variant once
    /// we have image layouts
    fn first_image_rect(ui: &mut egui::Ui, media: &MediaInfo, images: &mut Images) -> Rect {
        // fetch image texture
        let Some(texture) = images.latest_texture(ui, &media.url, ImageType::Content(None)) else {
            tracing::error!("could not get latest texture in first_image_rect");
            return Rect::ZERO;
        };

        // the area the next image will be put in.
        let mut img_rect = ui.available_rect_before_wrap();

        let size = texture.size_vec2();
        img_rect.set_height(size.y);
        img_rect.set_width(size.x);
        img_rect
    }

    ///
    /// Tile a scene with images.
    ///
    /// TODO(jb55): Let's improve image tiling over time, spiraling outward. We
    /// should have a way to click "next" and have the scene smoothly transition and
    /// focus on the next image
    fn render_image_tiles(
        infos: &[MediaInfo],
        images: &mut Images,
        ui: &mut egui::Ui,
        open_amount: f32,
    ) {
        for info in infos {
            let url = &info.url;

            // fetch image texture
            let Some(texture) = images.latest_texture(ui, url, ImageType::Content(None)) else {
                continue;
            };

            // the area the next image will be put in.
            let mut img_rect = ui.available_rect_before_wrap();
            /*
            if !ui.is_rect_visible(img_rect) {
                // just stop rendering images if we're going out of the scene
                // basic culling when we have lots of images
                break;
            }
            */

            {
                let size = texture.size_vec2();
                img_rect.set_height(size.y);
                img_rect.set_width(size.x);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

                // image actions
                //let response = ui.interact(render_rect, carousel_id.with("img"), Sense::click());

                /*
                if response.clicked() {
                } else if background_response.clicked() {
                }
                */

                // Paint image
                ui.painter().image(
                    texture.id(),
                    img_rect,
                    uv,
                    Color32::from_white_alpha((open_amount * 255.0) as u8),
                );

                ui.advance_cursor_after_rect(img_rect);
            }
        }
    }
}

/// Helper: lerp a TSTransform (uniform scale + translation)
fn lerp_ts(a: TSTransform, b: TSTransform, t: f32) -> TSTransform {
    let s = egui::lerp(a.scaling..=b.scaling, t);
    let p = a.translation + (b.translation - a.translation) * t;
    TSTransform {
        scaling: s,
        translation: p,
    }
}

/// Calculate the open/close amount and transition rect
pub fn transition_scene_rect(
    outer_rect: &Rect,
    zoom_range: &Rangef,
    image_rect_in_scene: &Rect, // e.g. Rect::from_min_size(Pos2::ZERO, image_size)
    timeline_global_rect: &Rect, // saved from timeline Response.rect
    open_amt: f32,              // stable ID per media item
) -> Rect {
    // Compute the two endpoints:
    let from = fit_to_rect_in_scene(timeline_global_rect, image_rect_in_scene, zoom_range);
    let to = fit_to_rect_in_scene(outer_rect, image_rect_in_scene, zoom_range);

    // Interpolate transform and convert to scene_rect expected by Scene::show:
    let lerped = lerp_ts(from, to, open_amt);

    lerped.inverse() * (*outer_rect)
}

/// Creates a transformation that fits a given scene rectangle into the available screen size.
///
/// The resulting visual scene bounds can be larger, due to letterboxing.
///
/// Returns the transformation from `scene` to `global` coordinates.
fn fit_to_rect_in_scene(
    rect_in_global: &Rect,
    rect_in_scene: &Rect,
    zoom_range: &Rangef,
) -> TSTransform {
    // Compute the scale factor to fit the bounding rectangle into the available screen size:
    let scale = rect_in_global.size() / rect_in_scene.size();

    // Use the smaller of the two scales to ensure the whole rectangle fits on the screen:
    let scale = scale.min_elem();

    // Clamp scale to what is allowed
    let scale = zoom_range.clamp(scale);

    // Compute the translation to center the bounding rect in the screen:
    let center_in_global = rect_in_global.center().to_vec2();
    let center_scene = rect_in_scene.center().to_vec2();

    // Set the transformation to scale and then translate to center.
    TSTransform::from_translation(center_in_global - scale * center_scene)
        * TSTransform::from_scaling(scale)
}
