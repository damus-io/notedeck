use std::num::NonZeroU64;

use crate::vec3::Vec3;
use eframe::egui_wgpu::{self, wgpu};
use egui::{Rect, Response};
use rand::Rng;

pub struct DaveAvatar {
    rotation: Quaternion,
    rot_dir: Vec3,
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
    fn from_axis_angle(axis: &Vec3, angle: f32) -> Self {
        let half_angle = angle * 0.5;
        let s = half_angle.sin();
        Self {
            x: axis.x * s,
            y: axis.y * s,
            z: axis.z * s,
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
    model: mat4x4<f32>,    // Added model matrix for correct normal transformation
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
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
    
    // Define normals for each face
    var face_normals = array<vec3<f32>, 6>(
        vec3<f32>(0.0, 0.0, -1.0),  // back face (Z-)
        vec3<f32>(0.0, 0.0, 1.0),   // front face (Z+)
        vec3<f32>(-1.0, 0.0, 0.0),  // left face (X-)
        vec3<f32>(1.0, 0.0, 0.0),   // right face (X+)
        vec3<f32>(0.0, -1.0, 0.0),  // bottom face (Y-)
        vec3<f32>(0.0, 1.0, 0.0)    // top face (Y+)
    );

    var output: VertexOutput;
    
    // Get vertex from indices
    let index = indices[vertex_index];
    let position = positions[index];
    
    // Determine which face this vertex belongs to
    let face_index = vertex_index / 6u;
    
    // Apply transformations
    output.position = uniforms.model_view_proj * vec4<f32>(position, 1.0);
    
    // Transform normal to world space
    // Extract the 3x3 rotation part from the 4x4 model matrix
    let normal_matrix = mat3x3<f32>(
        uniforms.model[0].xyz,
        uniforms.model[1].xyz,
        uniforms.model[2].xyz
    );
    output.normal = normalize(normal_matrix * face_normals[face_index]);
    
    // Pass world position for lighting calculations
    output.world_pos = (uniforms.model * vec4<f32>(position, 1.0)).xyz;
    
    return output;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Material properties
    let material_color = vec3<f32>(1.0, 1.0, 1.0);  // White color
    let ambient_strength = 0.2;
    let diffuse_strength = 0.7;
    let specular_strength = 0.2;
    let shininess = 20.0;
    
    // Light properties
    let light_pos = vec3<f32>(2.0, 2.0, 2.0);  // Light positioned diagonally above and to the right
    let light_color = vec3<f32>(1.0, 1.0, 1.0); // White light
    
    // View position (camera)
    let view_pos = vec3<f32>(0.0, 0.0, 3.0);   // Camera position
    
    // Calculate ambient lighting
    let ambient = ambient_strength * light_color;
    
    // Calculate diffuse lighting
    let normal = normalize(in.normal);  // Renormalize the interpolated normal
    let light_dir = normalize(light_pos - in.world_pos);
    let diff = max(dot(normal, light_dir), 0.0);
    let diffuse = diffuse_strength * diff * light_color;
    
    // Calculate specular lighting
    let view_dir = normalize(view_pos - in.world_pos);
    let reflect_dir = reflect(-light_dir, normal);
    let spec = pow(max(dot(view_dir, reflect_dir), 0.0), shininess);
    let specular = specular_strength * spec * light_color;
    
    // Combine lighting components
    let result = (ambient + diffuse + specular) * material_color;
    
    return vec4<f32>(result, 1.0);
}
"#
                .into(),
            ),
        });

        // Create uniform buffer for MVP matrix and model matrix
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cube_uniform_buffer"),
            size: 128, // Two 4x4 matrices of f32 (2 * 16 * 4 bytes)
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
                    min_binding_size: NonZeroU64::new(128),
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
        let mut rng = rand::rng();

        let nudge = Vec3::new(
            rng.random::<f32>(),
            rng.random::<f32>(),
            rng.random::<f32>(),
        )
        .normalize();

        self.rot_dir.x += nudge.x;
        self.rot_dir.y += nudge.y;
        self.rot_dir.z += nudge.z;
    }

    pub fn render(&mut self, rect: Rect, ui: &mut egui::Ui) -> Response {
        let response = ui.allocate_rect(rect, egui::Sense::drag());

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
                    Quaternion::from_axis_angle(&Vec3::new(1.0, 0.0, 0.0), self.rot_dir.y * 0.01);
                let y_rotation =
                    Quaternion::from_axis_angle(&Vec3::new(0.0, 1.0, 0.0), self.rot_dir.x * 0.01);
                let z_rotation =
                    Quaternion::from_axis_angle(&Vec3::new(0.0, 0.0, 1.0), self.rot_dir.z * 0.01);

                self.rotation = y_rotation
                    .multiply(&x_rotation)
                    .multiply(&z_rotation)
                    .multiply(&self.rotation);

                ui.ctx().request_repaint();
            }
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

        // Add paint callback
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            CubeCallback {
                mvp_matrix,
                model_matrix,
            },
        ));

        response
    }
}

// Callback implementation
struct CubeCallback {
    mvp_matrix: [f32; 16],   // Model-View-Projection matrix
    model_matrix: [f32; 16], // Model matrix for lighting calculations
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

        // Create a combined uniform buffer with both matrices
        let mut uniform_data = [0.0f32; 32]; // Space for two 4x4 matrices

        // Copy MVP matrix to first 16 floats
        uniform_data[0..16].copy_from_slice(&self.mvp_matrix);

        // Copy model matrix to next 16 floats
        uniform_data[16..32].copy_from_slice(&self.model_matrix);

        // Update uniform buffer with both matrices
        queue.write_buffer(
            &resources.uniform_buffer,
            0,
            bytemuck::cast_slice(&uniform_data),
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
