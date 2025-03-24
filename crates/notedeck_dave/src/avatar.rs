use std::num::NonZeroU64;

use eframe::egui_wgpu::{self, wgpu};
use egui::{Rect, Response};

pub struct DaveAvatar {
    rotation: Quaternion,
}

// A simple quaternion implementation
struct Quaternion {
    x: f32,
    y: f32,
    z: f32,
    w: f32,
}

impl Quaternion {
    // Create identity quaternion
    fn identity() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        }
    }

    // Create from axis-angle representation
    fn from_axis_angle(axis: [f32; 3], angle: f32) -> Self {
        let half_angle = angle * 0.5;
        let s = half_angle.sin();
        Self {
            x: axis[0] * s,
            y: axis[1] * s,
            z: axis[2] * s,
            w: half_angle.cos(),
        }
    }

    // Multiply two quaternions (combines rotations)
    fn multiply(&self, other: &Self) -> Self {
        Self {
            x: self.w * other.x + self.x * other.w + self.y * other.z - self.z * other.y,
            y: self.w * other.y - self.x * other.z + self.y * other.w + self.z * other.x,
            z: self.w * other.z + self.x * other.y - self.y * other.x + self.z * other.w,
            w: self.w * other.w - self.x * other.x - self.y * other.y - self.z * other.z,
        }
    }

    // Convert quaternion to 4x4 matrix (for 3D transformation with homogeneous coordinates)
    fn to_matrix4(&self) -> [f32; 16] {
        // Normalize quaternion
        let magnitude =
            (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt();
        let x = self.x / magnitude;
        let y = self.y / magnitude;
        let z = self.z / magnitude;
        let w = self.w / magnitude;

        let x2 = x * x;
        let y2 = y * y;
        let z2 = z * z;
        let xy = x * y;
        let xz = x * z;
        let yz = y * z;
        let wx = w * x;
        let wy = w * y;
        let wz = w * z;

        // Row-major 3x3 rotation matrix components
        let m00 = 1.0 - 2.0 * (y2 + z2);
        let m01 = 2.0 * (xy - wz);
        let m02 = 2.0 * (xz + wy);

        let m10 = 2.0 * (xy + wz);
        let m11 = 1.0 - 2.0 * (x2 + z2);
        let m12 = 2.0 * (yz - wx);

        let m20 = 2.0 * (xz - wy);
        let m21 = 2.0 * (yz + wx);
        let m22 = 1.0 - 2.0 * (x2 + y2);

        // Convert 3x3 rotation matrix to 4x4 transformation matrix
        // Note: This is column-major for WGPU
        [
            m00, m10, m20, 0.0, m01, m11, m21, 0.0, m02, m12, m22, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }
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

impl DaveAvatar {
    pub fn new(wgpu_render_state: &egui_wgpu::RenderState) -> Self {
        let device = &wgpu_render_state.device;

        // Create shader module with improved shader code
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube_shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
struct Uniforms {
    model_view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Define cube vertices (-0.5 to 0.5 in each dimension)
    var positions = array<vec3<f32>, 8>(
        vec3<f32>(-0.5, -0.5, -0.5),  // 0: left bottom back
        vec3<f32>(0.5, -0.5, -0.5),   // 1: right bottom back
        vec3<f32>(-0.5, 0.5, -0.5),   // 2: left top back
        vec3<f32>(0.5, 0.5, -0.5),    // 3: right top back
        vec3<f32>(-0.5, -0.5, 0.5),   // 4: left bottom front
        vec3<f32>(0.5, -0.5, 0.5),    // 5: right bottom front
        vec3<f32>(-0.5, 0.5, 0.5),    // 6: left top front
        vec3<f32>(0.5, 0.5, 0.5)      // 7: right top front
    );
    
    // Define indices for the 12 triangles (6 faces * 2 triangles)
    var indices = array<u32, 36>(
        // back face (Z-)
        0, 2, 1, 1, 2, 3,
        // front face (Z+)
        4, 5, 6, 5, 7, 6,
        // left face (X-)
        0, 4, 2, 2, 4, 6,
        // right face (X+)
        1, 3, 5, 3, 7, 5,
        // bottom face (Y-)
        0, 1, 4, 1, 5, 4,
        // top face (Y+)
        2, 6, 3, 3, 6, 7
    );
    
    // Define colors for each face
    var face_colors = array<vec4<f32>, 6>(
        vec4<f32>(1.0, 0.0, 0.0, 1.0),  // back: red
        vec4<f32>(0.0, 1.0, 0.0, 1.0),  // front: green
        vec4<f32>(0.0, 0.0, 1.0, 1.0),  // left: blue
        vec4<f32>(1.0, 1.0, 0.0, 1.0),  // right: yellow
        vec4<f32>(1.0, 0.0, 1.0, 1.0),  // bottom: magenta
        vec4<f32>(0.0, 1.0, 1.0, 1.0)   // top: cyan
    );

    var output: VertexOutput;
    
    // Get vertex from indices
    let index = indices[vertex_index];
    let position = positions[index];
    
    // Determine which face this vertex belongs to
    let face_index = vertex_index / 6u;
    
    // Apply model-view-projection matrix
    output.position = uniforms.model_view_proj * vec4<f32>(position, 1.0);
    
    // Set color based on face
    output.color = face_colors[face_index];
    
    return output;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#
                .into(),
            ),
        });

        // Create uniform buffer for MVP matrix
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cube_uniform_buffer"),
            size: 64, // 4x4 matrix of f32 (16 * 4 bytes)
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube_bind_group_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(64),
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

        // Create render pipeline
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[], // No vertex buffer - vertices are in the shader
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
            });

        Self {
            rotation: Quaternion::identity(),
        }
    }
}

