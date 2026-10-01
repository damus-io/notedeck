//! Offscreen orbit views: one model, drawn alone into a texture of its own
//! through its own camera, re-rendered only when that camera moves.
//!
//! A [`Renderer`]'s scene has one world and one camera, so drawing it twice
//! in a frame shows the same thing twice. A [`ModelView`] sidesteps that: it
//! owns its colour and depth targets, its camera uniforms and the object
//! transform it draws with, and borrows only the renderer's pipelines,
//! lighting environment and the model's GPU data. Any number of views can
//! show different models (or the same one from different angles) in the
//! same frame, each as an ordinary texture the UI samples.

use glam::{Mat4, Vec3};

use crate::camera::{ArcballController, Camera};
use crate::model::{Aabb, Model};
use crate::{Globals, GpuData, ObjectUniform, Renderer, create_depth, write_gpu_data};

/// Largest side a view's targets are created at, in pixels.
const MAX_VIEW_SIDE: u32 = 4096;

/// How far past the model's bounding sphere the starting camera sits.
const FIT_PADDING: f32 = 1.2;

/// One model drawn into its own texture, orbited by dragging.
///
/// Made by [`Renderer::create_model_view`] and drawn by
/// [`Renderer::render_model_view`], which does nothing until the camera has
/// moved since the last render. The texture is
/// `RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC` in the renderer's colour
/// format, ready to register with a UI (or read back in a test).
pub struct ModelView {
    model: Model,
    size: (u32, u32),
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// This view's camera and light uniforms, bound with the renderer's
    /// shadow map for the main pass.
    globals: GpuData<Globals>,
    /// The same uniform buffer, bound alone for the shadow pass.
    shadow_globals_bg: wgpu::BindGroup,
    /// The model's transform (identity), in a buffer of its own so the
    /// renderer's per-scene object buffer is never touched.
    object_buf: wgpu::Buffer,
    object_bg: wgpu::BindGroup,
    bounds: Aabb,
    /// Where the camera starts and [`reset`](Self::reset) returns it.
    home: ArcballController,
    orbit: ArcballController,
    /// The fitted camera's lens, kept so the orbit only moves the eye.
    lens: Camera,
    dirty: bool,
}

impl ModelView {
    /// The model this view draws.
    pub fn model(&self) -> Model {
        self.model
    }

    /// The texture's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The texture the view renders into.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.color
    }

    /// A view of [`texture`](Self::texture), for registering with a UI.
    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.color_view
    }

    /// Turn the camera around the model by a drag of `dx`, `dy` pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.orbit.on_drag(dx, dy);
        self.dirty = true;
    }

    /// Move the camera in (positive `delta`) or out.
    pub fn zoom(&mut self, delta: f32) {
        if delta == 0.0 {
            return;
        }
        self.orbit.on_scroll(delta);
        self.dirty = true;
    }

    /// Put the camera back where it started.
    pub fn reset(&mut self) {
        self.orbit = self.home.clone();
        self.dirty = true;
    }

    /// Whether the camera has moved since the last render.
    pub fn needs_render(&self) -> bool {
        self.dirty
    }

    /// The camera the orbit currently looks through.
    fn camera(&self) -> Camera {
        let radius = self.bounds.half_extents().length().max(1e-4);
        Camera {
            eye: self.orbit.eye(),
            target: self.orbit.target,
            znear: (self.orbit.distance - radius * 2.0).max(radius * 0.01),
            zfar: self.orbit.distance + radius * 50.0,
            ..self.lens
        }
    }
}

/// An orthographic light-space matrix whose box just holds `bounds`, for the
/// light shining along `light_dir`, so any model's shadow lands in the map at
/// a usable resolution whatever its scale.
fn light_view_proj(bounds: &Aabb, light_dir: Vec3) -> Mat4 {
    let center = bounds.center();
    let radius = bounds.half_extents().length().max(1e-4);
    let dir = light_dir.normalize();
    let eye = center - dir * radius * 3.0;
    // look_at_rh needs an up that isn't parallel to the light.
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(eye, center, up);
    let proj = Mat4::orthographic_rh(-radius, radius, -radius, radius, radius, radius * 5.0);
    proj * view
}

