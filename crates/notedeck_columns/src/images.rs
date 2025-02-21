use egui::{pos2, Color32, ColorImage, Rect, Sense, SizeHint};
use image::codecs::gif::GifDecoder;
use image::imageops::FilterType;
use image::AnimationDecoder;
use image::DynamicImage;
use image::FlatSamples;
use image::Frame;
use notedeck::Animation;
use notedeck::ImageFrame;
use notedeck::MediaCache;
use notedeck::MediaCacheType;
use notedeck::Result;
use notedeck::TextureFrame;
use notedeck::TexturedImage;
use poll_promise::Promise;
use std::collections::VecDeque;
use std::io::Cursor;
use std::path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;
use tokio::fs;

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

fn process_pfp_bitmap(imgtyp: ImageType, mut image: image::DynamicImage) -> ColorImage {
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
                image = image.crop_imm(excess / 2, 0, image.width() - excess, image.height());
            } else if image.height() > smaller {
                let excess = image.height() - smaller;
                image = image.crop_imm(0, excess / 2, image.width(), image.height() - excess);
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
    }
}

fn parse_img_response(response: ehttp::Response, imgtyp: ImageType) -> Result<ColorImage> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let content_type = response.content_type().unwrap_or_default();
    let size_hint = match imgtyp {
        ImageType::Profile(size) => SizeHint::Size(size, size),
        ImageType::Content(w, h) => SizeHint::Size(w, h),
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
        let dyn_image = image::load_from_memory(&response.bytes)?;
        Ok(process_pfp_bitmap(imgtyp, dyn_image))
    } else {
        Err(format!("Expected image, found content-type {:?}", content_type).into())
    }
}

fn fetch_img_from_disk(
    ctx: &egui::Context,
    url: &str,
    path: &path::Path,
    cache_type: MediaCacheType,
) -> Promise<Result<TexturedImage>> {
    let ctx = ctx.clone();
    let url = url.to_owned();
    let path = path.to_owned();
    Promise::spawn_async(async move {
        match cache_type {
            MediaCacheType::Image => {
                let data = fs::read(path).await?;
                let image_buffer =
                    image::load_from_memory(&data).map_err(notedeck::Error::Image)?;

                let img = buffer_to_color_image(
                    image_buffer.as_flat_samples_u8(),
                    image_buffer.width(),
                    image_buffer.height(),
                );
                Ok(TexturedImage::Static(ctx.load_texture(
                    &url,
                    img,
                    Default::default(),
                )))
            }
            MediaCacheType::Gif => {
                let gif_bytes = fs::read(path.clone()).await?; // Read entire file into a Vec<u8>
                generate_gif(ctx, url, &path, gif_bytes, false, |i| {
                    buffer_to_color_image(i.as_flat_samples_u8(), i.width(), i.height())
                })
            }
        }
    })
}

