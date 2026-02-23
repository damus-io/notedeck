use crate::GpuData;
use crate::texture::{make_1x1_rgba8, texture_layout_entry};
use glam::Vec4;

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    pub base_color_factor: glam::Vec4, // rgba
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub ao_strength: f32,
    pub _pad0: f32,
}

pub struct MaterialGpu {
    pub uniform: MaterialUniform,
    pub buffer: wgpu::Buffer,
    pub bindgroup: wgpu::BindGroup,
}

pub fn make_material_gpudata(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (GpuData<MaterialUniform>, wgpu::BindGroupLayout) {
    let material_uniform = MaterialUniform {
        base_color_factor: Vec4::new(1.0, 0.1, 0.1, 1.0),
        metallic_factor: 1.0,
        roughness_factor: 0.1,
        ao_strength: 1.0,
        _pad0: 0.0,
    };

    let material_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material"),
        size: std::mem::size_of::<MaterialUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("material_sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // Default textures
    let basecolor_view = make_1x1_rgba8(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        [255, 255, 255, 255],
        "basecolor_1x1",
    );

    let mr_view = make_1x1_rgba8(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
        [0, 255, 0, 255], // G=roughness=1, B=metallic=0 (A unused)
        "mr_1x1",
    );

    let normal_view = make_1x1_rgba8(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
        [128, 128, 255, 255],
        "normal_1x1",
    );

    let material_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("material_bgl"),
        entries: &[
            // uniform
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // sampler
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // baseColor (sRGB)
            texture_layout_entry(2),
            // metallicRougness (linear)
            texture_layout_entry(3),
            // normal (linear)
            texture_layout_entry(4),
        ],
    });

    let material_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material_bg"),
        layout: &material_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: material_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&material_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&basecolor_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&mr_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&normal_view),
            },
        ],
    });

    (
        GpuData::<MaterialUniform> {
            data: material_uniform,
            buffer: material_buf,
            bindgroup: material_bg,
        },
        material_bgl,
    )
}
