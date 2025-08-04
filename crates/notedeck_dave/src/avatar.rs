use std::num::NonZeroU64;

use crate::mesh;
use crate::{Quaternion, Vec3};
use eframe::egui_wgpu::{
    self,
    wgpu::{self, util::DeviceExt},
};
use egui::{Rect, Response};
use rand::Rng;
use std::borrow::Cow;

pub struct DaveAvatar {
    rotation: Quaternion,
    rot_dir: Vec3,
    logical_time: f32,
}

// Matrix utilities for perspective projection
fn perspective_matrix(fovy_radians: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fovy_radians / 2.0).tan();
    let nf = 1.0 / (near - far);

    // Column-major for WGPU
    [
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        (far + near) * nf,
        -1.0,
        0.0,
        0.0,
        2.0 * far * near * nf,
        0.0,
    ]
}

// Combine two 4x4 matrices (column-major)
fn matrix_multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];

    for row in 0..4 {
        for col in 0..4 {
            let mut sum = 0.0;
            for i in 0..4 {
                sum += a[row + i * 4] * b[i + col * 4];
            }
            result[row + col * 4] = sum;
        }
    }

    result
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn generate_dave_instances(instance_count: u32) -> Vec<mesh::Instance> {
    let mut rng = rand::rng();
    let mut instances = Vec::with_capacity(instance_count as usize);

    // Logo gradient endpoints (0–1 range)
    const C0: [f32; 3] = [53.0 / 255.0, 77.0 / 255.0, 235.0 / 255.0]; // rgb(53, 77, 235)
    const C1: [f32; 3] = [229.0 / 255.0, 20.0 / 255.0, 205.0 / 255.0]; // rgb(229, 20, 205)
    let golden_angle = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());

    for i in 0..instance_count {
        let i_f = (i as f32) + 0.5;
        let n = instance_count as f32;

        // Fibonacci sphere (unit directions)
        let z = 1.0 - (2.0 * i_f) / n;
        let r = (1.0 - z * z).sqrt();
        let theta = golden_angle * i_f;

        // Use base_pos as *direction*; shader will normalize/scale anyway
        let base_pos = [r * theta.cos(), z, r * theta.sin()];

        let scale = 0.002;

        //let scale = scale + scale_var + rng.random::<f32>() * scale; // slightly smaller cubes
        let seed = rng.random::<f32>() * 1000.0;

        // damus logo gradient
        let t_base = (z + 1.0) * 0.5; // 0..1
        let t_jitter = (rng.random::<f32>() - 0.5) * 0.06; // ±0.03
        let t = (t_base + t_jitter).clamp(0.0, 1.0);
        let color = lerp3(C0, C1, t);

        instances.push(mesh::Instance {
            base_pos,
            scale,
            seed,
            color,
        });
    }

    instances
}

impl DaveAvatar {
    pub fn new(wgpu_render_state: &egui_wgpu::RenderState) -> Self {
        const BINDING_SIZE: u64 = 256;

        let device = &wgpu_render_state.device;
        let instance_count: u32 = 100000;
        let instances = generate_dave_instances(instance_count);

        // Create shader module with improved shader code
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("dave.wgsl"))),
        });

        // Create uniform buffer for MVP matrix and model matrix
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cube_uniform_buffer"),
            size: BINDING_SIZE, // Two 4x4 matrices of f32 (2 * 16 * 4 bytes)
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube_bind_group_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(BINDING_SIZE),
                },
                count: None,
            }],
        });

        // Create bind group
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cube_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Create pipeline layout
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cube_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube_vertices"),
            contents: bytemuck::cast_slice(&mesh::CUBE_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube_instances"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube_indices"),
            contents: bytemuck::cast_slice(&mesh::CUBE_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        // Create render pipeline
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[mesh::Vertex::LAYOUT, mesh::Instance::LAYOUT],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu_render_state.target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Store resources in renderer
        wgpu_render_state
            .renderer
            .write()
            .callback_resources
            .insert(CubeRenderResources {
                pipeline,
                bind_group,
                uniform_buffer,
                instance_buffer,
                vertex_buffer,
                index_buffer,
                instance_count,
            });

        let initial_rot = {
            let x_rotation = Quaternion::from_axis_angle(&Vec3::new(1.0, 0.0, 0.0), 0.5);
            let y_rotation = Quaternion::from_axis_angle(&Vec3::new(0.0, 1.0, 0.0), 0.5);

            // Apply rotations (order matters)
            y_rotation.multiply(&x_rotation)
        };

        Self {
            logical_time: 0.0,
            rotation: initial_rot,
            rot_dir: Vec3::new(0.0, 0.0, 0.0),
        }
    }
}

#[inline]
fn apply_friction(val: f32, friction: f32, clamp: f32) -> f32 {
    if val < clamp {
        0.0
    } else {
        val * friction
    }
}

