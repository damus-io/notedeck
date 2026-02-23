use rayon::prelude::*;
use std::path::Path;

pub struct IblData {
    pub irradiance_view: wgpu::TextureView,
    pub prefiltered_view: wgpu::TextureView,
    pub brdf_lut_view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub bindgroup: wgpu::BindGroup,
}

pub fn create_ibl_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ibl_bgl"),
        entries: &[
            // binding 0: irradiance cubemap
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // binding 1: sampler
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // binding 2: pre-filtered environment cubemap (with mipmaps)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // binding 3: BRDF LUT (2D texture)
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
        ],
    })
}

/// Create IBL data with a procedural gradient cubemap for testing.
/// Replace this with a real irradiance map later.
#[allow(dead_code)]
pub fn create_test_ibl(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> IblData {
    let size = 32u32; // small for testing
    let irradiance_view = create_gradient_cubemap(device, queue, size);

    // For test IBL, use the same gradient cubemap for prefiltered (not accurate but works)
    let prefiltered_view = create_test_prefiltered_cubemap(device, queue, 64, 5);

    // Generate BRDF LUT (this is environment-independent)
    let brdf_lut_view = generate_brdf_lut(device, queue, 256);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ibl_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let bindgroup = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ibl_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&irradiance_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&prefiltered_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&brdf_lut_view),
            },
        ],
    });

    IblData {
        irradiance_view,
        prefiltered_view,
        brdf_lut_view,
        sampler,
        bindgroup,
    }
}

