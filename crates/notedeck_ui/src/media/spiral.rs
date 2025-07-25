/// Spiral layout for media galleries

use egui::{pos2, vec2, Color32, Rect, Sense, TextureId, Vec2};

#[derive(Clone, Copy, Debug)]
pub struct ImageItem {
    pub texture: TextureId,
    pub ar: f32, // width / height (must be > 0)
}

#[derive(Clone, Debug)]
struct Placed {
    texture: TextureId,
    rect: Rect,
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutParams {
    pub gutter: f32,
    pub h_min: f32,
    pub h_max: f32,
    pub w_min: f32,
    pub w_max: f32,
    pub seed_center: bool,
}

pub fn layout_spiral(images: &[ImageItem], params: LayoutParams) -> (Vec<Placed>, Vec2) {
    if images.is_empty() {
        return (Vec::new(), vec2(0.0, 0.0));
    }

    let eps = f32::EPSILON;
    let g = params.gutter.max(0.0);
    let h_min = params.h_min.max(1.0);
    let h_max = params.h_max.max(h_min);
    let w_min = params.w_min.max(1.0);
    let w_max = params.w_max.max(w_min);

    let mut placed = Vec::with_capacity(images.len());

    // Build around origin; normalize at the end.
    let mut x_min = 0.0f32;
    let mut x_max = 0.0f32;
    let mut y_min = 0.0f32;
    let mut y_max = 0.0f32;

    // dir: 0 right-col, 1 top-row, 2 left-col, 3 bottom-row
    let mut dir = 0usize;
    let mut i = 0usize;

    // Optional seed: center a single image
    if params.seed_center && i < images.len() {
        let ar = images[i].ar.max(eps);
        let h = ((h_min + h_max) * 0.5).clamp(h_min, h_max);
        let w = ar * h;

        let rect = Rect::from_center_size(pos2(0.0, 0.0), vec2(w, h));
        placed.push(Placed { texture: images[i].texture, rect });

        x_min = rect.min.x;
        x_max = rect.max.x;
        y_min = rect.min.y;
        y_max = rect.max.y;

        i += 1;
        dir = 1; // start by adding a row above
    } else {
        // ensure non-empty bbox for the first strip
        x_min = 0.0; x_max = 1.0; y_min = 0.0; y_max = 1.0;
    }

    // --- helpers -------------------------------------------------------------

    // Choose how many items fit and the strip size S (W for column, H for row).
    fn choose_k<F: Fn(&ImageItem) -> f32>(
        images: &[ImageItem],
        L: f32,
        g: f32,
        s_min: f32,
        s_max: f32,
        weight: F,
    ) -> (usize, f32) {
        // prefix sums of weights (sum over first k items)
        let mut pref = Vec::with_capacity(images.len() + 1);
        pref.push(0.0);
        for im in images {
            pref.push(pref.last().copied().unwrap_or(0.0) + weight(im));
        }

        let k_max = images.len().max(1);
        let mut chosen_k = 1usize;
        let mut chosen_s = f32::NAN;

        for k in 1..=k_max {
            let L_eff = (L - g * (k as f32 - 1.0)).max(1.0);
            let sum_w = pref[k].max(f32::EPSILON);
            let s = (L_eff / sum_w).max(1.0);

            if s > s_max && k < k_max {
                continue; // too big; add one more to thin the strip
            }
            if s < s_min {
                // prefer one fewer if possible
                if k > 1 {
                    let k2 = k - 1;
                    let L_eff2 = (L - g * (k2 as f32 - 1.0)).max(1.0);
                    let sum_w2 = pref[k2].max(f32::EPSILON);
                    chosen_k = k2;
                    chosen_s = (L_eff2 / sum_w2).max(1.0);
                } else {
                    chosen_k = 1;
                    chosen_s = s_min;
                }
                return (chosen_k, chosen_s);
            }
            return (k, s); // within bounds
        }

        // Fell through: use k_max and clamp
        let L_eff = (L - g * (k_max as f32 - 1.0)).max(1.0);
        let sum_w = pref[k_max].max(f32::EPSILON);
        let s = (L_eff / sum_w).clamp(s_min, s_max);
        (k_max, s)
    }

    // Place a column (top→bottom). Returns the new right/left edge.
    fn place_column(
        placed: &mut Vec<Placed>,
        strip: &[ImageItem],
        W: f32,
        x: f32,
        y_top: f32,
        g: f32,
    ) -> f32 {
        let mut y = y_top;
        for (idx, im) in strip.iter().enumerate() {
            let h = (W / im.ar.max(f32::EPSILON)).max(1.0);
            let rect = Rect::from_min_size(pos2(x, y), vec2(W, h));
            placed.push(Placed { texture: im.texture, rect });
            y += h;
            if idx + 1 != strip.len() { y += g; }
        }
        x + W
    }

    // Place a row (left→right). Returns the new top/bottom edge.
    fn place_row(
        placed: &mut Vec<Placed>,
        strip: &[ImageItem],
        H: f32,
        x_left: f32,
        y: f32,
        g: f32,
    ) -> f32 {
        let mut x = x_left;
        for (idx, im) in strip.iter().enumerate() {
            let w = (im.ar.max(f32::EPSILON) * H).max(1.0);
            let rect = Rect::from_min_size(pos2(x, y), vec2(w, H));
            placed.push(Placed { texture: im.texture, rect });
            x += w;
            if idx + 1 != strip.len() { x += g; }
        }
        y + H
    }

    // --- main loop -----------------------------------------------------------

    while i < images.len() {
        let remaining = &images[i..];

        if dir % 2 == 0 {
            // COLUMN (dir 0: right, 2: left)
            let L = (y_max - y_min).max(1.0);
            let (k, W) = choose_k(
                remaining,
                L, g, w_min, w_max,
                |im| 1.0 / im.ar.max(f32::EPSILON),
            );

            let x = if dir == 0 { x_max + g } else { x_min - g - W };
            let new_edge = place_column(&mut placed, &remaining[..k], W, x, y_min, g);
            if dir == 0 { x_max = new_edge; } else { x_min = x; }
            i += k;
        } else {
            // ROW (dir 1: top, 3: bottom)
            let L = (x_max - x_min).max(1.0);
            let (k, H) = choose_k(
                remaining,
                L, g, h_min, h_max,
                |im| im.ar.max(f32::EPSILON),
            );

            let y = if dir == 1 { y_max + g } else { y_min - g - H };
            let new_edge = place_row(&mut placed, &remaining[..k], H, x_min, y, g);
            if dir == 1 { y_max = new_edge; } else { y_min = y; }
            i += k;
        }

        dir = (dir + 1) % 4;
    }

    // Normalize so bbox top-left is (0,0)
    let shift = vec2(-x_min, -y_min);
    for p in &mut placed {
        p.rect = p.rect.translate(shift);
    }
    let total_size = vec2(x_max - x_min, y_max - y_min);
    (placed, total_size)
}

pub fn spiral_gallery(ui: &mut egui::Ui, images: &[ImageItem], params: LayoutParams) {
    use egui::{ScrollArea, Stroke};

    let (placed, size) = layout_spiral(images, params);

    ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let (rect, _resp) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_stroke(
            Rect::from_min_size(rect.min, size),
            0.0,
            Stroke::new(1.0, Color32::DARK_GRAY),
        );

        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        for p in &placed {
            let r = Rect::from_min_max(rect.min + p.rect.min.to_vec2(),
                                       rect.min + p.rect.max.to_vec2());
            painter.image(p.texture, r, uv, Color32::WHITE);
        }
    });
}
