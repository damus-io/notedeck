use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::material::{MaterialUniform, make_material_gpudata};
use crate::model::ModelData;
use crate::model::Vertex;
use std::collections::HashMap;
use std::num::NonZeroU64;

mod camera;
mod ibl;
mod material;
mod model;
mod texture;
mod world;

#[cfg(feature = "egui")]
pub mod egui;

pub use camera::{ArcballController, Camera, FlyController, ThirdPersonController};
pub use model::{Aabb, Model};
pub use world::{Node, NodeId, ObjectId, Transform, World};

/// Active camera controller mode.
pub enum CameraMode {
    Fly(camera::FlyController),
    ThirdPerson(camera::ThirdPersonController),
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::NoUninit, bytemuck::Zeroable)]
struct ObjectUniform {
    model: Mat4,
    normal: Mat4, // inverse-transpose(model)
}

impl ObjectUniform {
    fn from_model(model: Mat4) -> Self {
        Self {
            model,
            normal: model.inverse().transpose(),
        }
    }
}

const MAX_SCENE_OBJECTS: usize = 256;

struct DynamicObjectBuffer {
    buffer: wgpu::Buffer,
    bindgroup: wgpu::BindGroup,
    stride: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    // 0..16
    time: f32,
    _pad0: f32,
    resolution: Vec2, // 8 bytes, finishes first 16-byte slot

    // 16..32
    cam_pos: Vec3, // takes 12, but aligned to 16
    _pad3: f32,    // fills the last 4 bytes of this 16-byte slot nicely

    // 32..48
    light_dir: Vec3,
    _pad1: f32,

    // 48..64
    light_color: Vec3,
    _pad2: f32,

    // 64..80
    fill_light_dir: Vec3,
    _pad4: f32,

    // 80..96
    fill_light_color: Vec3,
    _pad5: f32,

    // 96..160
    view_proj: Mat4,

    // 160..224
    inv_view_proj: Mat4,

    // 224..288
    light_view_proj: Mat4,
}

impl Globals {
    fn set_camera(&mut self, w: f32, h: f32, camera: &Camera) {
        self.cam_pos = camera.eye;
        self.view_proj = camera.view_proj(w, h);
        self.inv_view_proj = self.view_proj.inverse();
    }
}

struct GpuData<R> {
    data: R,
    buffer: wgpu::Buffer,
    bindgroup: wgpu::BindGroup,
}

const SHADOW_MAP_SIZE: u32 = 2048;

pub struct Renderer {
    size: (u32, u32),

    /// To propery resize we need a device. Provide a target size so
    /// we can dynamically resize next time get one.
    target_size: (u32, u32),

    model_ids: u64,

    depth_tex: wgpu::Texture,
    depth_view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
    skybox_pipeline: wgpu::RenderPipeline,
    grid_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,

    shadow_view: wgpu::TextureView,
    shadow_globals_bg: wgpu::BindGroup,

    world: World,
    camera_mode: CameraMode,

    globals: GpuData<Globals>,
    object_buf: DynamicObjectBuffer,
    material: GpuData<MaterialUniform>,

    material_bgl: wgpu::BindGroupLayout,

    ibl: ibl::IblData,

    models: HashMap<Model, ModelData>,

    start: std::time::Instant,
}

fn make_globals_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    })
}

fn make_globals_bindgroup(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals_buf: &wgpu::Buffer,
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(shadow_sampler),
            },
        ],
    })
}

fn make_global_gpudata(
    device: &wgpu::Device,
    width: f32,
    height: f32,
    camera: &Camera,
    globals_bgl: &wgpu::BindGroupLayout,
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
) -> GpuData<Globals> {
    let view_proj = camera.view_proj(width, height);
    let globals = Globals {
        time: 0.0,
        _pad0: 0.0,
        resolution: Vec2::new(width, height),
        cam_pos: camera.eye,
        _pad3: 0.0,
        // Key light: warm, from upper right (direction of light rays)
        light_dir: Vec3::new(-0.5, -0.7, -0.3),
        _pad1: 0.0,
        light_color: Vec3::new(1.0, 0.98, 0.92),
        _pad2: 0.0,
        // Fill light: cooler, from lower left (opposite side)
        fill_light_dir: Vec3::new(-0.7, -0.3, -0.5),
        _pad4: 0.0,
        fill_light_color: Vec3::new(0.5, 0.55, 0.6),
        _pad5: 0.0,
        view_proj,
        inv_view_proj: view_proj.inverse(),
        light_view_proj: Mat4::IDENTITY,
    };

    println!("Globals size = {}", std::mem::size_of::<Globals>());

    let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("globals"),
        size: std::mem::size_of::<Globals>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let globals_bg = make_globals_bindgroup(
        device,
        globals_bgl,
        &globals_buf,
        shadow_view,
        shadow_sampler,
    );

    GpuData::<Globals> {
        data: globals,
        buffer: globals_buf,
        bindgroup: globals_bg,
    }
}