impl DaveAvatar {
    pub fn render(&mut self, rect: Rect, ui: &mut egui::Ui) -> Response {
        let response = ui.allocate_rect(rect, egui::Sense::drag());

        // Update rotation based on drag or animation
        if response.dragged() {
            // Create rotation quaternions based on drag
            let x_rotation =
                Quaternion::from_axis_angle([1.0, 0.0, 0.0], response.drag_delta().y * 0.01);
            let y_rotation =
                Quaternion::from_axis_angle([0.0, 1.0, 0.0], response.drag_delta().x * 0.01);

            // Apply rotations (order matters)
            self.rotation = y_rotation.multiply(&x_rotation).multiply(&self.rotation);
        } else {
            // Continuous rotation - reduced speed and simplified axis
            let continuous_rotation = Quaternion::from_axis_angle([0.0, 1.0, 0.0], 0.005);
            self.rotation = continuous_rotation.multiply(&self.rotation);
        }

        // Create model matrix from rotation quaternion
        let model_matrix = self.rotation.to_matrix4();

        // Create projection matrix with proper depth range
        // Adjust aspect ratio based on rect dimensions
        let aspect = rect.width() / rect.height();
        let projection = perspective_matrix(std::f32::consts::PI / 4.0, aspect, 0.1, 100.0);

        // Create view matrix (move camera back a bit)
        let view_matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -3.0, 1.0,
        ];

        // Combine matrices: projection * view * model
        let mv_matrix = matrix_multiply(&view_matrix, &model_matrix);
        let mvp_matrix = matrix_multiply(&projection, &mv_matrix);

        // Request continuous rendering
        ui.ctx().request_repaint();

        // Add paint callback
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            CubeCallback { mvp_matrix },
        ));

        response
    }
}

// Callback implementation
struct CubeCallback {
    mvp_matrix: [f32; 16], // Model-View-Projection matrix
}

impl egui_wgpu::CallbackTrait for CubeCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let resources: &CubeRenderResources = resources.get().unwrap();

        // Update uniform buffer with MVP matrix
        queue.write_buffer(
            &resources.uniform_buffer,
            0,
            bytemuck::cast_slice(&self.mvp_matrix),
        );

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
        render_pass.draw(0..36, 0..1); // 36 vertices for a cube (6 faces * 2 triangles * 3 vertices)
    }
}

// Simple resources struct
struct CubeRenderResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
}