impl DaveAvatar {
    pub fn random_nudge(&mut self) {
        self.random_nudge_with(1.0);
    }

    pub fn random_nudge_with(&mut self, force: f32) {
        let mut rng = rand::rng();

        let nudge = Vec3::new(
            rng.random::<f32>() * force,
            rng.random::<f32>() * force,
            rng.random::<f32>() * force,
        )
        .normalize();

        self.rot_dir.x += nudge.x;
        self.rot_dir.y += nudge.y;
        self.rot_dir.z += nudge.z;
    }

    pub fn render(&mut self, rect: Rect, ui: &mut egui::Ui) -> Response {
        let response = ui.allocate_rect(rect, egui::Sense::CLICK | egui::Sense::DRAG);

        // Update rotation based on drag or animation
        if response.dragged() {
            // Create rotation quaternions based on drag
            let dx = response.drag_delta().x;
            let dy = response.drag_delta().y;
            let x_rotation = Quaternion::from_axis_angle(&Vec3::new(1.0, 0.0, 0.0), dy * 0.01);
            let y_rotation = Quaternion::from_axis_angle(&Vec3::new(0.0, 1.0, 0.0), dx * 0.01);

            self.rot_dir = Vec3::new(dx, dy, 0.0);

            // Apply rotations (order matters)
            self.rotation = y_rotation.multiply(&x_rotation).multiply(&self.rotation);
        } else if response.clicked() {
            self.random_nudge_with(1.0);
        } else {
            // Continuous rotation - reduced speed and simplified axis
            let friction = 0.95;
            let clamp = 0.1;
            self.rot_dir.x = apply_friction(self.rot_dir.x, friction, clamp);
            self.rot_dir.y = apply_friction(self.rot_dir.y, friction, clamp);
            self.rot_dir.z = apply_friction(self.rot_dir.y, friction, clamp);

            // we only need to render if we're still spinning
            if self.rot_dir.x > clamp || self.rot_dir.y > clamp || self.rot_dir.z > clamp {
                let x_rotation =
                    Quaternion::from_axis_angle(&Vec3::new(1.0, 0.0, 0.0), self.rot_dir.y * 0.03);
                let y_rotation =
                    Quaternion::from_axis_angle(&Vec3::new(0.0, 1.0, 0.0), self.rot_dir.x * 0.03);
                let z_rotation =
                    Quaternion::from_axis_angle(&Vec3::new(0.0, 0.0, 1.0), self.rot_dir.z * 0.03);

                self.rotation = y_rotation
                    .multiply(&x_rotation)
                    .multiply(&z_rotation)
                    .multiply(&self.rotation);

                tracing::trace!("repainting due to avatar rotation");
                ui.ctx().request_repaint();
            }
        }

        // Create model matrix from rotation quaternion
        let model = self.rotation.to_matrix4();

        // Create projection matrix with proper depth range
        // Adjust aspect ratio based on rect dimensions
        let aspect = rect.width() / rect.height();
        let projection = perspective_matrix(std::f32::consts::PI / 4.0, aspect, 0.1, 100.0);

        // Create view matrix (move camera back a bit)
        let camera_pos = [0.0, 0.0, 1.5];

        // Right-handed look-at at origin; view is a translate by -camera_pos
        let [cx, cy, cz] = camera_pos;

        #[rustfmt::skip]
        let view = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            -cx, -cy, -cz, 1.0,
        ];

        let view_proj = matrix_multiply(&projection, &view);
        let is_light = if ui.ctx().theme() == egui::Theme::Light {
            1.0
        } else {
            -1.0
        };

        self.logical_time += ui.ctx().input(|i| i.stable_dt.min(0.1));

        // Add paint callback
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            GpuData {
                view_proj,
                model,
                camera_pos,
                time: self.logical_time,
                is_light: [is_light, 0.0, 0.0, 0.0],
            },
        ));

        response
    }
}

// Callback implementation
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuData {
    view_proj: [f32; 16], // Model-View-Projection matrix
    model: [f32; 16],     // Model matrix for lighting calculations
    camera_pos: [f32; 3], // xyz
    time: f32,
    is_light: [f32; 4],
}

impl egui_wgpu::CallbackTrait for GpuData {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let resources: &CubeRenderResources = resources.get().unwrap();

        // Update uniform buffer with both matrices
        queue.write_buffer(&resources.uniform_buffer, 0, bytemuck::bytes_of(self));

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let resources: &CubeRenderResources = resources.get().unwrap();

        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, &resources.bind_group, &[]);
        render_pass.set_vertex_buffer(0, resources.vertex_buffer.slice(..));
        render_pass.set_vertex_buffer(1, resources.instance_buffer.slice(..));
        render_pass.set_index_buffer(resources.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        render_pass.draw_indexed(
            0..mesh::CUBE_INDICES.len() as u32,
            0,
            0..resources.instance_count,
        );
    }
}

// Simple resources struct
struct CubeRenderResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    instance_count: u32,
}