fn make_dynamic_object_buffer(
    device: &wgpu::Device,
) -> (DynamicObjectBuffer, wgpu::BindGroupLayout) {
    // Alignment for dynamic uniform buffer offsets (typically 256)
    let align = device.limits().min_uniform_buffer_offset_alignment as u64;
    let obj_size = std::mem::size_of::<ObjectUniform>() as u64;
    let stride = obj_size.div_ceil(align) * align;
    let total_size = stride * MAX_SCENE_OBJECTS as u64;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("object_dynamic"),
        size: total_size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let object_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("object_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(obj_size),
            },
            count: None,
        }],
    });

    let bindgroup = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("object_dynamic_bg"),
        layout: &object_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: NonZeroU64::new(obj_size),
            }),
        }],
    });

    (
        DynamicObjectBuffer {
            buffer,
            bindgroup,
            stride,
        },
        object_bgl,
    )
}

/// Ray-AABB intersection using the slab method.
/// Transforms the ray into the object's local space via the inverse world matrix.
/// Returns the distance along the ray if there's a hit.
fn ray_aabb(origin: Vec3, dir: Vec3, aabb: &Aabb, world: &Mat4) -> Option<f32> {
    let inv = world.inverse();
    let lo = (inv * origin.extend(1.0)).truncate();
    let ld = (inv * dir.extend(0.0)).truncate();
    let t1 = (aabb.min - lo) / ld;
    let t2 = (aabb.max - lo) / ld;
    let tmin = t1.min(t2);
    let tmax = t1.max(t2);
    let enter = tmin.x.max(tmin.y).max(tmin.z);
    let exit = tmax.x.min(tmax.y).min(tmax.z);
    if exit >= enter.max(0.0) {
        Some(enter.max(0.0))
    } else {
        None
    }
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Self {
        let (width, height) = size;

        let eye = Vec3::new(0.0, 16.0, 24.0);
        let target = Vec3::new(0.0, 0.0, 0.0);
        let camera = Camera::new(eye, target);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let (_shadow_tex, shadow_view, shadow_sampler) = create_shadow_map(device);
        let globals_bgl = make_globals_bgl(device);
        let globals = make_global_gpudata(
            device,
            width as f32,
            height as f32,
            &camera,
            &globals_bgl,
            &shadow_view,
            &shadow_sampler,
        );
        let (object_buf, object_bgl) = make_dynamic_object_buffer(device);
        let (material, material_bgl) = make_material_gpudata(device, queue);

        let ibl_bgl = ibl::create_ibl_bind_group_layout(device);
        let ibl = ibl::load_hdr_ibl_from_bytes(
            device,
            queue,
            &ibl_bgl,
            include_bytes!("../assets/venice_sunset_1k.hdr"),
        )
        .expect("failed to load HDR environment map");

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline_layout"),
            bind_group_layouts: &[&globals_bgl, &object_bgl, &material_bgl, &ibl_bgl],
            push_constant_ranges: &[],
        });

        /*
        let pipeline_cache = unsafe {
            device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("pipeline_cache"),
                data: None,
                fallback: true,
            })
        };
        */
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipeline"),
            //cache: Some(&pipeline_cache),
            cache: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
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
        });

        // Skybox pipeline
        let skybox_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skybox_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("skybox.wgsl").into()),
        });

        let skybox_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("skybox_pipeline_layout"),
                bind_group_layouts: &[&globals_bgl, &object_bgl, &material_bgl, &ibl_bgl],
                push_constant_ranges: &[],
            });

        let skybox_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("skybox_pipeline"),
            cache: None,
            layout: Some(&skybox_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &skybox_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("vs_main"),
                buffers: &[], // No vertex buffers - procedural fullscreen triangle
            },
            fragment: Some(wgpu::FragmentState {
                module: &skybox_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // Grid pipeline (infinite ground plane)
        let grid_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grid.wgsl").into()),
        });

        let grid_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grid_pipeline_layout"),
            bind_group_layouts: &[&globals_bgl, &object_bgl, &material_bgl, &ibl_bgl],
            push_constant_ranges: &[],
        });

        let grid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid_pipeline"),
            cache: None,
            layout: Some(&grid_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("vs_main"),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
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
        });

        // Shadow depth pipeline (depth-only, no fragment stage)
        // Uses a separate globals BGL without the shadow texture to avoid
        // the resource conflict (shadow tex as both attachment and binding).
        let shadow_globals_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow_globals_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let shadow_globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow_globals_bg"),
            layout: &shadow_globals_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.buffer.as_entire_binding(),
            }],
        });

        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shadow.wgsl").into()),
        });

        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow_pipeline_layout"),
                bind_group_layouts: &[&shadow_globals_bgl, &object_bgl],
                push_constant_ranges: &[],
            });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow_pipeline"),
            cache: None,
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
            },
            fragment: None, // depth-only pass
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // Outline pipeline (inverted hull, front-face culling)
        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("outline.wgsl").into()),
        });

        let outline_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("outline_pipeline_layout"),
                bind_group_layouts: &[&shadow_globals_bgl, &object_bgl],
                push_constant_ranges: &[],
            });

        let outline_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("outline_pipeline"),
            cache: None,
            layout: Some(&outline_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &outline_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &outline_shader,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
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
        });

        let (depth_tex, depth_view) = create_depth(device, width, height);

        /* TODO: move to example
        let model = load_gltf_model(
            &device,
            &queue,
            &material_bgl,
            "/home/jb55/var/models/ironwood/ironwood.glb",
        )
        .unwrap();
        */

        let model_ids = 0;

        let world = World::new(camera);

        let camera_mode = CameraMode::Fly(camera::FlyController::from_camera(&world.camera));

        Self {
            world,
            camera_mode,
            target_size: size,
            model_ids,
            size,
            pipeline,
            skybox_pipeline,
            grid_pipeline,
            shadow_pipeline,
            outline_pipeline,
            shadow_view,
            shadow_globals_bg,
            globals,
            object_buf,
            material,
            material_bgl,
            ibl,
            models: HashMap::new(),
            depth_tex,
            depth_view,
            start: std::time::Instant::now(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    fn globals_mut(&mut self) -> &mut Globals {
        &mut self.globals.data
    }

    /// Load a glTF model from disk. Returns a handle that can be placed in
    /// the scene with [`place_object`].
    pub fn load_gltf_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Model, gltf::Error> {
        let model_data = crate::model::load_gltf_model(device, queue, &self.material_bgl, path)?;

        self.model_ids += 1;
        let id = Model { id: self.model_ids };

        self.models.insert(id, model_data);

        Ok(id)
    }

    /// Place a loaded model in the scene with the given transform.
    pub fn place_object(&mut self, model: Model, transform: Transform) -> ObjectId {
        self.world.add_object(model, transform)
    }

    /// Place a loaded model as a child of an existing scene node.
    /// The transform is local (relative to the parent).
    pub fn place_object_with_parent(
        &mut self,
        model: Model,
        transform: Transform,
        parent: ObjectId,
    ) -> ObjectId {
        self.world.create_renderable(model, transform, Some(parent))
    }

    /// Set or clear the parent of a scene object.
    /// When parented, the object's transform becomes local to the parent.
    pub fn set_parent(&mut self, id: ObjectId, parent: Option<ObjectId>) -> bool {
        self.world.set_parent(id, parent)
    }

    /// Remove an object from the scene.
    pub fn remove_object(&mut self, id: ObjectId) -> bool {
        self.world.remove_object(id)
    }

    /// Update the transform of a placed object.
    pub fn update_object_transform(&mut self, id: ObjectId, transform: Transform) -> bool {
        self.world.update_transform(id, transform)
    }

    /// Perform a resize if the target size is not the same as size
    pub fn set_target_size(&mut self, size: (u32, u32)) {
        self.target_size = size;
    }

    pub fn resize(&mut self, device: &wgpu::Device) {
        if self.target_size == self.size {
            return;
        }

        let (width, height) = self.target_size;
        let w = width as f32;
        let h = height as f32;

        self.size = self.target_size;

        self.globals.data.resolution = Vec2::new(w, h);
        self.globals.data.set_camera(w, h, &self.world.camera);

        let (depth_tex, depth_view) = create_depth(device, width, height);
        self.depth_tex = depth_tex;
        self.depth_view = depth_view;
    }

    pub fn focus_model(&mut self, model: Model) {
        let Some(md) = self.models.get(&model) else {
            return;
        };

        let (w, h) = self.size;
        let w = w as f32;
        let h = h as f32;

        let aspect = w / h.max(1.0);

        self.world.camera = Camera::fit_to_aabb(
            md.bounds.min,
            md.bounds.max,
            aspect,
            45_f32.to_radians(),
            1.2,
        );

        // Sync controller to new camera position
        self.camera_mode = CameraMode::Fly(camera::FlyController::from_camera(&self.world.camera));

        self.globals.data.set_camera(w, h, &self.world.camera);
    }

    /// Set or clear which object shows a selection outline.
    pub fn set_selected(&mut self, id: Option<ObjectId>) {
        self.world.selected_object = id;
    }

    /// Get the axis-aligned bounding box for a loaded model.
    pub fn model_bounds(&self, model: Model) -> Option<Aabb> {
        self.models.get(&model).map(|md| md.bounds)
    }

    /// Get the cached world matrix for a scene object.
    pub fn world_matrix(&self, id: ObjectId) -> Option<glam::Mat4> {
        self.world.world_matrix(id)
    }

    /// Get the parent of a scene object, if it has one.
    pub fn node_parent(&self, id: ObjectId) -> Option<ObjectId> {
        self.world.node_parent(id)
    }

    /// Convert screen coordinates (relative to viewport) to a world-space ray.
    /// Returns (origin, direction).
    fn screen_to_ray(&self, screen_x: f32, screen_y: f32) -> (Vec3, Vec3) {
        let (w, h) = self.target_size;
        let ndc_x = (screen_x / w as f32) * 2.0 - 1.0;
        let ndc_y = 1.0 - (screen_y / h as f32) * 2.0;
        let vp = self.world.camera.view_proj(w as f32, h as f32);
        let inv_vp = vp.inverse();
        let near4 = inv_vp * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far4 = inv_vp * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near = near4.truncate() / near4.w;
        let far = far4.truncate() / far4.w;
        (near, (far - near).normalize())
    }

    /// Pick the closest scene object at the given screen coordinates.
    /// Coordinates are relative to the viewport (0,0 = top-left).
    pub fn pick(&self, screen_x: f32, screen_y: f32) -> Option<ObjectId> {
        let (origin, dir) = self.screen_to_ray(screen_x, screen_y);
        let mut closest: Option<(ObjectId, f32)> = None;
        for &id in self.world.renderables() {
            let model = match self.world.node_model(id) {
                Some(m) => m,
                None => continue,
            };
            let aabb = match self.model_bounds(model) {
                Some(a) => a,
                None => continue,
            };
            let world = match self.world.world_matrix(id) {
                Some(w) => w,
                None => continue,
            };
            if let Some(t) = ray_aabb(origin, dir, &aabb, &world)
                && closest.is_none_or(|(_, d)| t < d)
            {
                closest = Some((id, t));
            }
        }
        closest.map(|(id, _)| id)
    }

    /// Unproject screen coordinates to a point on a horizontal plane at the given Y height.
    /// Useful for constraining object drag to the ground plane.
    pub fn unproject_to_plane(&self, screen_x: f32, screen_y: f32, plane_y: f32) -> Option<Vec3> {
        let (origin, dir) = self.screen_to_ray(screen_x, screen_y);
        if dir.y.abs() < 1e-6 {
            return None;
        }
        let t = (plane_y - origin.y) / dir.y;
        if t < 0.0 {
            return None;
        }
        Some(origin + dir * t)
    }

    /// Handle mouse drag for camera look/orbit.
    pub fn on_mouse_drag(&mut self, delta_x: f32, delta_y: f32) {
        match &mut self.camera_mode {
            CameraMode::Fly(fly) => fly.on_mouse_look(delta_x, delta_y),
            CameraMode::ThirdPerson(tp) => tp.on_mouse_look(delta_x, delta_y),
        }
    }

    /// Handle scroll for camera speed/zoom.
    pub fn on_scroll(&mut self, delta: f32) {
        match &mut self.camera_mode {
            CameraMode::Fly(fly) => fly.on_scroll(delta),
            CameraMode::ThirdPerson(tp) => tp.on_scroll(delta),
        }
    }

    /// Move the camera or avatar. forward/right/up are signed.
    pub fn process_movement(&mut self, forward: f32, right: f32, up: f32, dt: f32) {
        match &mut self.camera_mode {
            CameraMode::Fly(fly) => fly.process_movement(forward, right, up, dt),
            CameraMode::ThirdPerson(tp) => tp.process_movement(forward, right, up, dt),
        }
    }

    /// Switch to third-person camera mode with avatar at the given position.
    pub fn set_third_person_mode(&mut self, avatar_position: Vec3) {
        let mut tp = camera::ThirdPersonController::from_camera(&self.world.camera);
        tp.avatar_position = avatar_position;
        self.camera_mode = CameraMode::ThirdPerson(tp);
    }

    /// Switch to fly camera mode.
    pub fn set_fly_mode(&mut self) {
        self.camera_mode = CameraMode::Fly(camera::FlyController::from_camera(&self.world.camera));
    }

    /// Get the avatar position (None if not in third-person mode).
    pub fn avatar_position(&self) -> Option<Vec3> {
        match &self.camera_mode {
            CameraMode::ThirdPerson(tp) => Some(tp.avatar_position),
            _ => None,
        }
    }

    /// Get the avatar yaw (None if not in third-person mode).
    pub fn avatar_yaw(&self) -> Option<f32> {
        match &self.camera_mode {
            CameraMode::ThirdPerson(tp) => Some(tp.avatar_yaw),
            _ => None,
        }
    }

    pub fn update(&mut self) {
        self.globals_mut().time = self.start.elapsed().as_secs_f32();

        // Update camera from active controller
        match &self.camera_mode {
            CameraMode::Fly(fly) => fly.update_camera(&mut self.world.camera),
            CameraMode::ThirdPerson(tp) => tp.update_camera(&mut self.world.camera),
        }
        let (w, h) = self.size;
        self.globals
            .data
            .set_camera(w as f32, h as f32, &self.world.camera);

        //let t = self.globals_mut().time * 0.3;
        //self.globals_mut().light_dir = Vec3::new(t_slow.cos() * 0.6, 0.7, t_slow.sin() * 0.6);

        // Recompute dirty world transforms before rendering
        self.world.update_world_transforms();

        // Compute light space matrix for shadow mapping
        let light_dir = self.globals.data.light_dir.normalize();
        let light_pos = -light_dir * 30.0; // Position light 30m back along its direction
        let light_view = Mat4::look_at_rh(light_pos, Vec3::ZERO, Vec3::Y);
        let extent = 15.0; // 30m x 30m ortho frustum
        let light_proj = Mat4::orthographic_rh(-extent, extent, -extent, extent, 0.1, 80.0);
        self.globals.data.light_view_proj = light_proj * light_view;
    }

    pub fn prepare(&self, queue: &wgpu::Queue) {
        write_gpu_data(queue, &self.globals);

        // Write per-object transforms into the dynamic buffer
        for (i, &node_id) in self.world.renderables().iter().enumerate() {
            let node = self.world.get_node(node_id).unwrap();
            let obj_uniform = ObjectUniform::from_model(node.world_matrix());
            let offset = i as u64 * self.object_buf.stride;
            queue.write_buffer(
                &self.object_buf.buffer,
                offset,
                bytemuck::bytes_of(&obj_uniform),
            );
        }
    }

    /// Record the shadow depth pass onto the given command encoder.
    /// Must be called before the main render pass.
    pub fn render_shadow(&self, encoder: &mut wgpu::CommandEncoder) {
        let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow_pass"),
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
        });

        shadow_pass.set_pipeline(&self.shadow_pipeline);
        shadow_pass.set_bind_group(0, &self.shadow_globals_bg, &[]);

        for (i, &node_id) in self.world.renderables().iter().enumerate() {
            let node = self.world.get_node(node_id).unwrap();
            let model_handle = node.model.unwrap();
            let Some(model_data) = self.models.get(&model_handle) else {
                continue;
            };
            let dynamic_offset = (i as u64 * self.object_buf.stride) as u32;
            shadow_pass.set_bind_group(1, &self.object_buf.bindgroup, &[dynamic_offset]);

            for d in &model_data.draws {
                shadow_pass.set_vertex_buffer(0, d.mesh.vert_buf.slice(..));
                shadow_pass.set_index_buffer(d.mesh.ind_buf.slice(..), wgpu::IndexFormat::Uint16);
                shadow_pass.draw_indexed(0..d.mesh.num_indices, 0, 0..1);
            }
        }
    }

    pub fn render(&self, frame: &wgpu::TextureView, encoder: &mut wgpu::CommandEncoder) {
        self.render_shadow(encoder);

        // Main render pass
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rpass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.00,
                        g: 0.00,
                        b: 0.00,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        self.render_pass(&mut rpass);
    }

    pub fn render_pass(&self, rpass: &mut wgpu::RenderPass<'_>) {
        // 1. Draw skybox first (writes depth=1.0)
        rpass.set_pipeline(&self.skybox_pipeline);
        rpass.set_bind_group(0, &self.globals.bindgroup, &[]);
        rpass.set_bind_group(1, &self.object_buf.bindgroup, &[0]); // dynamic offset 0
        rpass.set_bind_group(2, &self.material.bindgroup, &[]); // unused but required by layout
        rpass.set_bind_group(3, &self.ibl.bindgroup, &[]);
        rpass.draw(0..3, 0..1);

        // 2. Draw ground grid (alpha-blended over skybox, writes depth)
        rpass.set_pipeline(&self.grid_pipeline);
        rpass.set_bind_group(0, &self.globals.bindgroup, &[]);
        rpass.set_bind_group(1, &self.object_buf.bindgroup, &[0]);
        rpass.set_bind_group(2, &self.material.bindgroup, &[]);
        rpass.set_bind_group(3, &self.ibl.bindgroup, &[]);
        rpass.draw(0..3, 0..1);

        // 3. Draw all scene objects
        rpass.set_pipeline(&self.pipeline);
        rpass.set_bind_group(0, &self.globals.bindgroup, &[]);
        rpass.set_bind_group(3, &self.ibl.bindgroup, &[]);

        for (i, &node_id) in self.world.renderables().iter().enumerate() {
            let node = self.world.get_node(node_id).unwrap();
            let model_handle = node.model.unwrap();
            let Some(model_data) = self.models.get(&model_handle) else {
                continue;
            };

            let dynamic_offset = (i as u64 * self.object_buf.stride) as u32;
            rpass.set_bind_group(1, &self.object_buf.bindgroup, &[dynamic_offset]);

            for d in &model_data.draws {
                rpass.set_bind_group(2, &model_data.materials[d.material_index].bindgroup, &[]);
                rpass.set_vertex_buffer(0, d.mesh.vert_buf.slice(..));
                rpass.set_index_buffer(d.mesh.ind_buf.slice(..), wgpu::IndexFormat::Uint16);
                rpass.draw_indexed(0..d.mesh.num_indices, 0, 0..1);
            }
        }

        // 4. Draw selection outline for selected object
        if let Some(selected_id) = self.world.selected_object
            && let Some(sel_idx) = self
                .world
                .renderables()
                .iter()
                .position(|&id| id == selected_id)
        {
            let node = self.world.get_node(selected_id).unwrap();
            let model_handle = node.model.unwrap();
            if let Some(model_data) = self.models.get(&model_handle) {
                rpass.set_pipeline(&self.outline_pipeline);
                rpass.set_bind_group(0, &self.shadow_globals_bg, &[]);
                let dynamic_offset = (sel_idx as u64 * self.object_buf.stride) as u32;
                rpass.set_bind_group(1, &self.object_buf.bindgroup, &[dynamic_offset]);

                for d in &model_data.draws {
                    rpass.set_vertex_buffer(0, d.mesh.vert_buf.slice(..));
                    rpass.set_index_buffer(d.mesh.ind_buf.slice(..), wgpu::IndexFormat::Uint16);
                    rpass.draw_indexed(0..d.mesh.num_indices, 0, 0..1);
                }
            }
        }
    }
}

fn write_gpu_data<R: bytemuck::NoUninit>(queue: &wgpu::Queue, state: &GpuData<R>) {
    //state.staging.clear();
    //let mut storage = encase::UniformBuffer::new(&mut state.staging);
    //storage.write(&state.data).unwrap();
    queue.write_buffer(&state.buffer, 0, bytemuck::bytes_of(&state.data));
}

fn create_depth(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    assert!(width < 8192);
    assert!(height < 8192);
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24Plus,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn create_shadow_map(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let size = wgpu::Extent3d {
        width: SHADOW_MAP_SIZE,
        height: SHADOW_MAP_SIZE,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow_map"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("shadow_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        compare: Some(wgpu::CompareFunction::Less),
        ..Default::default()
    });
    (tex, view, sampler)
}
