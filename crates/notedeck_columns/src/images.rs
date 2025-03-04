use egui::{pos2, Color32, ColorImage, Rect, Sense, SizeHint, TextureHandle};
use image::imageops::FilterType;
use notedeck::ImageCache;
use notedeck::Result;
use poll_promise::Promise;
use std::path;
use std::path::PathBuf;
use tokio::fs;

//pub type ImageCacheKey = String;
//pub type ImageCacheValue = Promise<Result<TextureHandle>>;
//pub type ImageCache = HashMap<String, ImageCacheValue>;

// NOTE(jb55): chatgpt wrote this because I was too dumb to
pub fn aspect_fill(
    ui: &mut egui::Ui,
    sense: Sense,
    texture_id: egui::TextureId,
    aspect_ratio: f32,
) -> egui::Response {
    let frame = ui.available_rect_before_wrap(); // Get the available frame space in the current layout
    let frame_ratio = frame.width() / frame.height();

    let (width, height) = if frame_ratio > aspect_ratio {
        // Frame is wider than the content
        (frame.width(), frame.width() / aspect_ratio)
    } else {
        // Frame is taller than the content
        (frame.height() * aspect_ratio, frame.height())
    };

    let content_rect = Rect::from_min_size(
        frame.min
            + egui::vec2(
                (frame.width() - width) / 2.0,
                (frame.height() - height) / 2.0,
            ),
        egui::vec2(width, height),
    );

    // Set the clipping rectangle to the frame
    //let clip_rect = ui.clip_rect(); // Preserve the original clipping rectangle
    //ui.set_clip_rect(frame);

    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

    let (response, painter) = ui.allocate_painter(ui.available_size(), sense);

    // Draw the texture within the calculated rect, potentially clipping it
    painter.rect_filled(content_rect, 0.0, ui.ctx().style().visuals.window_fill());
    painter.image(texture_id, content_rect, uv, Color32::WHITE);

    // Restore the original clipping rectangle
    //ui.set_clip_rect(clip_rect);
    response
}

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

fn process_pfp_bitmap(imgtyp: ImageType, image: &mut image::DynamicImage) -> ColorImage {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    match imgtyp {
        ImageType::Content(w, h) => {
            let image = image.resize(w, h, FilterType::CatmullRom); // DynamicImage
            let image_buffer = image.into_rgba8(); // RgbaImage (ImageBuffer)
            let color_image = ColorImage::from_rgba_unmultiplied(
                [
                    image_buffer.width() as usize,
                    image_buffer.height() as usize,
                ],
                image_buffer.as_flat_samples().as_slice(),
            );
            color_image
        }
        ImageType::Profile(size) => {
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
        ImageType::Original => {
            let image_buffer = image.clone().into_rgba8(); // RgbaImage (ImageBuffer)
            let color_image = ColorImage::from_rgba_unmultiplied(
                [
                    image_buffer.width() as usize,
                    image_buffer.height() as usize,
                ],
                image_buffer.as_flat_samples().as_slice(),
            );
            color_image
        }
    }
}

fn parse_img_response(response: ehttp::Response, imgtyp: ImageType) -> Result<ColorImage> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let content_type = response.content_type().unwrap_or_default();
    let size_hint = match imgtyp {
        ImageType::Profile(size) => SizeHint::Size(size, size),
        ImageType::Content(w, h) => SizeHint::Size(w, h),
        ImageType::Original => SizeHint::Size(0, 0),
    };

    if content_type.starts_with("image/svg") {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("load_svg");

        let mut color_image =
            egui_extras::image::load_svg_bytes_with_size(&response.bytes, Some(size_hint))?;
        round_image(&mut color_image);
        Ok(color_image)
    } else if content_type.starts_with("image/") {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("load_from_memory");
        let mut dyn_image = image::load_from_memory(&response.bytes)?;
        Ok(process_pfp_bitmap(imgtyp, &mut dyn_image))
    } else {
        Err(format!("Expected image, found content-type {:?}", content_type).into())
    }
}

