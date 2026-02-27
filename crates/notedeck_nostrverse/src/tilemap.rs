//! Tilemap mesh generation — builds a single textured quad mesh from TilemapData.

use crate::room_state::TilemapData;
use egui_wgpu::wgpu;
use glam::Vec3;
use renderbud::{Aabb, MaterialUniform, Mesh, ModelData, ModelDraw, Vertex};
use wgpu::util::DeviceExt;

/// Size of each tile in the atlas, in pixels.
const TILE_PX: u32 = 32;

/// Per-pixel detail pattern applied on top of the base color + noise.
enum TileDetail {
    /// No extra detail, just base color with noise variation.
    None,
    /// Dark clumps (coarse noise) + sparse bright specks on green channel.
    /// Used for grass-like tiles.
    Clumpy,
    /// Dark lines where coarse noise crosses zero — looks like cracks.
    Cracked,
    /// Sinusoidal horizontal wave highlights on green+blue channels.
    Wavy,
    /// Sparse bright specks across all channels — looks like pebbles.
    Speckled,
    /// Coarse blue-shifted shadows — looks like snow/ice.
    Frosty,
    /// Horizontal sinusoidal grain lines — looks like wood.
    Grainy,
}

/// Describes how to procedurally generate a tile texture.
struct TileStyle {
    base: [u8; 3],
    variation: i16,
    detail: TileDetail,
}

/// Look up the style for a tile name.
fn tile_style(name: &str) -> TileStyle {
    match name {
        "grass" => TileStyle {
            base: [76, 140, 56],
            variation: 20,
            detail: TileDetail::Clumpy,
        },
        "stone" | "rock" => TileStyle {
            base: [140, 136, 128],
            variation: 18,
            detail: TileDetail::Cracked,
        },
        "water" => TileStyle {
            base: [48, 100, 160],
            variation: 12,
            detail: TileDetail::Wavy,
        },
        "sand" => TileStyle {
            base: [194, 178, 128],
            variation: 15,
            detail: TileDetail::None,
        },
        "dirt" | "earth" | "mud" => TileStyle {
            base: [120, 85, 58],
            variation: 16,
            detail: TileDetail::Speckled,
        },
        "snow" | "ice" => TileStyle {
            base: [220, 225, 235],
            variation: 10,
            detail: TileDetail::Frosty,
        },
        "wood" | "plank" | "floor" => TileStyle {
            base: [156, 110, 68],
            variation: 12,
            detail: TileDetail::Grainy,
        },
        _ => TileStyle {
            base: hash_name_to_color(name),
            variation: 18,
            detail: TileDetail::None,
        },
    }
}

/// Deterministic color from an unknown tile name.
fn hash_name_to_color(name: &str) -> [u8; 3] {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    [
        80 + (h & 0xFF) as u8 % 120,
        80 + ((h >> 8) & 0xFF) as u8 % 120,
        80 + ((h >> 16) & 0xFF) as u8 % 120,
    ]
}

/// Simple deterministic hash for procedural noise.
fn hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = seed;
    h = h
        .wrapping_mul(374761393)
        .wrapping_add(x.wrapping_mul(668265263));
    h = h
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(2654435761));
    h ^= h >> 13;
    h = h.wrapping_mul(1274126177);
    h ^= h >> 16;
    h
}

/// Noise value in -1.0..1.0 for pixel (x, y).
fn noise(x: u32, y: u32, seed: u32) -> f32 {
    (hash(x, y, seed) & 0xFFFF) as f32 / 32768.0 - 1.0
}

/// Base color + random per-pixel brightness variation.
fn vary(base: [u8; 3], amount: i16, x: u32, y: u32, seed: u32) -> [u8; 4] {
    let n = noise(x, y, seed);
    let delta = (n * amount as f32) as i16;
    let r = (base[0] as i16 + delta).clamp(0, 255) as u8;
    let g = (base[1] as i16 + delta).clamp(0, 255) as u8;
    let b = (base[2] as i16 + delta).clamp(0, 255) as u8;
    [r, g, b, 255]
}

/// Apply detail pattern to a pixel. `lx`/`ly` are tile-local coordinates.
fn apply_detail(px: &mut [u8; 4], detail: &TileDetail, x: u32, lx: u32, ly: u32, seed: u32) {
    match detail {
        TileDetail::None => {}
        TileDetail::Clumpy => {
            if noise(lx / 4, ly / 4, seed ^ 0xBEEF) > 0.4 {
                px[0] = px[0].saturating_sub(15);
                px[1] = px[1].saturating_sub(8);
                px[2] = px[2].saturating_sub(12);
            }
            if noise(x, ly, seed ^ 0xCAFE) > 0.85 {
                px[1] = px[1].saturating_add(30);
            }
        }
        TileDetail::Cracked => {
            if noise(lx / 3, ly / 3, seed ^ 0xD00D).abs() < 0.08 {
                px[0] = px[0].saturating_sub(35);
                px[1] = px[1].saturating_sub(35);
                px[2] = px[2].saturating_sub(35);
            }
        }
        TileDetail::Wavy => {
            let wave = ((ly as f32 * 0.4 + lx as f32 * 0.15).sin() * 0.5 + 0.5) * 20.0;
            px[1] = px[1].saturating_add(wave as u8);
            px[2] = px[2].saturating_add(wave as u8);
        }
        TileDetail::Speckled => {
            if noise(x, ly, seed ^ 0xF00D) > 0.88 {
                px[0] = px[0].saturating_add(25);
                px[1] = px[1].saturating_add(20);
                px[2] = px[2].saturating_add(15);
            }
        }
        TileDetail::Frosty => {
            if noise(lx / 5, ly / 5, seed ^ 0x1CE) > 0.3 {
                px[2] = px[2].saturating_add(8);
                px[0] = px[0].saturating_sub(5);
            }
        }
        TileDetail::Grainy => {
            if (ly as f32 * 1.2).sin().abs() < 0.15 {
                px[0] = px[0].saturating_sub(20);
                px[1] = px[1].saturating_sub(15);
                px[2] = px[2].saturating_sub(10);
            }
        }
    }
}

/// Generate a procedural tile texture into the atlas at the given row.
fn fill_tile(rgba: &mut [u8], atlas_w: u32, y_start: u32, name: &str, tile_idx: u32) {
    let seed = tile_idx.wrapping_mul(2654435761);
    let style = tile_style(name);

    for y in y_start..y_start + TILE_PX {
        for x in 0..TILE_PX {
            let off = ((y * atlas_w + x) * 4) as usize;
            let ly = y - y_start;
            let mut px = vary(style.base, style.variation, x, y, seed);
            apply_detail(&mut px, &style.detail, x, x, ly, seed);
            rgba[off..off + 4].copy_from_slice(&px);
        }
    }
}

/// Build the atlas RGBA texture data.
/// Atlas is a 1-tile-wide vertical strip (TILE_PX x (TILE_PX * N)).
fn build_atlas(tileset: &[String]) -> (u32, u32, Vec<u8>) {
    let n = tileset.len().max(1) as u32;
    let atlas_w = TILE_PX;
    let atlas_h = TILE_PX * n;
    let mut rgba = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (i, name) in tileset.iter().enumerate() {
        let y_start = i as u32 * TILE_PX;
        fill_tile(&mut rgba, atlas_w, y_start, name, i as u32);
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
    let y = 0.01_f32; // Above ground plane to avoid z-fighting with grid

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
