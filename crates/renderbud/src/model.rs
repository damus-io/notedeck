use glam::{Vec3, Vec4};

use crate::material::{MaterialGpu, MaterialUniform};
use crate::texture::upload_rgba8_texture_2d;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
}

pub struct Mesh {
    pub num_indices: u32,
    pub vert_buf: wgpu::Buffer,
    pub ind_buf: wgpu::Buffer,
}

pub struct ModelDraw {
    pub mesh: Mesh,
    pub material_index: usize,
}

pub struct ModelData {
    pub draws: Vec<ModelDraw>,
    pub materials: Vec<MaterialGpu>,
    pub bounds: Aabb,
}

/// A model handle
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Copy, Clone)]
pub struct Model {
    pub id: u64,
}

struct GltfWgpuCache {
    samplers: Vec<Option<wgpu::Sampler>>,
    tex_views: HashMap<(usize, bool), wgpu::TextureView>,
}

impl GltfWgpuCache {
    pub fn new(doc: &gltf::Document) -> Self {
        Self {
            samplers: (0..doc.samplers().len()).map(|_| None).collect(),
            tex_views: HashMap::new(),
        }
    }

    fn ensure_sampler(
        &mut self,
        device: &wgpu::Device,
        sam: gltf::texture::Sampler<'_>,
    ) -> Option<usize> {
        let idx = sam.index()?;
        if self.samplers[idx].is_none() {
            let (min_f, mip_f) = map_min_filter(sam.min_filter());
            let samp = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("gltf_sampler"),
                address_mode_u: map_wrap_mode(sam.wrap_s()),
                address_mode_v: map_wrap_mode(sam.wrap_t()),
                address_mode_w: wgpu::AddressMode::Repeat,
                mag_filter: map_mag_filter(sam.mag_filter()),
                min_filter: min_f,
                mipmap_filter: mip_f,
                ..Default::default()
            });
            self.samplers[idx] = Some(samp);
        }
        Some(idx)
    }

    fn sampler_ref(&self, idx: usize) -> &wgpu::Sampler {
        self.samplers[idx].as_ref().unwrap()
    }

    fn ensure_texture_view(
        &mut self,
        images: &[gltf::image::Data],
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: gltf::Texture<'_>,
        srgb: bool,
    ) -> (usize, bool) {
        let key = (tex.index(), srgb);
        self.tex_views.entry(key).or_insert_with(|| {
            let img = &images[tex.source().index()];
            let rgba8 = build_rgba(img);

            let format = if srgb {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            };

            upload_rgba8_texture_2d(
                device, queue, img.width, img.height, &rgba8, format, "gltf_tex",
            )
        });
        key
    }

    fn view_ref(&self, key: (usize, bool)) -> &wgpu::TextureView {
        self.tex_views.get(&key).unwrap()
    }
}

impl Vertex {
    pub fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                // position
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // normal
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as u64,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // uv
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() + mem::size_of::<[f32; 3]>()) as u64,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // tangent
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()) as u64, // 12+12+8 = 32
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

fn build_rgba(img: &gltf::image::Data) -> Vec<u8> {
    match img.format {
        gltf::image::Format::R8 => img.pixels.iter().flat_map(|&r| [r, r, r, 255]).collect(),
        gltf::image::Format::R8G8B8 => img
            .pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        gltf::image::Format::R8G8B8A8 => img.pixels.clone(),
        gltf::image::Format::R16 => {
            // super rare for your target; quick & dirty downconvert
            img.pixels
                .chunks_exact(2)
                .flat_map(|p| {
                    let r = p[0];
                    [r, r, r, 255]
                })
                .collect()
        }
        gltf::image::Format::R16G16B16 => img
            .pixels
            .chunks_exact(6)
            .flat_map(|p| {
                let r = p[0];
                let g = p[2];
                let b = p[4];
                [r, g, b, 255]
            })
            .collect(),
        gltf::image::Format::R16G16B16A16 => img
            .pixels
            .chunks_exact(8)
            .flat_map(|p| {
                let r = p[0];
                let g = p[2];
                let b = p[4];
                let a = p[6];
                [r, g, b, a]
            })
            .collect(),
        _ => panic!("Unhandled image format {:?}", img.format),
    }
}