impl Renderer {
    /// A view of `model` into a `size` texture, its camera fitted to the
    /// model's bounds. `None` when the model isn't loaded. The first
    /// [`render_model_view`](Self::render_model_view) draws it.
    pub fn create_model_view(
        &self,
        device: &wgpu::Device,
        model: Model,
        size: (u32, u32),
    ) -> Option<ModelView> {
        let bounds = self.models.get(&model)?.bounds;
        let (width, height) = (
            size.0.clamp(1, MAX_VIEW_SIDE),
            size.1.clamp(1, MAX_VIEW_SIDE),
        );

        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model_view_color"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.color_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let (depth, depth_view) = create_depth(device, width, height);

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model_view_globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = crate::make_globals_bindgroup(
            device,
            &self.globals_bgl,
            &globals_buf,
            &self.shadow_view,
            &self.shadow_sampler,
        );
        let shadow_globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model_view_shadow_globals"),
            layout: &self.shadow_globals_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let mut data = self.globals.data;
        data.light_view_proj = light_view_proj(&bounds, data.light_dir);
        let globals = GpuData {
            data,
            buffer: globals_buf,
            bindgroup: globals_bg,
        };

        let obj_size = std::mem::size_of::<ObjectUniform>() as u64;
        let object_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model_view_object"),
            size: obj_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let object_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model_view_object_bg"),
            layout: &self.object_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &object_buf,
                    offset: 0,
                    size: std::num::NonZeroU64::new(obj_size),
                }),
            }],
        });

        let lens = Camera::fit_to_aabb(
            bounds.min,
            bounds.max,
            width as f32 / height as f32,
            45_f32.to_radians(),
            FIT_PADDING,
        );
        let radius = bounds.half_extents().length().max(1e-4);
        let home = ArcballController {
            min_distance: radius * 0.5,
            max_distance: radius * 20.0,
            ..ArcballController::from_camera(&lens)
        };

        Some(ModelView {
            model,
            size: (width, height),
            color,
            color_view,
            _depth: depth,
            depth_view,
            globals,
            shadow_globals_bg,
            object_buf,
            object_bg,
            bounds,
            orbit: home.clone(),
            home,
            lens,
            dirty: true,
        })
    }

    /// Draw `view` if its camera moved since it was last drawn; returns
    /// whether it did. Records the model's shadow pass (into the renderer's
    /// shared shadow map) and the skybox and model into the view's texture,
    /// then submits. Nothing waits on the GPU, so this is safe on the UI
    /// thread; queue order puts it ahead of any later submit that samples
    /// the texture.
    pub fn render_model_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &mut ModelView,
    ) -> bool {
        if !view.dirty {
            return false;
        }
        let Some(model_data) = self.models.get(&view.model) else {
            return false;
        };
        view.dirty = false;

        let (w, h) = view.size;
        let camera = view.camera();
        view.globals.data.time = self.start.elapsed().as_secs_f32();
        view.globals.data.resolution = glam::Vec2::new(w as f32, h as f32);
        view.globals.data.set_camera(w as f32, h as f32, &camera);
        write_gpu_data(queue, &view.globals);
        queue.write_buffer(
            &view.object_buf,
            0,
            bytemuck::bytes_of(&ObjectUniform::from_model(Mat4::IDENTITY)),
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("model_view"),
        });

        {
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model_view_shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            shadow_pass.set_pipeline(&self.shadow_pipeline);
            shadow_pass.set_bind_group(0, &view.shadow_globals_bg, &[]);
            shadow_pass.set_bind_group(1, &view.object_bg, &[0]);
            crate::draw_model(&mut shadow_pass, model_data, false);
        }

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model_view_main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view.color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            rpass.set_pipeline(&self.skybox_pipeline);
            rpass.set_bind_group(0, &view.globals.bindgroup, &[]);
            rpass.set_bind_group(1, &view.object_bg, &[0]);
            rpass.set_bind_group(2, &self.material.bindgroup, &[]);
            rpass.set_bind_group(3, &self.ibl.bindgroup, &[]);
            rpass.draw(0..3, 0..1);

            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &view.globals.bindgroup, &[]);
            rpass.set_bind_group(1, &view.object_bg, &[0]);
            rpass.set_bind_group(3, &self.ibl.bindgroup, &[]);
            crate::draw_model(&mut rpass, model_data, true);
        }

        queue.submit(Some(encoder.finish()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("no GPU adapter: run via scripts/snapshot-test (lavapipe)");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device")
    }

    /// Read a view's texture back as tightly packed RGBA8 rows.
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, view: &ModelView) -> Vec<u8> {
        let (w, h) = view.size();
        let row = w * 4;
        let padded = row.div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            view.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = slice.get_mapped_range().expect("mapped");
        (0..h as usize)
            .flat_map(|y| data[y * padded as usize..][..row as usize].to_vec())
            .collect()
    }

    /// A cube drawn into a view covers its centre (which then isn't the
    /// skybox in the corner), renders only when the camera moved, and an
    /// orbit changes the picture.
    #[test]
    #[ignore] // needs a GPU adapter (lavapipe): run via scripts/snapshot-test
    fn model_view_renders_and_orbits() {
        let (device, queue) = headless_device();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = Renderer::new(&device, &queue, format, (64, 64));
        let data = renderer
            .model_uploader(&device, &queue)
            .upload_gltf_slice(&crate::test_util::cube_glb([0.9, 0.2, 0.1, 1.0]))
            .expect("cube uploads");
        assert_eq!(data.triangles(), 12);
        let model = renderer.insert_model(data);
        let mut view = renderer
            .create_model_view(&device, model, (64, 64))
            .expect("model is loaded");

        assert!(renderer.render_model_view(&device, &queue, &mut view));
        assert!(!renderer.render_model_view(&device, &queue, &mut view));
        let first = read_back(&device, &queue, &view);
        let px = |img: &[u8], x: usize, y: usize| img[(y * 64 + x) * 4..][..4].to_vec();
        assert_ne!(
            px(&first, 32, 32),
            px(&first, 0, 0),
            "cube covers the centre"
        );

        view.orbit(200.0, 0.0);
        assert!(renderer.render_model_view(&device, &queue, &mut view));
        let turned = read_back(&device, &queue, &view);
        assert_ne!(first, turned, "orbiting changes the picture");

        view.reset();
        assert!(renderer.render_model_view(&device, &queue, &mut view));
        assert_eq!(
            first,
            read_back(&device, &queue, &view),
            "reset returns home"
        );
    }

    /// The light box for a unit cube at the origin maps the cube's centre
    /// into the middle of clip space and every corner inside it.
    #[test]
    fn light_view_proj_holds_the_bounds() {
        let bounds = Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        };
        let m = light_view_proj(&bounds, Vec3::new(-0.5, -0.7, -0.3));
        let c = m.project_point3(Vec3::ZERO);
        assert!(c.x.abs() < 1e-4 && c.y.abs() < 1e-4, "{c}");
        for x in [-0.5, 0.5] {
            for y in [-0.5, 0.5] {
                for z in [-0.5, 0.5] {
                    let p = m.project_point3(Vec3::new(x, y, z));
                    assert!(p.x.abs() <= 1.0 && p.y.abs() <= 1.0, "{p}");
                    assert!((0.0..=1.0).contains(&p.z), "{p}");
                }
            }
        }
    }
}