/// Creates a simple gradient cubemap for testing IBL pipeline.
/// Sky-ish blue on top, ground-ish brown on bottom, neutral sides.
fn create_gradient_cubemap(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: u32,
) -> wgpu::TextureView {
    let extent = wgpu::Extent3d {
        width: size,
        height: size,
        depth_or_array_layers: 6,
    };

    // Use Rgba16Float for HDR values > 1.0
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("irradiance_cubemap"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // Face order: +X, -X, +Y, -Y, +Z, -Z
    // HDR values - will be tonemapped in shader
    let face_colors: [[f32; 3]; 6] = [
        [0.4, 0.38, 0.35],  // +X (right) - warm neutral
        [0.35, 0.38, 0.4],  // -X (left) - cool neutral
        [0.5, 0.6, 0.8],    // +Y (up/sky) - blue sky
        [0.25, 0.2, 0.15],  // -Y (down/ground) - brown ground
        [0.4, 0.4, 0.4],    // +Z (front) - neutral
        [0.38, 0.38, 0.42], // -Z (back) - slightly cool
    ];

    let bytes_per_pixel = 8usize; // 4 x f16 = 8 bytes
    let unpadded_row = size as usize * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_row = unpadded_row.div_ceil(align) * align;

    for (face_idx, color) in face_colors.iter().enumerate() {
        let mut data = vec![0u8; padded_row * size as usize];

        for y in 0..size {
            for x in 0..size {
                let offset = (y as usize * padded_row) + (x as usize * bytes_per_pixel);
                let r = half::f16::from_f32(color[0]);
                let g = half::f16::from_f32(color[1]);
                let b = half::f16::from_f32(color[2]);
                let a = half::f16::from_f32(1.0);

                data[offset..offset + 2].copy_from_slice(&r.to_le_bytes());
                data[offset + 2..offset + 4].copy_from_slice(&g.to_le_bytes());
                data[offset + 4..offset + 6].copy_from_slice(&b.to_le_bytes());
                data[offset + 6..offset + 8].copy_from_slice(&a.to_le_bytes());
            }
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: face_idx as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }

    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("irradiance_cubemap_view"),
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

/// Creates a simple test prefiltered cubemap with mip levels.
/// Uses solid colors that get darker with higher mip levels (simulating blur).
#[allow(dead_code)]
fn create_test_prefiltered_cubemap(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    face_size: u32,
    mip_count: u32,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test_prefiltered_cubemap"),
        size: wgpu::Extent3d {
            width: face_size,
            height: face_size,
            depth_or_array_layers: 6,
        },
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // Face colors (same as gradient cubemap)
    let face_colors: [[f32; 3]; 6] = [
        [0.4, 0.38, 0.35],
        [0.35, 0.38, 0.4],
        [0.5, 0.6, 0.8],
        [0.25, 0.2, 0.15],
        [0.4, 0.4, 0.4],
        [0.38, 0.38, 0.42],
    ];

    for mip in 0..mip_count {
        let mip_size = face_size >> mip;
        let bytes_per_pixel = 8usize;
        let unpadded_row = mip_size as usize * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded_row = unpadded_row.div_ceil(align) * align;

        for (face_idx, color) in face_colors.iter().enumerate() {
            let mut data = vec![0u8; padded_row * mip_size as usize];

            for y in 0..mip_size {
                for x in 0..mip_size {
                    let offset = (y as usize * padded_row) + (x as usize * bytes_per_pixel);
                    let r = half::f16::from_f32(color[0]);
                    let g = half::f16::from_f32(color[1]);
                    let b = half::f16::from_f32(color[2]);
                    let a = half::f16::from_f32(1.0);

                    data[offset..offset + 2].copy_from_slice(&r.to_le_bytes());
                    data[offset + 2..offset + 4].copy_from_slice(&g.to_le_bytes());
                    data[offset + 4..offset + 6].copy_from_slice(&b.to_le_bytes());
                    data[offset + 6..offset + 8].copy_from_slice(&a.to_le_bytes());
                }
            }

            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: face_idx as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: Some(mip_size),
                },
                wgpu::Extent3d {
                    width: mip_size,
                    height: mip_size,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("test_prefiltered_cubemap_view"),
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

/// Load an HDR environment map from an equirectangular panorama file.
pub fn load_hdr_ibl(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    path: impl AsRef<Path>,
) -> Result<IblData, image::ImageError> {
    let img = image::open(path)?.into_rgb32f();
    load_hdr_ibl_from_image(device, queue, layout, img)
}

/// Load an HDR environment map from raw bytes (e.g. from `include_bytes!`).
pub fn load_hdr_ibl_from_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    bytes: &[u8],
) -> Result<IblData, image::ImageError> {
    let img = image::load_from_memory(bytes)?.into_rgb32f();
    load_hdr_ibl_from_image(device, queue, layout, img)
}

fn load_hdr_ibl_from_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    img: image::Rgb32FImage,
) -> Result<IblData, image::ImageError> {
    let width = img.width();
    let height = img.height();
    let pixels: Vec<_> = img.pixels().cloned().collect();

    // Convolve for diffuse irradiance (CPU-side, relatively slow but correct)
    let irradiance_view = equirect_to_irradiance_cubemap(device, queue, &pixels, width, height, 32);

    // Generate pre-filtered specular cubemap with mip chain
    let prefiltered_view =
        generate_prefiltered_cubemap(device, queue, &pixels, width, height, 128, 5);

    // Generate BRDF integration LUT (environment-independent, could be cached)
    let brdf_lut_view = generate_brdf_lut(device, queue, 256);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ibl_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let bindgroup = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ibl_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&irradiance_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&prefiltered_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&brdf_lut_view),
            },
        ],
    });

    Ok(IblData {
        irradiance_view,
        prefiltered_view,
        brdf_lut_view,
        sampler,
        bindgroup,
    })
}

