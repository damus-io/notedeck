//! Tilemap mesh generation — builds a single textured quad mesh from TilemapData.

use crate::room_state::TilemapData;
use egui_wgpu::wgpu;
use glam::Vec3;
use renderbud::{Aabb, MaterialUniform, Mesh, ModelData, ModelDraw, Vertex};
use wgpu::util::DeviceExt;

/// Size of each tile in the atlas, in pixels.
const TILE_PX: u32 = 32;

/// Generate a deterministic color for a tile name.
fn tile_color(name: &str) -> [u8; 4] {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    // Clamp channels to a pleasant range (64..224) so tiles aren't too dark or bright
    let r = 64 + (h & 0xFF) as u8 % 160;
    let g = 64 + ((h >> 8) & 0xFF) as u8 % 160;
    let b = 64 + ((h >> 16) & 0xFF) as u8 % 160;
    [r, g, b, 255]
}

/// Build the atlas RGBA texture data.
/// Atlas is a 1-tile-wide vertical strip (TILE_PX x (TILE_PX * N)).
fn build_atlas(tileset: &[String]) -> (u32, u32, Vec<u8>) {
    let n = tileset.len().max(1) as u32;
    let atlas_w = TILE_PX;
    let atlas_h = TILE_PX * n;
    let mut rgba = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (i, name) in tileset.iter().enumerate() {
        let color = tile_color(name);
        let y_start = i as u32 * TILE_PX;
        for y in y_start..y_start + TILE_PX {
            for x in 0..TILE_PX {
                let offset = ((y * atlas_w + x) * 4) as usize;
                rgba[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }

    (atlas_w, atlas_h, rgba)
}

/// Build a tilemap model (mesh + atlas material) and register it in the renderer.
pub fn build_tilemap_model(
    tm: &TilemapData,
    renderer: &mut renderbud::Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> renderbud::Model {
    let w = tm.width;
    let h = tm.height;
    let n_tiles = (w * h) as usize;
    let n_tileset = tm.tileset.len().max(1) as f32;

    // Build atlas texture
    let (atlas_w, atlas_h, atlas_rgba) = build_atlas(&tm.tileset);
    let atlas_view = renderbud::upload_rgba8_texture_2d(
        device,
        queue,
        atlas_w,
        atlas_h,
        &atlas_rgba,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        "tilemap_atlas",
    );

    // Build mesh: one quad per tile
    let mut verts: Vec<Vertex> = Vec::with_capacity(n_tiles * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(n_tiles * 6);
    let mut bounds = Aabb::empty();

    // Center the tilemap so origin is in the middle
    let offset_x = -(w as f32) / 2.0;
    let offset_z = -(h as f32) / 2.0;
    let y = 0.001_f32; // Just above ground to avoid z-fighting with grid

    let normal = [0.0_f32, 1.0, 0.0]; // Facing up
    let tangent = [1.0_f32, 0.0, 0.0, 1.0]; // Tangent along +X

    for ty in 0..h {
        for tx in 0..w {
            let tile_idx = tm.tile_at(tx, ty) as f32;
            let base_vert = verts.len() as u32;

            // Quad corners in world space
            let x0 = offset_x + tx as f32;
            let x1 = x0 + 1.0;
            let z0 = offset_z + ty as f32;
            let z1 = z0 + 1.0;

            // UV coords: map to the tile's strip in the atlas
            let v0 = tile_idx / n_tileset;
            let v1 = (tile_idx + 1.0) / n_tileset;

            verts.push(Vertex {
                pos: [x0, y, z0],
                normal,
                uv: [0.0, v0],
                tangent,
            });
            verts.push(Vertex {
                pos: [x1, y, z0],
                normal,
                uv: [1.0, v0],
                tangent,
            });
            verts.push(Vertex {
                pos: [x1, y, z1],
                normal,
                uv: [1.0, v1],
                tangent,
            });
            verts.push(Vertex {
                pos: [x0, y, z1],
                normal,
                uv: [0.0, v1],
                tangent,
            });

            // Two triangles (CCW winding when viewed from above)
            indices.push(base_vert);
            indices.push(base_vert + 2);
            indices.push(base_vert + 1);
            indices.push(base_vert);
            indices.push(base_vert + 3);
            indices.push(base_vert + 2);

            bounds.include_point(Vec3::new(x0, y, z0));
            bounds.include_point(Vec3::new(x1, y, z1));
        }
    }

    // Upload buffers
    let vert_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("tilemap_verts"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ind_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("tilemap_indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    // Create material with Nearest filtering for crisp tiles
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("tilemap_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let material = renderer.create_material(
        device,
        queue,
        &sampler,
        &atlas_view,
        MaterialUniform {
            base_color_factor: glam::Vec4::ONE,
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            ao_strength: 1.0,
            _pad0: 0.0,
        },
    );

    let model_data = ModelData {
        draws: vec![ModelDraw {
            mesh: Mesh {
                num_indices: indices.len() as u32,
                vert_buf,
                ind_buf,
            },
            material_index: 0,
        }],
        materials: vec![material],
        bounds,
    };

    renderer.insert_model(model_data)
}
