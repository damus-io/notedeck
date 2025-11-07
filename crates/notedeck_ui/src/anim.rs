use egui::{vec2, Pos2, Rect, Response, Sense};

pub fn hover_expand(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: f32,
    expand_size: f32,
    anim_speed: f32,
) -> (egui::Rect, f32, egui::Response) {
    // Allocate space for the profile picture with a fixed size
    let default_size = size + expand_size;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(default_size, default_size), egui::Sense::click());

    let val = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered(), anim_speed);

    let size = size + val * expand_size;
    (rect, size, response)
}

#[inline]
pub fn hover_small_size() -> f32 {
    14.0
}

pub fn hover_expand_small(ui: &mut egui::Ui, id: egui::Id) -> (egui::Rect, f32, egui::Response) {
    let size = hover_small_size();
    let expand_size = 5.0;
    let anim_speed = 0.05;

    hover_expand(ui, id, size, expand_size, anim_speed)
}

pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;
pub static ANIM_SPEED: f32 = 0.05;
pub struct AnimationHelper {
    rect: Rect,
    center: Pos2,
    response: Response,
    animation_progress: f32,
    expansion_multiple: f32,
}

impl AnimationHelper {
    pub fn new(
        ui: &mut egui::Ui,
        animation_name: impl std::hash::Hash,
        max_size: egui::Vec2,
    ) -> Self {
        let id = ui.id().with(animation_name);
        let (rect, response) = ui.allocate_exact_size(max_size, Sense::click());

        let animation_progress =
            ui.ctx()
                .animate_bool_with_time(id, response.hovered(), ANIM_SPEED);

        Self {
            rect,
            center: rect.center(),
            response,
            animation_progress,
            expansion_multiple: ICON_EXPANSION_MULTIPLE,
        }
    }

    pub fn no_animation(ui: &mut egui::Ui, size: egui::Vec2, sense: Sense) -> Self {
        let (rect, response) = ui.allocate_exact_size(size, sense);

        Self {
            rect,
            center: rect.center(),
            response,
            animation_progress: 0.0,
            expansion_multiple: ICON_EXPANSION_MULTIPLE,
        }
    }

    pub fn new_from_rect(
        ui: &mut egui::Ui,
        animation_name: impl std::hash::Hash,
        animation_rect: egui::Rect,
    ) -> Self {
        let id = ui.id().with(animation_name);
        let response = ui.allocate_rect(animation_rect, Sense::click());

        let animation_progress =
            ui.ctx()
                .animate_bool_with_time(id, response.hovered(), ANIM_SPEED);

        Self {
            rect: animation_rect,
            center: animation_rect.center(),
            response,
            animation_progress,
            expansion_multiple: ICON_EXPANSION_MULTIPLE,
        }
    }

    pub fn scale_1d_pos(&self, min_object_size: f32) -> f32 {
        let max_object_size = min_object_size * self.expansion_multiple;

        if self.response.is_pointer_button_down_on() {
            min_object_size
        } else {
            min_object_size + ((max_object_size - min_object_size) * self.animation_progress)
        }
    }

    pub fn scale_radius(&self, min_diameter: f32) -> f32 {
        self.scale_1d_pos((min_diameter - 1.0) / 2.0)
    }

    pub fn get_animation_rect(&self) -> egui::Rect {
        self.rect
    }

    pub fn scaled_rect(&self) -> egui::Rect {
        let min_height = self.rect.height() * (1.0 / self.expansion_multiple);
        let min_width = self.rect.width() * (1.0 / self.expansion_multiple);

        egui::Rect::from_center_size(
            self.center,
            vec2(self.scale_1d_pos(min_width), self.scale_1d_pos(min_height)),
        )
    }

    pub fn center(&self) -> Pos2 {
        self.rect.center()
    }

    pub fn take_animation_response(self) -> egui::Response {
        self.response
    }

    // Scale a minimum position from center to the current animation position
    pub fn scale_from_center(&self, x_min: f32, y_min: f32) -> Pos2 {
        Pos2::new(
            self.center.x + self.scale_1d_pos(x_min),
            self.center.y + self.scale_1d_pos(y_min),
        )
    }

    pub fn scale_pos_from_center(&self, min_pos: Pos2) -> Pos2 {
        self.scale_from_center(min_pos.x, min_pos.y)
    }

    /// New method for min/max scaling when needed
    pub fn scale_1d_pos_min_max(&self, min_object_size: f32, max_object_size: f32) -> f32 {
        min_object_size + ((max_object_size - min_object_size) * self.animation_progress)
    }
}

pub struct PulseAlpha<'a> {
    ctx: &'a egui::Context,
    id: egui::Id,
    alpha_min: u8,
    alpha_max: u8,
    animation_speed: f32,
    start_max_alpha: bool,
}