/// Convert equirectangular panorama to irradiance cubemap (with hemisphere convolution).
fn equirect_to_irradiance_cubemap(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pixels: &[image::Rgb<f32>],
    src_width: u32,
    src_height: u32,
    face_size: u32,
) -> wgpu::TextureView {
    let extent = wgpu::Extent3d {
        width: face_size,
        height: face_size,
        depth_or_array_layers: 6,
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("irradiance_cubemap"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let bytes_per_pixel = 8usize;
    let unpadded_row = face_size as usize * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_row = unpadded_row.div_ceil(align) * align;

    for face in 0..6 {
        // Compute all pixels in parallel
        let pixel_colors: Vec<[f32; 3]> = (0..face_size * face_size)
            .into_par_iter()
            .map(|idx| {
                let x = idx % face_size;
                let y = idx / face_size;
                let u = (x as f32 + 0.5) / face_size as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / face_size as f32 * 2.0 - 1.0;

                let dir = face_uv_to_direction(face, u, v);
                let n = normalize(dir);
                convolve_irradiance(pixels, src_width, src_height, n)
            })
            .collect();

        // Write results to buffer
        let mut data = vec![0u8; padded_row * face_size as usize];
        for (idx, color) in pixel_colors.iter().enumerate() {
            let x = idx as u32 % face_size;
            let y = idx as u32 / face_size;
            let offset = (y as usize * padded_row) + (x as usize * bytes_per_pixel);

            let r = half::f16::from_f32(color[0]);
            let g = half::f16::from_f32(color[1]);
            let b = half::f16::from_f32(color[2]);
            let a = half::f16::from_f32(1.0);

            data[offset..offset + 2].copy_from_slice(&r.to_le_bytes());
            data[offset + 2..offset + 4].copy_from_slice(&g.to_le_bytes());
            data[offset + 4..offset + 6].copy_from_slice(&b.to_le_bytes());
            data[offset + 6..offset + 8].copy_from_slice(&a.to_le_bytes());
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: face,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(face_size),
            },
            wgpu::Extent3d {
                width: face_size,
                height: face_size,
                depth_or_array_layers: 1,
            },
        );
    }

    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("irradiance_cubemap_view"),
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

/// Integrate the environment map over a hemisphere for diffuse irradiance.
/// Uses discrete sampling over the hemisphere.
fn convolve_irradiance(
    pixels: &[image::Rgb<f32>],
    width: u32,
    height: u32,
    normal: [f32; 3],
) -> [f32; 3] {
    let mut irradiance = [0.0f32; 3];

    // Build tangent frame from normal
    let up = if normal[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let tangent = normalize(cross(up, normal));
    let bitangent = cross(normal, tangent);

    // Sample hemisphere with uniform spacing
    let sample_delta = 0.05; // Adjust for quality vs speed
    let mut n_samples = 0u32;

    let mut phi = 0.0f32;
    while phi < 2.0 * std::f32::consts::PI {
        let mut theta = 0.0f32;
        while theta < 0.5 * std::f32::consts::PI {
            // Spherical to cartesian (in tangent space)
            let sin_theta = theta.sin();
            let cos_theta = theta.cos();
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();

            let tangent_sample = [sin_theta * cos_phi, sin_theta * sin_phi, cos_theta];

            // Transform to world space
            let sample_dir = [
                tangent_sample[0] * tangent[0]
                    + tangent_sample[1] * bitangent[0]
                    + tangent_sample[2] * normal[0],
                tangent_sample[0] * tangent[1]
                    + tangent_sample[1] * bitangent[1]
                    + tangent_sample[2] * normal[1],
                tangent_sample[0] * tangent[2]
                    + tangent_sample[1] * bitangent[2]
                    + tangent_sample[2] * normal[2],
            ];

            let color = sample_equirect(pixels, width, height, sample_dir);

            // Weight by cos(theta) * sin(theta) for hemisphere integration
            let weight = cos_theta * sin_theta;
            irradiance[0] += color[0] * weight;
            irradiance[1] += color[1] * weight;
            irradiance[2] += color[2] * weight;
            n_samples += 1;

            theta += sample_delta;
        }
        phi += sample_delta;
    }

    // Normalize
    let scale = std::f32::consts::PI / n_samples as f32;
    [
        irradiance[0] * scale,
        irradiance[1] * scale,
        irradiance[2] * scale,
    ]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Convert face index + UV to 3D direction.
/// Face order: +X, -X, +Y, -Y, +Z, -Z
fn face_uv_to_direction(face: u32, u: f32, v: f32) -> [f32; 3] {
    match face {
        0 => [1.0, -v, -u],  // +X
        1 => [-1.0, -v, u],  // -X
        2 => [u, 1.0, v],    // +Y
        3 => [u, -1.0, -v],  // -Y
        4 => [u, -v, 1.0],   // +Z
        5 => [-u, -v, -1.0], // -Z
        _ => [0.0, 0.0, 1.0],
    }
}

/// Sample equirectangular panorama given a 3D direction.
fn sample_equirect(pixels: &[image::Rgb<f32>], width: u32, height: u32, dir: [f32; 3]) -> [f32; 3] {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let x = dir[0] / len;
    let y = dir[1] / len;
    let z = dir[2] / len;

    // Convert to spherical (theta = azimuth, phi = elevation)
    let theta = z.atan2(x); // -PI to PI
    let phi = y.asin(); // -PI/2 to PI/2

    // Convert to UV
    let u = (theta / std::f32::consts::PI + 1.0) * 0.5; // 0 to 1
    let v = (-phi / std::f32::consts::FRAC_PI_2 + 1.0) * 0.5; // 0 to 1

    let px = ((u * width as f32) as u32).min(width - 1);
    let py = ((v * height as f32) as u32).min(height - 1);

    let idx = (py * width + px) as usize;
    let p = &pixels[idx];
    [p.0[0], p.0[1], p.0[2]]
}

// ============================================================================
// Specular IBL: Pre-filtered environment map and BRDF LUT
// ============================================================================

/// Generate a 2D BRDF integration LUT for split-sum approximation.
/// X axis: NdotV (0..1), Y axis: roughness (0..1)
/// Output: RG16Float with (scale, bias) for Fresnel term
fn generate_brdf_lut(device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("brdf_lut"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let bytes_per_pixel = 4usize; // 2 x f16 = 4 bytes
    let unpadded_row = size as usize * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_row = unpadded_row.div_ceil(align) * align;

    let sample_count = 1024u32;

    // Compute all BRDF values in parallel
    let brdf_values: Vec<(f32, f32)> = (0..size * size)
        .into_par_iter()
        .map(|idx| {
            let x = idx % size;
            let y = idx / size;
            let ndot_v = (x as f32 + 0.5) / size as f32;
            let roughness = (y as f32 + 0.5) / size as f32;
            integrate_brdf(ndot_v.max(0.001), roughness, sample_count)
        })
        .collect();

    // Write results to buffer
    let mut data = vec![0u8; padded_row * size as usize];
    for (idx, (scale, bias)) in brdf_values.iter().enumerate() {
        let x = idx as u32 % size;
        let y = idx as u32 / size;
        let offset = (y as usize * padded_row) + (x as usize * bytes_per_pixel);

        let r = half::f16::from_f32(*scale);
        let g = half::f16::from_f32(*bias);

        data[offset..offset + 2].copy_from_slice(&r.to_le_bytes());
        data[offset + 2..offset + 4].copy_from_slice(&g.to_le_bytes());
    }

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_row as u32),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );

    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Integrate the BRDF over the hemisphere using importance sampling.
/// Returns (scale, bias) for the split-sum: F0 * scale + bias
fn integrate_brdf(ndot_v: f32, roughness: f32, sample_count: u32) -> (f32, f32) {
    // View direction in tangent space (N = [0,0,1])
    let v = [
        (1.0 - ndot_v * ndot_v).sqrt(), // sin(theta)
        0.0,
        ndot_v, // cos(theta)
    ];

    let mut a = 0.0f32;
    let mut b = 0.0f32;

    let alpha = roughness * roughness;

    for i in 0..sample_count {
        // Hammersley sequence for quasi-random sampling
        let (xi_x, xi_y) = hammersley(i, sample_count);

        // Importance sample GGX
        let h = importance_sample_ggx(xi_x, xi_y, alpha);

        // Compute light direction by reflecting view around half vector
        let v_dot_h = dot(v, h).max(0.0);
        let l = [
            2.0 * v_dot_h * h[0] - v[0],
            2.0 * v_dot_h * h[1] - v[1],
            2.0 * v_dot_h * h[2] - v[2],
        ];

        let n_dot_l = l[2].max(0.0); // N = [0,0,1]
        let n_dot_h = h[2].max(0.0);

        if n_dot_l > 0.0 {
            let g = geometry_smith(ndot_v, n_dot_l, roughness);
            let g_vis = (g * v_dot_h) / (n_dot_h * ndot_v).max(0.001);
            let fc = (1.0 - v_dot_h).powf(5.0);

            a += (1.0 - fc) * g_vis;
            b += fc * g_vis;
        }
    }

    let inv_samples = 1.0 / sample_count as f32;
    (a * inv_samples, b * inv_samples)
}

/// Hammersley quasi-random sequence
fn hammersley(i: u32, n: u32) -> (f32, f32) {
    (i as f32 / n as f32, radical_inverse_vdc(i))
}

/// Van der Corput radical inverse
fn radical_inverse_vdc(mut bits: u32) -> f32 {
    bits = (bits << 16) | (bits >> 16);
    bits = ((bits & 0x55555555) << 1) | ((bits & 0xAAAAAAAA) >> 1);
    bits = ((bits & 0x33333333) << 2) | ((bits & 0xCCCCCCCC) >> 2);
    bits = ((bits & 0x0F0F0F0F) << 4) | ((bits & 0xF0F0F0F0) >> 4);
    bits = ((bits & 0x00FF00FF) << 8) | ((bits & 0xFF00FF00) >> 8);
    bits as f32 * 2.3283064365386963e-10 // 0x100000000
}

/// Importance sample the GGX NDF to get a half-vector in tangent space
fn importance_sample_ggx(xi_x: f32, xi_y: f32, alpha: f32) -> [f32; 3] {
    let a2 = alpha * alpha;

    let phi = 2.0 * std::f32::consts::PI * xi_x;
    let cos_theta = ((1.0 - xi_y) / (1.0 + (a2 - 1.0) * xi_y)).sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();

    [sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta]
}

/// Smith geometry function for GGX
fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;

    let g1_v = n_dot_v / (n_dot_v * (1.0 - k) + k);
    let g1_l = n_dot_l / (n_dot_l * (1.0 - k) + k);
    g1_v * g1_l
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Generate pre-filtered environment cubemap with mip levels for different roughness.
fn generate_prefiltered_cubemap(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pixels: &[image::Rgb<f32>],
    src_width: u32,
    src_height: u32,
    face_size: u32,
    mip_count: u32,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("prefiltered_cubemap"),
        size: wgpu::Extent3d {
            width: face_size,
            height: face_size,
            depth_or_array_layers: 6,
        },
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let sample_count = 512u32;

    for mip in 0..mip_count {
        let mip_size = face_size >> mip;
        let roughness = mip as f32 / (mip_count - 1) as f32;

        let bytes_per_pixel = 8usize; // 4 x f16
        let unpadded_row = mip_size as usize * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded_row = unpadded_row.div_ceil(align) * align;

        for face in 0..6u32 {
            // Compute all pixels in parallel
            let pixel_colors: Vec<[f32; 3]> = (0..mip_size * mip_size)
                .into_par_iter()
                .map(|idx| {
                    let x = idx % mip_size;
                    let y = idx / mip_size;
                    let u = (x as f32 + 0.5) / mip_size as f32 * 2.0 - 1.0;
                    let v = (y as f32 + 0.5) / mip_size as f32 * 2.0 - 1.0;

                    let n = normalize(face_uv_to_direction(face, u, v));
                    prefilter_env_map(pixels, src_width, src_height, n, roughness, sample_count)
                })
                .collect();

            // Write results to buffer
            let mut data = vec![0u8; padded_row * mip_size as usize];
            for (idx, color) in pixel_colors.iter().enumerate() {
                let x = idx as u32 % mip_size;
                let y = idx as u32 / mip_size;
                let offset = (y as usize * padded_row) + (x as usize * bytes_per_pixel);

                let r = half::f16::from_f32(color[0]);
                let g = half::f16::from_f32(color[1]);
                let b = half::f16::from_f32(color[2]);
                let a = half::f16::from_f32(1.0);

                data[offset..offset + 2].copy_from_slice(&r.to_le_bytes());
                data[offset + 2..offset + 4].copy_from_slice(&g.to_le_bytes());
                data[offset + 4..offset + 6].copy_from_slice(&b.to_le_bytes());
                data[offset + 6..offset + 8].copy_from_slice(&a.to_le_bytes());
            }

            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: face,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: Some(mip_size),
                },
                wgpu::Extent3d {
                    width: mip_size,
                    height: mip_size,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("prefiltered_cubemap_view"),
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

/// Pre-filter the environment map for a given roughness using GGX importance sampling.
fn prefilter_env_map(
    pixels: &[image::Rgb<f32>],
    src_width: u32,
    src_height: u32,
    n: [f32; 3],
    roughness: f32,
    sample_count: u32,
) -> [f32; 3] {
    // For roughness = 0, just sample the environment directly
    if roughness < 0.001 {
        return sample_equirect(pixels, src_width, src_height, n);
    }

    // Use N = V = R assumption for pre-filtering
    let r = n;
    let v = r;

    let mut prefilt = [0.0f32; 3];
    let mut total_weight = 0.0f32;

    let alpha = roughness * roughness;

    for i in 0..sample_count {
        let (xi_x, xi_y) = hammersley(i, sample_count);
        let h = importance_sample_ggx_world(xi_x, xi_y, n, alpha);

        // Reflect V around H to get L
        let v_dot_h = dot(v, h).max(0.0);
        let l = [
            2.0 * v_dot_h * h[0] - v[0],
            2.0 * v_dot_h * h[1] - v[1],
            2.0 * v_dot_h * h[2] - v[2],
        ];

        let n_dot_l = dot(n, l);
        if n_dot_l > 0.0 {
            let color = sample_equirect(pixels, src_width, src_height, l);
            prefilt[0] += color[0] * n_dot_l;
            prefilt[1] += color[1] * n_dot_l;
            prefilt[2] += color[2] * n_dot_l;
            total_weight += n_dot_l;
        }
    }

    if total_weight > 0.0 {
        let inv = 1.0 / total_weight;
        [prefilt[0] * inv, prefilt[1] * inv, prefilt[2] * inv]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Importance sample GGX and return half-vector in world space.
fn importance_sample_ggx_world(xi_x: f32, xi_y: f32, n: [f32; 3], alpha: f32) -> [f32; 3] {
    // Sample in tangent space
    let h_tangent = importance_sample_ggx(xi_x, xi_y, alpha);

    // Build tangent frame
    let up = if n[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let tangent = normalize(cross(up, n));
    let bitangent = cross(n, tangent);

    // Transform to world space
    normalize([
        h_tangent[0] * tangent[0] + h_tangent[1] * bitangent[0] + h_tangent[2] * n[0],
        h_tangent[0] * tangent[1] + h_tangent[1] * bitangent[1] + h_tangent[2] * n[1],
        h_tangent[0] * tangent[2] + h_tangent[1] * bitangent[2] + h_tangent[2] * n[2],
    ])
}
