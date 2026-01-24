use bitflags::bitflags;
use egui::{pos2, Color32, Rect};
use notedeck::media::{AnimationMode, MediaInfo, ViewMediaInfo};
use notedeck::{ImageType, Images, MediaJobSender};

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

    /// Current displayed image index
    pub current_index: usize,
}

impl Default for MediaViewerState {
    fn default() -> Self {
        Self {
            anim_id: egui::Id::new("notedeck-fullscreen-media-viewer"),
            media_info: Default::default(),
            scene_rect: None,
            flags: MediaViewerFlags::Transition | MediaViewerFlags::Fullscreen,
            current_index: 0,
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

    /// Set media info and initialize viewer state
    pub fn set_media_info(&mut self, media_info: ViewMediaInfo) {
        self.current_index = media_info.clicked_index;
        self.media_info = media_info;
    }

    /// Navigate to the next image
    pub fn next_image(&mut self) {
        if self.current_index + 1 < self.media_info.medias.len() {
            self.current_index += 1;
        }
    }

    /// Navigate to the previous image
    pub fn prev_image(&mut self) {
        if self.current_index > 0 {
            self.current_index -= 1;
        }
    }

    /// Get the current media info
    pub fn current_media(&self) -> Option<&MediaInfo> {
        self.media_info.medias.get(self.current_index)
    }

    /// Check if there's a next image
    pub fn has_next(&self) -> bool {
        self.current_index + 1 < self.media_info.medias.len()
    }

    /// Check if there's a previous image
    pub fn has_prev(&self) -> bool {
        self.current_index > 0
    }

    /// Total number of images
    pub fn image_count(&self) -> usize {
        self.media_info.medias.len()
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

    pub fn ui(
        &mut self,
        images: &mut Images,
        jobs: &MediaJobSender,
        ui: &mut egui::Ui,
    ) -> egui::Response {
        if self.state.flags.contains(MediaViewerFlags::Fullscreen) {
            egui::Window::new("Media Viewer")
                .title_bar(false)
                .fixed_size(ui.ctx().screen_rect().size())
                .fixed_pos(ui.ctx().screen_rect().min)
                .frame(egui::Frame::NONE)
                .show(ui.ctx(), |ui| self.ui_content(images, jobs, ui))
                .unwrap() // SAFETY: we are always open
                .inner
                .unwrap()
        } else {
            self.ui_content(images, jobs, ui)
        }
    }

    fn ui_content(
        &mut self,
        images: &mut Images,
        jobs: &MediaJobSender,
        ui: &mut egui::Ui,
    ) -> egui::Response {
        let avail_rect = ui.available_rect_before_wrap();

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

        // Draw background
        ui.painter().rect_filled(
            avail_rect,
            0.0,
            egui::Color32::from_black_alpha((200.0 * open_amount) as u8),
        );

        // Create an interactive area for the entire viewer
        let response = ui.allocate_rect(avail_rect, egui::Sense::click_and_drag());

        // Handle input (keyboard, clicks) - but not during transition
        if !transitioning {
            self.handle_input(ui, &response);
        }

        // Render the current image
        self.render_single_image(images, jobs, ui, &avail_rect, open_amount, transitioning);

        // Render navigation arrows (only if multiple images and not transitioning)
        if self.state.image_count() > 1 && !transitioning {
            self.render_nav_arrows(ui, &avail_rect);
        }

        // Render position indicator (only if multiple images)
        if self.state.image_count() > 1 {
            self.render_position_indicator(ui, &avail_rect, open_amount);
        }

        // Render close button when fullscreen (touch-friendly affordance)
        if self.state.flags.contains(MediaViewerFlags::Fullscreen) && !transitioning {
            self.render_close_button(ui, &avail_rect, open_amount);
        }

        response
    }

    /// Handle all input: keyboard, mouse, touch
    fn handle_input(&mut self, ui: &mut egui::Ui, _response: &egui::Response) {
        let ctx = ui.ctx();

        // Keyboard navigation
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            self.state.next_image();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            self.state.prev_image();
        }

        // Escape to close
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
        }

        // Android back button / browser back
        if ctx.input(|i| i.viewport().close_requested()) {
            self.close();
        }
    }

    /// Close the media viewer
    fn close(&mut self) {
        self.state.flags.remove(MediaViewerFlags::Open);
    }

    /// Render the current single image fitted to screen
    fn render_single_image(
        &mut self,
        images: &mut Images,
        jobs: &MediaJobSender,
        ui: &mut egui::Ui,
        avail_rect: &Rect,
        open_amount: f32,
        transitioning: bool,
    ) {
        let index = self.state.current_index;
        let Some(media) = self.state.media_info.medias.get(index) else {
            return;
        };

        let Some(texture) = images.latest_texture(
            jobs,
            ui,
            &media.url,
            ImageType::Content(None),
            AnimationMode::Continuous { fps: None },
        ) else {
            return;
        };

        // Get original_position before mutable borrow (only needed for transitioning)
        let original_position = self.state.media_info.medias[index].original_position;

        let img_size = texture.size_vec2();
        let viewport_size = avail_rect.size();

        // Calculate fit-to-screen scale
        let fit_scale = (viewport_size.x / img_size.x).min(viewport_size.y / img_size.y);

        // Determine actual scale based on zoom state
        let scale = if transitioning {
            // During transition, interpolate from original position
            let src_scale = (original_position.width() / img_size.x)
                .min(original_position.height() / img_size.y);
            egui::lerp(src_scale..=fit_scale, open_amount)
        } else {
            fit_scale
        };

        let scaled_size = img_size * scale;

        // Calculate position (centered)
        let center = if transitioning {
            let src_center = original_position.center().to_vec2();
            let dst_center = avail_rect.center().to_vec2();
            let lerped = src_center + (dst_center - src_center) * open_amount;
            lerped.to_pos2()
        } else {
            avail_rect.center()
        };

        let img_rect = Rect::from_center_size(center, scaled_size);

        // Paint the image
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(
            texture.id(),
            img_rect,
            uv,
            Color32::from_white_alpha((open_amount * 255.0) as u8),
        );
    }

    /// Render navigation arrows and handle clicks
    fn render_nav_arrows(&mut self, ui: &mut egui::Ui, avail_rect: &Rect) {
        let arrow_size = egui::vec2(60.0, 120.0);
        let margin = 20.0;

        // Only show arrows on hover (desktop behavior)
        let pointer_pos = ui.ctx().pointer_hover_pos();
        let show_arrows = pointer_pos.is_some_and(|p| avail_rect.contains(p));

        if !show_arrows {
            return;
        }

        let arrow_color = Color32::from_white_alpha(180);
        let hover_color = Color32::from_white_alpha(255);

        // Left arrow
        if self.state.has_prev() {
            let left_rect = Rect::from_min_size(
                pos2(
                    avail_rect.left() + margin,
                    avail_rect.center().y - arrow_size.y / 2.0,
                ),
                arrow_size,
            );

            let left_response =
                ui.interact(left_rect, ui.id().with("nav_left"), egui::Sense::click());

            let color = if left_response.hovered() {
                hover_color
            } else {
                arrow_color
            };

            // Draw left chevron
            Self::draw_chevron(ui, left_rect.center(), true, color);

            if left_response.clicked() {
                self.state.prev_image();
            }
        }

        // Right arrow
        if self.state.has_next() {
            let right_rect = Rect::from_min_size(
                pos2(
                    avail_rect.right() - margin - arrow_size.x,
                    avail_rect.center().y - arrow_size.y / 2.0,
                ),
                arrow_size,
            );

            let right_response =
                ui.interact(right_rect, ui.id().with("nav_right"), egui::Sense::click());

            let color = if right_response.hovered() {
                hover_color
            } else {
                arrow_color
            };

            // Draw right chevron
            Self::draw_chevron(ui, right_rect.center(), false, color);

            if right_response.clicked() {
                self.state.next_image();
            }
        }
    }

    /// Draw a chevron arrow
    fn draw_chevron(ui: &mut egui::Ui, center: egui::Pos2, left: bool, color: Color32) {
        let size = 20.0;
        let stroke = egui::Stroke::new(3.0, color);

        let (p1, p2, p3) = if left {
            (
                pos2(center.x + size * 0.5, center.y - size),
                pos2(center.x - size * 0.5, center.y),
                pos2(center.x + size * 0.5, center.y + size),
            )
        } else {
            (
                pos2(center.x - size * 0.5, center.y - size),
                pos2(center.x + size * 0.5, center.y),
                pos2(center.x - size * 0.5, center.y + size),
            )
        };

        ui.painter().line_segment([p1, p2], stroke);
        ui.painter().line_segment([p2, p3], stroke);
    }

    /// Render position indicator (e.g., "2/5")
    fn render_position_indicator(&self, ui: &mut egui::Ui, avail_rect: &Rect, open_amount: f32) {
        let text = format!(
            "{}/{}",
            self.state.current_index + 1,
            self.state.image_count()
        );

        let font_id = egui::FontId::proportional(14.0);
        let text_color = Color32::from_white_alpha((open_amount * 220.0) as u8);

        let galley = ui.painter().layout_no_wrap(text, font_id, text_color);

        // Position at bottom center with padding
        let padding = egui::vec2(12.0, 6.0);
        let pill_size = galley.size() + padding * 2.0;
        let pill_pos = pos2(
            avail_rect.center().x - pill_size.x / 2.0,
            avail_rect.bottom() - 50.0 - pill_size.y,
        );
        let pill_rect = Rect::from_min_size(pill_pos, pill_size);

        // Draw pill background
        ui.painter().rect_filled(
            pill_rect,
            pill_size.y / 2.0,
            Color32::from_black_alpha((open_amount * 150.0) as u8),
        );

        // Draw text
        let text_pos = pill_rect.min + padding;
        ui.painter().galley(text_pos, galley, text_color);
    }

    /// Render a close button in the top-right corner (touch-friendly)
    fn render_close_button(&mut self, ui: &mut egui::Ui, avail_rect: &Rect, open_amount: f32) {
        let button_size = 44.0; // Touch-friendly size
        let margin = 16.0;

        let button_rect = Rect::from_min_size(
            pos2(
                avail_rect.right() - margin - button_size,
                avail_rect.top() + margin,
            ),
            egui::vec2(button_size, button_size),
        );

        let response = ui.interact(
            button_rect,
            ui.id().with("close_button"),
            egui::Sense::click(),
        );

        // Draw button background (pill/circle)
        let bg_alpha = if response.hovered() { 180 } else { 120 };
        ui.painter().circle_filled(
            button_rect.center(),
            button_size / 2.0,
            Color32::from_black_alpha((open_amount * bg_alpha as f32) as u8),
        );

        // Draw X icon
        let icon_size = 12.0;
        let center = button_rect.center();
        let stroke_alpha = if response.hovered() { 255 } else { 220 };
        let stroke = egui::Stroke::new(
            2.5,
            Color32::from_white_alpha((open_amount * stroke_alpha as f32) as u8),
        );

        ui.painter().line_segment(
            [
                pos2(center.x - icon_size, center.y - icon_size),
                pos2(center.x + icon_size, center.y + icon_size),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                pos2(center.x + icon_size, center.y - icon_size),
                pos2(center.x - icon_size, center.y + icon_size),
            ],
            stroke,
        );

        if response.clicked() {
            self.close();
        }
    }
}
