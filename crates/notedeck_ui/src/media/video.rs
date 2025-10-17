use egui::{
    pos2, vec2, Align2, Color32, ColorImage, Context, CornerRadius, FontId, Id, Painter, Rect,
    Shape, Stroke, TextureHandle, TextureOptions, Ui,
};
use notedeck::{VideoFrame, VideoHandle};

#[derive(Clone, Default)]
pub struct VideoTextureState {
    pub handle: Option<VideoHandle>,
    pub poster_generation: u64,
    pub frame_generation: u64,
    pub poster_texture: Option<TextureHandle>,
    pub frame_texture: Option<TextureHandle>,
}

impl VideoTextureState {
    pub fn ensure_handle(&mut self, handle: VideoHandle) {
        if self.handle != Some(handle) {
            self.handle = Some(handle);
            self.poster_generation = 0;
            self.frame_generation = 0;
            self.poster_texture = None;
            self.frame_texture = None;
        }
    }

    pub fn poster_texture(
        &mut self,
        ui: &Ui,
        handle: VideoHandle,
        frame: &VideoFrame,
    ) -> TextureHandle {
        self.ensure_handle(handle);
        update_texture(
            ui,
            &mut self.poster_texture,
            &mut self.poster_generation,
            handle,
            "poster",
            frame,
        )
    }

    pub fn frame_texture(
        &mut self,
        ui: &Ui,
        handle: VideoHandle,
        frame: &VideoFrame,
    ) -> TextureHandle {
        self.ensure_handle(handle);
        update_texture(
            ui,
            &mut self.frame_texture,
            &mut self.frame_generation,
            handle,
            "frame",
            frame,
        )
    }
}

fn update_texture(
    ui: &Ui,
    slot: &mut Option<TextureHandle>,
    generation: &mut u64,
    handle: VideoHandle,
    suffix: &str,
    frame: &VideoFrame,
) -> TextureHandle {
    if *generation != frame.generation || slot.is_none() {
        let image = frame_to_image(frame);
        let name = format!("video:{}:{suffix}", handle.id.as_u64());
        match slot {
            Some(existing) => {
                existing.set(image, TextureOptions::LINEAR);
            }
            None => {
                let texture = ui.ctx().load_texture(name, image, TextureOptions::LINEAR);
                *slot = Some(texture);
            }
        }
        *generation = frame.generation;
    }

    slot.as_ref()
        .expect("texture must exist after update")
        .clone()
}

fn frame_to_image(frame: &VideoFrame) -> ColorImage {
    ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        frame.pixels.as_ref(),
    )
}

pub fn load_state(ctx: &Context, id: Id) -> VideoTextureState {
    ctx.data_mut(|data| {
        if let Some(existing) = data.get_persisted::<VideoTextureState>(id) {
            existing.clone()
        } else {
            VideoTextureState::default()
        }
    })
}

pub fn store_state(ctx: &Context, id: Id, state: VideoTextureState) {
    ctx.data_mut(|data| {
        data.insert_persisted(id, state);
    });
}

pub fn fit_rect_to_aspect(rect: Rect, aspect: f32) -> Rect {
    if aspect <= f32::EPSILON {
        return rect;
    }

    let size = rect.size();
    if size.x <= 0.0 || size.y <= 0.0 {
        return rect;
    }

    let current = size.x / size.y;
    if (current - aspect).abs() < f32::EPSILON {
        rect
    } else if current > aspect {
        let new_width = size.y * aspect;
        Rect::from_center_size(rect.center(), vec2(new_width, size.y))
    } else {
        let new_height = size.x / aspect;
        Rect::from_center_size(rect.center(), vec2(size.x, new_height))
    }
}

pub fn draw_play_overlay(painter: &Painter, rect: Rect) {
    let radius = rect.size().min_elem() * 0.18;
    let center = rect.center();

    painter.circle_filled(center, radius, Color32::from_black_alpha(160));

    let points = [
        pos2(center.x - radius * 0.35, center.y - radius * 0.6),
        pos2(center.x - radius * 0.35, center.y + radius * 0.6),
        pos2(center.x + radius * 0.75, center.y),
    ];
    painter.add(Shape::convex_polygon(
        points.to_vec(),
        Color32::WHITE,
        Stroke::NONE,
    ));
}

pub fn draw_pause_overlay(painter: &Painter, rect: Rect) {
    let radius = rect.size().min_elem() * 0.18;
    let center = rect.center();

    painter.circle_filled(center, radius, Color32::from_black_alpha(160));

    let bar_width = radius * 0.35;
    let bar_height = radius * 1.2;
    let spacing = bar_width * 0.7;

    let left = Rect::from_center_size(
        pos2(center.x - spacing * 0.5, center.y),
        vec2(bar_width, bar_height),
    );
    let right = Rect::from_center_size(
        pos2(center.x + spacing * 0.5, center.y),
        vec2(bar_width, bar_height),
    );

    let bar_corner = (bar_width * 0.25).clamp(0.0, 50.0) as u8;
    painter.rect_filled(left, CornerRadius::same(bar_corner), Color32::WHITE);
    painter.rect_filled(right, CornerRadius::same(bar_corner), Color32::WHITE);
}

pub fn draw_error_overlay(painter: &Painter, rect: Rect, message: &str) {
    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_black_alpha(200));

    let mut truncated = message.trim().to_owned();
    if truncated.len() > 96 {
        truncated.truncate(96);
        truncated.push('…');
    }

    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        truncated,
        FontId::proportional(13.0),
        Color32::from_rgb(255, 120, 120),
    );
}