pub fn load_gltf_model(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    material_bgl: &wgpu::BindGroupLayout,
    path: impl AsRef<std::path::Path>,
) -> Result<ModelData, gltf::Error> {
    let path = path.as_ref();

    let (doc, buffers, images) = gltf::import(path)?;

    // --- default textures
    let default_sampler = make_default_sampler(device);
    let default_basecolor = upload_rgba8_texture_2d(
        device,
        queue,
        1,
        1,
        &[255, 255, 255, 255],
        wgpu::TextureFormat::Rgba8UnormSrgb,
        "basecolor_1x1",
    );
    let default_mr = upload_rgba8_texture_2d(
        device,
        queue,
        1,
        1,
        &[0, 255, 0, 255],
        wgpu::TextureFormat::Rgba8Unorm,
        "mr_1x1",
    );
    let default_normal = upload_rgba8_texture_2d(
        device,
        queue,
        1,
        1,
        &[128, 128, 255, 255],
        wgpu::TextureFormat::Rgba8Unorm,
        "normal_1x1",
    );

    let mut cache = GltfWgpuCache::new(&doc);

    let mut materials: Vec<MaterialGpu> = Vec::new();

    for mat in doc.materials() {
        let pbr = mat.pbr_metallic_roughness();
        let bc_factor = pbr.base_color_factor();
        let metallic_factor = pbr.metallic_factor();
        let roughness_factor = pbr.roughness_factor();
        let ao_strength = mat.occlusion_texture().map(|o| o.strength()).unwrap_or(1.0);

        let mut chosen_sampler_idx: Option<usize> = None;

        let basecolor_key = pbr.base_color_texture().map(|info| {
            let s_idx = cache.ensure_sampler(device, info.texture().sampler());
            if chosen_sampler_idx.is_none() {
                chosen_sampler_idx = s_idx;
            }
            cache.ensure_texture_view(&images, device, queue, info.texture(), true)
        });

        let mr_key = pbr.metallic_roughness_texture().map(|info| {
            let s_idx = cache.ensure_sampler(device, info.texture().sampler());
            if chosen_sampler_idx.is_none() {
                chosen_sampler_idx = s_idx;
            }
            cache.ensure_texture_view(&images, device, queue, info.texture(), false)
        });

        let normal_key = mat.normal_texture().map(|norm_tex| {
            let s_idx = cache.ensure_sampler(device, norm_tex.texture().sampler());
            if chosen_sampler_idx.is_none() {
                chosen_sampler_idx = s_idx;
            }
            cache.ensure_texture_view(&images, device, queue, norm_tex.texture(), false)
        });

        let uniform = MaterialUniform {
            base_color_factor: Vec4::new(bc_factor[0], bc_factor[1], bc_factor[2], bc_factor[3]),
            metallic_factor,
            roughness_factor,
            ao_strength,
            _pad0: 0.0,
        };

        let chosen_sampler: &wgpu::Sampler = chosen_sampler_idx
            .map(|i| cache.sampler_ref(i))
            .unwrap_or(&default_sampler);

        let normal_view: &wgpu::TextureView = normal_key
            .map(|k| cache.view_ref(k))
            .unwrap_or(&default_normal);

        let basecolor_view: &wgpu::TextureView = basecolor_key
            .map(|k| cache.view_ref(k))
            .unwrap_or(&default_basecolor);

        let mr_view: &wgpu::TextureView = mr_key.map(|k| cache.view_ref(k)).unwrap_or(&default_mr);

        materials.push(make_material_gpu(
            device,
            queue,
            material_bgl,
            chosen_sampler,
            basecolor_view,
            mr_view,
            normal_view,
            uniform,
        ));
    }

    let default_material_index = materials.len();
    materials.push(make_material_gpu(
        device,
        queue,
        material_bgl,
        &default_sampler,
        &default_basecolor,
        &default_mr,
        &default_normal,
        MaterialUniform {
            base_color_factor: Vec4::ONE,
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            ao_strength: 1.0,
            _pad0: 0.0,
        },
    ));

    let mut draws: Vec<ModelDraw> = Vec::new();
    let mut bounds = Aabb::empty();

    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }

            let reader = prim.reader(|b| Some(&buffers[b.index()]));

            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(it) => it.collect(),
                None => continue,
            };

            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|it| it.collect())
                .unwrap_or_else(|| vec![[0.0, 0.0, 1.0]; positions.len()]);

            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|tc| tc.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            // TODO(jb55): switch to u32 indices
            let indices: Vec<u16> = if let Some(read) = reader.read_indices() {
                read.into_u32().map(|i| i as u16).collect()
            } else {
                (0..positions.len() as u16).collect()
            };

            /*
            let tangents: Vec<[f32; 4]> = reader
                .read_tangents()
                .map(|it| it.collect())
                .unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 1.0]; positions.len()]);
                */

            let mut verts: Vec<Vertex> = Vec::with_capacity(positions.len());
            for i in 0..positions.len() {
                let pos = positions[i];
                bounds.include_point(Vec3::new(pos[0], pos[1], pos[2]));

                verts.push(Vertex {
                    pos: pos,
                    normal: normals[i],
                    uv: uvs[i],
                    tangent: [0.0, 0.0, 0.0, 0.0],
                })
            }

            compute_tangents(&mut verts, &indices);

            let vert_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf_vert_buf"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });

            let ind_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf_ind_buf"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

            let material_index = prim.material().index().unwrap_or(default_material_index);

            draws.push(ModelDraw {
                mesh: Mesh {
                    num_indices: indices.len() as u32,
                    vert_buf,
                    ind_buf,
                },
                material_index,
            });
        }
    }

    Ok(ModelData {
        draws,
        materials,
        bounds,
    })
}

