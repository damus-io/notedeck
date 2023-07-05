use egui::{Color32, ColorImage};

pub fn round_image(image: &mut ColorImage) {
    // The radius to the edge of of the avatar circle
    let edge_radius = image.size[0] as f32 / 2.0;
    let edge_radius_squared = edge_radius * edge_radius;

    for (pixnum, pixel) in image.pixels.iter_mut().enumerate() {
        // y coordinate
        let uy = pixnum / image.size[0];
        let y = uy as f32;
        let y_offset = edge_radius - y;

        // x coordinate
        let ux = pixnum % image.size[0];
        let x = ux as f32;
        let x_offset = edge_radius - x;

        // The radius to this pixel (may be inside or outside the circle)
        let pixel_radius_squared: f32 = x_offset * x_offset + y_offset * y_offset;

        // If inside of the avatar circle
        if pixel_radius_squared <= edge_radius_squared {
            // squareroot to find how many pixels we are from the edge
            let pixel_radius: f32 = pixel_radius_squared.sqrt();
            let distance = edge_radius - pixel_radius;

            // If we are within 1 pixel of the edge, we should fade, to
            // antialias the edge of the circle. 1 pixel from the edge should
            // be 100% of the original color, and right on the edge should be
            // 0% of the original color.
            if distance <= 1.0 {
                *pixel = Color32::from_rgba_premultiplied(
                    (pixel.r() as f32 * distance) as u8,
                    (pixel.g() as f32 * distance) as u8,
                    (pixel.b() as f32 * distance) as u8,
                    (pixel.a() as f32 * distance) as u8,
                );
            }
        } else {
            // Outside of the avatar circle
            *pixel = Color32::TRANSPARENT;
        }
    }
}