fn fetch_img_from_disk(
    ctx: &egui::Context,
    url: &str,
    path: &path::Path,
    imgtyp: ImageType,
) -> Promise<Result<TextureHandle>> {
    let ctx = ctx.clone();
    let url = url.to_owned();
    let path = path.to_owned();
    Promise::spawn_async(async move {
        let data = fs::read(path).await?;
        let mut image_buffer = image::load_from_memory(&data).map_err(notedeck::Error::Image)?;

        let img = match imgtyp {
            ImageType::Profile(size) => {
                // Crop square
                let smaller = image_buffer.width().min(image_buffer.height());

                if image_buffer.width() > smaller {
                    let excess = image_buffer.width() - smaller;
                    image_buffer = image_buffer.crop_imm(
                        excess / 2,
                        0,
                        image_buffer.width() - excess,
                        image_buffer.height(),
                    );
                } else if image_buffer.height() > smaller {
                    let excess = image_buffer.height() - smaller;
                    image_buffer = image_buffer.crop_imm(
                        0,
                        excess / 2,
                        image_buffer.width(),
                        image_buffer.height() - excess,
                    );
                }

                let image_buffer = image_buffer.resize(size, size, FilterType::CatmullRom);
                let image_buffer = image_buffer.into_rgba8();
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
            ImageType::Content(w, h) => {
                let image_buffer = image_buffer.resize(w, h, FilterType::CatmullRom);
                let image_buffer = image_buffer.into_rgba8();
                ColorImage::from_rgba_unmultiplied(
                    [
                        image_buffer.width() as usize,
                        image_buffer.height() as usize,
                    ],
                    image_buffer.as_flat_samples().as_slice(),
                )
            }
            ImageType::Original => {
                let image_buffer = image_buffer.into_rgba8();
                ColorImage::from_rgba_unmultiplied(
                    [
                        image_buffer.width() as usize,
                        image_buffer.height() as usize,
                    ],
                    image_buffer.as_flat_samples().as_slice(),
                )
            }
        };

        Ok(ctx.load_texture(&url, img, Default::default()))
    })
}

pub fn fetch_binary_from_disk(path: PathBuf) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| notedeck::Error::Generic(e.to_string()))
}

/// Controls type-specific handling
#[derive(Debug, Clone, Copy)]
pub enum ImageType {
    /// Profile Image (size)
    Profile(u32),
    /// Content Image (width, height)
    Content(u32, u32),
    /// Original Image (width, height)
    Original,
}

pub fn fetch_img(
    img_cache: &ImageCache,
    ctx: &egui::Context,
    url: &str,
    imgtyp: ImageType,
) -> Promise<Result<TextureHandle>> {
    let key = ImageCache::key(url);
    let path = img_cache.cache_dir.join(key);

    if path.exists() {
        fetch_img_from_disk(ctx, url, &path, imgtyp)
    } else {
        fetch_img_from_net(&img_cache.cache_dir, ctx, url, imgtyp)
    }

    // TODO: fetch image from local cache
}

fn fetch_img_from_net(
    cache_path: &path::Path,
    ctx: &egui::Context,
    url: &str,
    imgtyp: ImageType,
) -> Promise<Result<TextureHandle>> {
    let (sender, promise) = Promise::new();
    let request = ehttp::Request::get(url);
    let ctx = ctx.clone();
    let cloned_url = url.to_owned();
    let cache_path = cache_path.to_owned();
    ehttp::fetch(request, move |response| {
        let handle = response
            .map_err(notedeck::Error::Generic)
            .and_then(|resp| parse_img_response(resp, imgtyp))
            .map(|img| {
                let texture_handle = ctx.load_texture(&cloned_url, img.clone(), Default::default());

                // write to disk
                std::thread::spawn(move || ImageCache::write(&cache_path, &cloned_url, img));

                texture_handle
            });

        sender.send(handle); // send the results back to the UI thread.
        ctx.request_repaint();
    });

    promise
}