fn make_default_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gltf_default_sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    })
}

/// Keep wrap modes consistent when mapping glTF sampler -> wgpu sampler
fn map_wrap_mode(wrap_mode: gltf::texture::WrappingMode) -> wgpu::AddressMode {
    match wrap_mode {
        gltf::texture::WrappingMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        gltf::texture::WrappingMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        gltf::texture::WrappingMode::Repeat => wgpu::AddressMode::Repeat,
    }
}

fn map_min_filter(f: Option<gltf::texture::MinFilter>) -> (wgpu::FilterMode, wgpu::FilterMode) {
    // (min, mipmap)
    match f {
        Some(gltf::texture::MinFilter::Nearest) => {
            (wgpu::FilterMode::Nearest, wgpu::FilterMode::Nearest)
        }
        Some(gltf::texture::MinFilter::Linear) => {
            (wgpu::FilterMode::Linear, wgpu::FilterMode::Nearest)
        }

        Some(gltf::texture::MinFilter::NearestMipmapNearest) => {
            (wgpu::FilterMode::Nearest, wgpu::FilterMode::Nearest)
        }
        Some(gltf::texture::MinFilter::LinearMipmapNearest) => {
            (wgpu::FilterMode::Linear, wgpu::FilterMode::Nearest)
        }
        Some(gltf::texture::MinFilter::NearestMipmapLinear) => {
            (wgpu::FilterMode::Nearest, wgpu::FilterMode::Linear)
        }
        Some(gltf::texture::MinFilter::LinearMipmapLinear) => {
            (wgpu::FilterMode::Linear, wgpu::FilterMode::Linear)
        }

        None => (wgpu::FilterMode::Linear, wgpu::FilterMode::Nearest),
    }
}

fn map_mag_filter(f: Option<gltf::texture::MagFilter>) -> wgpu::FilterMode {
    match f {
        Some(gltf::texture::MagFilter::Nearest) => wgpu::FilterMode::Nearest,
        Some(gltf::texture::MagFilter::Linear) => wgpu::FilterMode::Linear,
        None => wgpu::FilterMode::Linear,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_material_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    material_bgl: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    basecolor: &wgpu::TextureView,
    mr: &wgpu::TextureView,
    normal: &wgpu::TextureView,
    uniform: MaterialUniform,
) -> MaterialGpu {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material_ubo"),
        size: std::mem::size_of::<MaterialUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    // write uniform once
    queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniform));

    let bindgroup = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material_bg"),
        layout: material_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(basecolor),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(mr),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(normal),
            },
        ],
    });

    MaterialGpu {
        uniform,
        buffer,
        bindgroup,
    }
}

fn compute_tangents(verts: &mut [Vertex], indices: &[u16]) {
    use glam::{Vec2, Vec3};

    let n = verts.len();
    let mut tan1 = vec![Vec3::ZERO; n];
    let mut tan2 = vec![Vec3::ZERO; n];

    let to_v3 = |a: [f32; 3]| Vec3::new(a[0], a[1], a[2]);
    let to_v2 = |a: [f32; 2]| Vec2::new(a[0], a[1]);

    // Accumulate per-triangle tangents/bitangents
    for tri in indices.chunks_exact(3) {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;

        let p0 = to_v3(verts[i0].pos);
        let p1 = to_v3(verts[i1].pos);
        let p2 = to_v3(verts[i2].pos);

        let w0 = to_v2(verts[i0].uv);
        let w1 = to_v2(verts[i1].uv);
        let w2 = to_v2(verts[i2].uv);

        let e1 = p1 - p0;
        let e2 = p2 - p0;

        let d1 = w1 - w0;
        let d2 = w2 - w0;

        let denom = d1.x * d2.y - d1.y * d2.x;
        if denom.abs() < 1e-8 {
            continue; // degenerate UV mapping; skip
        }
        let r = 1.0 / denom;

        let sdir = (e1 * d2.y - e2 * d1.y) * r; // tangent direction
        let tdir = (e2 * d1.x - e1 * d2.x) * r; // bitangent direction

        tan1[i0] += sdir;
        tan1[i1] += sdir;
        tan1[i2] += sdir;
        tan2[i0] += tdir;
        tan2[i1] += tdir;
        tan2[i2] += tdir;
    }

    // Orthonormalize & store handedness in w
    for i in 0..n {
        let nrm = to_v3(verts[i].normal).normalize_or_zero();
        let t = tan1[i];

        // Gram–Schmidt: make T perpendicular to N
        let t_ortho = (t - nrm * nrm.dot(t)).normalize_or_zero();

        // Handedness: +1 or -1
        let w = if nrm.cross(t_ortho).dot(tan2[i]) < 0.0 {
            -1.0
        } else {
            1.0
        };

        verts[i].tangent = [t_ortho.x, t_ortho.y, t_ortho.z, w];
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn empty() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    pub fn include_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    pub fn radius(&self) -> f32 {
        self.half_extents().length()
    }
}
