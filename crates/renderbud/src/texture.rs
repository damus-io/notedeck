pub fn make_1x1_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    rgba: [u8; 4],
    label: &str,
) -> wgpu::TextureView {
    let extent = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        extent,
    );

    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

pub fn texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            view_dimension: wgpu::TextureViewDimension::D2,
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
        },
        count: None,
    }
}

/// Robust texture upload helper (handles row padding)
/// "queue.write_texture is annoying once width*4 isn't 256-aligned. This helper always works"
pub fn upload_rgba8_texture_2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::TextureView {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    assert!(!rgba.is_empty());

    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let bytes_per_pixel = 4usize;
    let unpadded_bytes_per_row = width as usize * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize; // 256

    // CEIL division to next multiple of 256
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    assert!(padded_bytes_per_row >= unpadded_bytes_per_row);
    assert!(padded_bytes_per_row.is_multiple_of(align));

    let mut padded = vec![0u8; padded_bytes_per_row * height as usize];
    for y in 0..height as usize {
        let src = &rgba[y * unpadded_bytes_per_row..(y + 1) * unpadded_bytes_per_row];
        let dst = &mut padded
            [y * padded_bytes_per_row..y * padded_bytes_per_row + unpadded_bytes_per_row];
        dst.copy_from_slice(src);
    }

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &padded,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_bytes_per_row as u32),
            rows_per_image: Some(height),
        },
        extent,
    );

    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