impl<'a> PulseAlpha<'a> {
    pub fn new(ctx: &'a egui::Context, id: egui::Id, alpha_min: u8, alpha_max: u8) -> Self {
        Self {
            ctx,
            id,
            alpha_min,
            alpha_max,
            animation_speed: ANIM_SPEED,
            start_max_alpha: false,
        }
    }

    pub fn with_speed(mut self, speed: f32) -> Self {
        self.animation_speed = speed;
        self
    }

    pub fn start_max_alpha(mut self) -> Self {
        self.start_max_alpha = true;
        self
    }

    // returns the current alpha value for the frame
    pub fn animate(self) -> u8 {
        let pulse_direction = if let Some(pulse_dir) = self.ctx.data(|d| d.get_temp(self.id)) {
            pulse_dir
        } else {
            self.ctx
                .data_mut(|d| d.insert_temp(self.id, self.start_max_alpha));
            self.start_max_alpha
        };

        let alpha_min_f32 = self.alpha_min as f32;
        let target = if pulse_direction {
            self.alpha_max as f32 - alpha_min_f32
        } else {
            0.0
        };

        let cur_val = self
            .ctx
            .animate_value_with_time(self.id, target, self.animation_speed);

        if (target - cur_val).abs() < 0.5 {
            self.ctx
                .data_mut(|d| d.insert_temp(self.id, !pulse_direction));
        }

        (cur_val + alpha_min_f32).clamp(self.alpha_min as f32, self.alpha_max as f32) as u8
    }
}

/// Stateless rolling number using egui's internal animation memory.
/// Each digit has a different "speed" / easing.
pub fn rolling_number(ui: &mut egui::Ui, id_source: impl std::hash::Hash, value: u32) -> Response {
    let ctx = ui.ctx();
    let id = ui.make_persistent_id(id_source);

    // Global animated value (one float in egui's memory):
    let anim = ctx.animate_value_with_time(id, value as f32, 0.35);

    let anim_floor = anim.floor().max(0.0);
    let base = anim_floor as u32;
    let t_global = anim - anim_floor; // base step phase: 0..1
    let next = if t_global == 0.0 {
        base
    } else {
        base.saturating_add(1)
    };

    // Choose how many digits we want to show.
    let max_show = value.max(next);
    let num_digits = max_show.to_string().len().max(1);

    let font_size = 12.0;
    let font_id = egui::FontId::proportional(font_size);
    let color = ui.visuals().text_color();

    let response = ui.allocate_response(egui::Vec2::ZERO, Sense::hover());

    let prev_spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 0.0;
    //let pos = ui.available_rect_before_wrap().min;
    let digit_size = egui::vec2(7.0, font_size);

    for i in 0..num_digits {
        // Leftmost digit = index 0, rightmost = num_digits - 1
        let place = 10_u32.pow((num_digits - 1 - i) as u32);
        let from = (base / place) % 10;
        let to = (next / place) % 10;

        // Per-digit "speed": rightmost digits move more / earlier.
        let idx_from_right = (num_digits - 1 - i) as f32;
        // tweak these constants to taste:
        let speed_factor = 0.8 + 0.25 * idx_from_right; // higher place → slightly faster

        // Local phase for this digit:
        let mut t_digit = (t_global * speed_factor).clamp(0.0, 1.0);

        // Add a nice easing curve so some digits ease in/out:
        t_digit = ease_in_out_cubic(t_digit);

        draw_rolling_digit(
            ui, from as u8, to as u8, t_digit, &font_id, color, digit_size,
        );
    }

    ui.spacing_mut().item_spacing.x = prev_spacing;

    response
}

// Basic cubic ease-in-out
fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let t = 2.0 * t - 2.0;
        0.5 * t * t * t + 1.0
    }
}

fn draw_rolling_digit(
    ui: &mut egui::Ui,
    from: u8,
    to: u8,
    t: f32, // 0..1, already "warped" per digit
    font_id: &egui::FontId,
    color: egui::Color32,
    desired_size: egui::Vec2,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

    let painter = ui.painter().with_clip_rect(rect);

    let current_str = format!("{from}");
    let next_str = format!("{to}");

    let current_galley = painter.layout_no_wrap(current_str, font_id.clone(), color);
    let next_galley = painter.layout_no_wrap(next_str, font_id.clone(), color);

    let h = current_galley.rect.height().max(next_galley.rect.height());
    let center_x = rect.center().x;
    let center_y = rect.center().y;

    let current_y = egui::lerp(center_y..=center_y - h, t);
    let next_y = egui::lerp(center_y + h..=center_y, t);

    painter.galley(
        egui::pos2(
            center_x - current_galley.rect.width() * 0.5,
            current_y - current_galley.rect.height() * 0.5,
        ),
        current_galley,
        egui::Color32::RED,
    );

    painter.galley(
        egui::pos2(
            center_x - next_galley.rect.width() * 0.5,
            next_y - next_galley.rect.height() * 0.5,
        ),
        next_galley,
        egui::Color32::RED,
    );

    response
}