fn generate_gif(
    ctx: egui::Context,
    url: String,
    path: &path::Path,
    data: Vec<u8>,
    write_to_disk: bool,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> Result<TexturedImage> {
    let decoder = {
        let reader = Cursor::new(data.as_slice());
        GifDecoder::new(reader)?
    };
    let (tex_input, tex_output) = mpsc::sync_channel(4);
    let (maybe_encoder_input, maybe_encoder_output) = if write_to_disk {
        let (inp, out) = mpsc::sync_channel(4);
        (Some(inp), Some(out))
    } else {
        (None, None)
    };

    let mut frames: VecDeque<Frame> = decoder
        .into_frames()
        .collect::<std::result::Result<VecDeque<_>, image::ImageError>>()
        .map_err(|e| notedeck::Error::Generic(e.to_string()))?;

    let first_frame = frames.pop_front().map(|frame| {
        generate_animation_frame(
            &ctx,
            &url,
            0,
            frame,
            maybe_encoder_input.as_ref(),
            process_to_egui,
        )
    });

    let cur_url = url.clone();
    thread::spawn(move || {
        for (index, frame) in frames.into_iter().enumerate() {
            let texture_frame = generate_animation_frame(
                &ctx,
                &cur_url,
                index,
                frame,
                maybe_encoder_input.as_ref(),
                process_to_egui,
            );

            if tex_input.send(texture_frame).is_err() {
                tracing::error!("AnimationTextureFrame mpsc stopped abruptly");
                break;
            }
        }
    });

    if let Some(encoder_output) = maybe_encoder_output {
        let path = path.to_owned();

        thread::spawn(move || {
            let mut imgs = Vec::new();
            while let Ok(img) = encoder_output.recv() {
                imgs.push(img);
            }

            if let Err(e) = MediaCache::write_gif(&path, &url, imgs) {
                tracing::error!("Could not write gif to disk: {e}");
            }
        });
    }

    first_frame.map_or_else(
        || {
            Err(notedeck::Error::Generic(
                "first frame not found for gif".to_owned(),
            ))
        },
        |first_frame| {
            Ok(TexturedImage::Animated(Animation {
                other_frames: Default::default(),
                receiver: Some(tex_output),
                first_frame,
            }))
        },
    )
}

fn generate_animation_frame(
    ctx: &egui::Context,
    url: &str,
    index: usize,
    frame: image::Frame,
    maybe_encoder_input: Option<&SyncSender<ImageFrame>>,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + 'static,
) -> TextureFrame {
    let delay = Duration::from(frame.delay());
    let img = DynamicImage::ImageRgba8(frame.into_buffer());
    let color_img = process_to_egui(img);

    if let Some(sender) = maybe_encoder_input {
        if let Err(e) = sender.send(ImageFrame {
            delay,
            image: color_img.clone(),
        }) {
            tracing::error!("ImageFrame mpsc unexpectedly closed: {e}");
        }
    }

    TextureFrame {
        delay,
        texture: ctx.load_texture(format!("{}{}", url, index), color_img, Default::default()),
    }
}

fn buffer_to_color_image(
    samples: Option<FlatSamples<&[u8]>>,
    width: u32,
    height: u32,
) -> ColorImage {
    // TODO(jb55): remove unwrap here
    let flat_samples = samples.unwrap();
    ColorImage::from_rgba_unmultiplied([width as usize, height as usize], flat_samples.as_slice())
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
}

pub fn fetch_img(
    img_cache: &MediaCache,
    ctx: &egui::Context,
    url: &str,
    imgtyp: ImageType,
    cache_type: MediaCacheType,
) -> Promise<Result<TexturedImage>> {
    let key = MediaCache::key(url);
    let path = img_cache.cache_dir.join(key);

    if path.exists() {
        fetch_img_from_disk(ctx, url, &path, cache_type)
    } else {
        fetch_img_from_net(&img_cache.cache_dir, ctx, url, imgtyp, cache_type)
    }

    // TODO: fetch image from local cache
}

fn fetch_img_from_net(
    cache_path: &path::Path,
    ctx: &egui::Context,
    url: &str,
    imgtyp: ImageType,
    cache_type: MediaCacheType,
) -> Promise<Result<TexturedImage>> {
    let (sender, promise) = Promise::new();
    let request = ehttp::Request::get(url);
    let ctx = ctx.clone();
    let cloned_url = url.to_owned();
    let cache_path = cache_path.to_owned();
    ehttp::fetch(request, move |response| {
        let handle = response.map_err(notedeck::Error::Generic).and_then(|resp| {
            match cache_type {
                MediaCacheType::Image => {
                    let img = parse_img_response(resp, imgtyp);
                    img.map(|img| {
                        let texture_handle =
                            ctx.load_texture(&cloned_url, img.clone(), Default::default());

                        // write to disk
                        std::thread::spawn(move || {
                            MediaCache::write(&cache_path, &cloned_url, img)
                        });

                        TexturedImage::Static(texture_handle)
                    })
                }
                MediaCacheType::Gif => {
                    let gif_bytes = resp.bytes;
                    generate_gif(
                        ctx.clone(),
                        cloned_url,
                        &cache_path,
                        gif_bytes,
                        true,
                        move |img| process_pfp_bitmap(imgtyp, img),
                    )
                }
            }
        });

        sender.send(handle); // send the results back to the UI thread.
        ctx.request_repaint();
    });

    promise
}
