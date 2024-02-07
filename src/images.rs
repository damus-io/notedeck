use crate::error::Error;
use crate::result::Result;
use egui::{Color32, ColorImage, SizeHint, TextureHandle};
use image::imageops::FilterType;
use poll_promise::Promise;

pub fn round_image(image: &mut ColorImage) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

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

fn process_pfp_bitmap(size: u32, image: &mut image::DynamicImage) -> ColorImage {
    #[cfg(features = "profiling")]
    puffin::profile_function!();

    // Crop square
    let smaller = image.width().min(image.height());

    if image.width() > smaller {
        let excess = image.width() - smaller;
        *image = image.crop_imm(excess / 2, 0, image.width() - excess, image.height());
    } else if image.height() > smaller {
        let excess = image.height() - smaller;
        *image = image.crop_imm(0, excess / 2, image.width(), image.height() - excess);
    }
    let image = image.resize(size, size, FilterType::CatmullRom); // DynamicImage
    let image_buffer = image.into_rgba8(); // RgbaImage (ImageBuffer)
    let mut color_image = ColorImage::from_rgba_unmultiplied(
        [
            image_buffer.width() as usize,
            image_buffer.height() as usize,
        ],
        image_buffer.as_flat_samples().as_slice(),
    );
    round_image(&mut color_image);
    color_image
}

fn parse_img_response(response: ehttp::Response) -> Result<ColorImage> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let content_type = response.content_type().unwrap_or_default();
    let size: u32 = 100;

    if content_type.starts_with("image/svg") {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("load_svg");

        let mut color_image = egui_extras::image::load_svg_bytes_with_size(
            &response.bytes,
            Some(SizeHint::Size(size, size)),
        )?;
        round_image(&mut color_image);
        Ok(color_image)
    } else if content_type.starts_with("image/") {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("load_from_memory");
        let mut dyn_image = image::load_from_memory(&response.bytes)?;
        Ok(process_pfp_bitmap(size, &mut dyn_image))
    } else {
        Err(format!("Expected image, found content-type {:?}", content_type).into())
    }
}

pub fn fetch_img(ctx: &egui::Context, url: &str) -> Promise<Result<TextureHandle>> {
    // TODO: fetch image from local cache
    fetch_img_from_net(ctx, url)
}

fn fetch_img_from_net(ctx: &egui::Context, url: &str) -> Promise<Result<TextureHandle>> {
    let (sender, promise) = Promise::new();
    let request = ehttp::Request::get(url);
    let ctx = ctx.clone();
    let cloned_url = url.to_owned();
    ehttp::fetch(request, move |response| {
        let handle = response
            .map_err(Error::Generic)
            .and_then(parse_img_response)
            .map(|img| ctx.load_texture(&cloned_url, img, Default::default()));

        sender.send(handle); // send the results back to the UI thread.
        ctx.request_repaint();
    });
    promise
}
