//! Drawing egui's output into a texture of our own instead of a window's
//! surface, and reading it back as an image.
//!
//! The renderer is set up as eframe sets up a window's (the same
//! [`RendererOptions`] from the same [`NativeOptions`], the same clear
//! colour), so a shot draws what the window would. What differs is only
//! where the pixels go: a texture made here, copied out after the frame,
//! rather than a surface presented to a display.

use std::iter;
use std::time::Duration;

use eframe::egui_wgpu::{self, wgpu, RenderState, RendererOptions, ScreenDescriptor};
use eframe::NativeOptions;
use image::RgbaImage;

/// How long to wait for the GPU to finish a frame or a copy before giving
/// up: generous, since a software rasterizer (lavapipe) draws a large
/// window slowly.
const GPU_TIMEOUT: Duration = Duration::from_secs(30);

/// A wgpu device with no surface, and egui's renderer on it.
pub struct Offscreen {
    render_state: RenderState,
    /// The depth buffer format a window gets, so pipelines that test depth
    /// (Dave's 3D avatar) find one here too.
    depth_format: Option<wgpu::TextureFormat>,
}

/// The renderer options eframe derives from `native` for a window.
fn window_renderer_options(native: &NativeOptions) -> RendererOptions {
    RendererOptions {
        msaa_samples: native.multisampling as u32,
        depth_stencil_format: egui_wgpu::depth_format_from_bits(
            native.depth_buffer,
            native.stencil_buffer,
        ),
        dithering: native.dithering,
        predictable_texture_filtering: false,
    }
}

impl Offscreen {
    /// A device on the adapter eframe would pick, with no window, no
    /// surface and no display.
    pub async fn new(native: &NativeOptions) -> Result<Self, egui_wgpu::WgpuError> {
        let config = &native.wgpu_options;
        let instance = config.wgpu_setup.new_instance().await;
        let options = window_renderer_options(native);
        let render_state = RenderState::create(config, &instance, None, options).await?;
        Ok(Self {
            render_state,
            depth_format: options.depth_stencil_format,
        })
    }

    /// What apps draw their own GPU work with (3D avatars, models), as a
    /// window's [`eframe::CreationContext`] would hand them.
    pub fn render_state(&self) -> &RenderState {
        &self.render_state
    }

    /// Upload the textures a frame made or changed. Every frame's must be,
    /// drawn or not, since a later frame only sends what changed.
    pub fn set_textures(&self, delta: &egui::TexturesDelta) {
        let mut renderer = self.render_state.renderer.write();
        // Each texture's updates apply in order; across textures order
        // doesn't matter.
        for (id, images) in &delta.set {
            for image in images {
                renderer.update_texture(
                    &self.render_state.device,
                    &self.render_state.queue,
                    *id,
                    image,
                );
            }
        }
    }

    /// Free the textures a frame let go of, once it is drawn (or won't be).
    pub fn free_textures(&self, delta: &egui::TexturesDelta) {
        let mut renderer = self.render_state.renderer.write();
        for id in &delta.free {
            renderer.free_texture(id);
        }
    }

    /// Draw one frame's tessellated output into a fresh `size_px` texture
    /// cleared to `clear_color`, and read it back.
    ///
    /// The image is opaque: a window on a display without a compositor
    /// shows no alpha, so neither does its shot.
    pub fn render(
        &self,
        primitives: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        size_px: [u32; 2],
        clear_color: [f32; 4],
    ) -> Result<RgbaImage, String> {
        let device = &self.render_state.device;
        let queue = &self.render_state.queue;
        let screen = ScreenDescriptor {
            size_in_pixels: size_px,
            pixels_per_point,
        };
        let extent = wgpu::Extent3d {
            width: size_px[0],
            height: size_px[1],
            depth_or_array_layers: 1,
        };

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shot_target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.render_state.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = self.depth_format.map(|format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("shot_depth"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shot_encoder"),
        });
        let mut renderer = self.render_state.renderer.write();
        let user_buffers =
            renderer.update_buffers(device, queue, &mut encoder, primitives, &screen);
        {
            let [r, g, b, a] = clear_color.map(f64::from);
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shot_render"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: depth_view.as_ref().map(|view| {
                        wgpu::RenderPassDepthStencilAttachment {
                            view,
                            depth_ops: self.depth_format.filter(|f| f.has_depth_aspect()).map(
                                |_| wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(1.0),
                                    store: wgpu::StoreOp::Discard,
                                },
                            ),
                            stencil_ops: self.depth_format.filter(|f| f.has_stencil_aspect()).map(
                                |_| wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(0),
                                    store: wgpu::StoreOp::Discard,
                                },
                            ),
                        }
                    }),
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render(&mut pass, primitives, &screen);
        }
        drop(renderer);
        queue.submit(user_buffers.into_iter().chain(iter::once(encoder.finish())));

        self.read_back(&target)
    }

    /// Copy `texture` out to an RGBA image.
    fn read_back(&self, texture: &wgpu::Texture) -> Result<RgbaImage, String> {
        let device = &self.render_state.device;
        let width = texture.width();
        let height = texture.height();
        let row_bytes = width as usize * 4;
        // A buffer copy's rows must be aligned; the padding is dropped below.
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded_row_bytes = row_bytes.div_ceil(align) * align;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shot_readback"),
            size: (padded_row_bytes * height as usize) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shot_copy"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes as u32),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        let submission = self.render_state.queue.submit(iter::once(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| drop(sender.send(res)));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_TIMEOUT),
            })
            .map_err(|err| format!("waiting for the GPU: {err}"))?;
        receiver
            .recv()
            .map_err(|err| format!("reading the frame back: {err}"))?
            .map_err(|err| format!("reading the frame back: {err}"))?;

        let bgra = matches!(
            self.render_state.target_format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mapped = slice
            .get_mapped_range()
            .map_err(|err| format!("reading the frame back: {err}"))?;
        let mut pixels = Vec::with_capacity(row_bytes * height as usize);
        for row in mapped.chunks_exact(padded_row_bytes) {
            let (row_pixels, _) = row[..row_bytes].as_chunks::<4>();
            for &[c0, c1, c2, _] in row_pixels {
                let [r, g, b] = if bgra { [c2, c1, c0] } else { [c0, c1, c2] };
                pixels.extend_from_slice(&[r, g, b, u8::MAX]);
            }
        }
        RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| "the frame's size doesn't match its pixels".to_owned())
    }
}
