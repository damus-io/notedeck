//! The level-up toast's 3D medallion: a gilt coin built in code, drawn
//! offscreen through the host's shared renderer
//! ([`Renderer3d`](notedeck::Renderer3d)) and laid under the new level.
//!
//! The split follows headway's `review_models`:
//!
//! - [`coin_mesh`] builds the geometry, turned from a profile: a rounded
//!   rim around a recessed field on both faces. No asset files.
//! - [`Coin::poll`] uploads it on a worker through
//!   [`Renderer3d::model_uploader`](notedeck::Renderer3d::model_uploader),
//!   which also builds the shared renderer the first time, slowly, there
//!   rather than in a frame. When the upload lands, the UI thread gives the
//!   model to the renderer, makes a transparent offscreen
//!   [`ModelView`] of it and registers that as an egui texture.
//! - [`CoinView::turn`] spins it to a yaw and renders again: only on the
//!   frames the toast is spinning it, never at rest.
//!
//! The coin stands upright with its faces along ±Z, so the view's yaw (a
//! turn about the vertical axis) flips it like a tossed coin. Its metal is
//! [`HudPalette::gilt`](crate::ui::palette::HudPalette::gilt), so a theme
//! reskins it; a new colour builds a new coin. With no renderer (headless,
//! or a window without wgpu) nothing is built and the toast keeps its 2D
//! medallion.

use std::f32::consts::TAU;

use egui::{Color32, TextureId};
use notedeck::Renderer3d;
use poll_promise::Promise;
use renderbud::{
    MaterialUniform, MeshData, ModelData, ModelView, PixelSize, Vertex, ViewBackground, ViewOptions,
};

/// Steps round the coin's circles.
const SEGMENTS: u32 = 64;

/// Where the raised rim gives way to the field, of the coin's radius (1).
const RIM_INNER: f32 = 0.8;

/// Half the coin's thickness, at the top of the rim.
const HALF_THICKNESS: f32 = 0.09;

/// How far the field sits below the rim on each face.
const FIELD_DEPTH: f32 = 0.035;

/// How far the field's normals lean out at its edge, so the flat field
/// shades as a shallow dome rather than one flat reflection.
const FIELD_DOME: f32 = 0.22;

/// How polished the metal is: microverse's brass.
const METALLIC: f32 = 1.0;

/// See [`METALLIC`].
const ROUGHNESS: f32 = 0.28;

/// One ring of the coin's profile, the shape [`lathe`] turns about the Z
/// axis: its radius, its depth, and its normal's radial and Z parts.
#[derive(Debug, Clone, Copy)]
struct Ring {
    r: f32,
    z: f32,
    normal: [f32; 2],
}

/// A ring at radius `r` and depth `z`, its normal leaning `radial` out for
/// each 1 along Z (normalized when it is turned).
const fn ring(r: f32, z: f32, radial: f32, along: f32) -> Ring {
    Ring {
        r,
        z,
        normal: [radial, along],
    }
}

/// The coin, radius 1, standing in the XY plane with its faces along ±Z.
/// Turned from a profile in three strips, each smooth inside and creased
/// where it meets the next: on each face a field recessed [`FIELD_DEPTH`]
/// below the rim (flat, but shaded as a shallow dome), the step up out of
/// it, and one strip from the front rim over the rounded edge to the back
/// rim, so the light catches the rim apart from the field. Startup only
/// (once per coin built); allocates.
pub fn coin_mesh() -> MeshData {
    let h = HALF_THICKNESS;
    let field = h - FIELD_DEPTH;
    let mut mesh = MeshData::default();
    for side in [1.0, -1.0] {
        let face = |r: Ring| Ring {
            z: r.z * side,
            normal: [r.normal[0], r.normal[1] * side],
            ..r
        };
        let dome = [
            ring(0.0, field, 0.0, 1.0),
            ring(RIM_INNER * 0.5, field, FIELD_DOME * 0.5, 1.0),
            ring(RIM_INNER, field, FIELD_DOME, 1.0),
        ];
        lathe(&mut mesh, &dome.map(face));
        let step = [
            ring(RIM_INNER, field, -1.0, 0.3),
            ring(RIM_INNER + 0.02, h, -1.0, 0.3),
        ];
        lathe(&mut mesh, &step.map(face));
    }
    // Front rim, over the edge, back rim: rounded, so one smooth strip.
    let rim = [
        ring(RIM_INNER + 0.02, h, -0.25, 1.0),
        ring(0.88, h, 0.0, 1.0),
        ring(0.95, h - 0.015, 0.5, 1.0),
        ring(0.99, h - 0.045, 1.0, 0.6),
        ring(1.0, 0.0, 1.0, 0.0),
    ];
    let back = |r: &Ring| Ring {
        z: -r.z,
        normal: [r.normal[0], -r.normal[1]],
        ..*r
    };
    let rim: Vec<Ring> = rim
        .iter()
        .copied()
        .chain(rim.iter().rev().skip(1).map(back))
        .collect();
    lathe(&mut mesh, &rim);
    mesh
}

/// Step `k` of [`SEGMENTS`] round a circle: its cosine and sine.
fn around(k: u32) -> [f32; 2] {
    let (sin, cos) = (k as f32 / SEGMENTS as f32 * TAU).sin_cos();
    [cos, sin]
}

/// Adds a vertex at `pos` facing `normal`, mapped planar across the face.
fn push(mesh: &mut MeshData, pos: [f32; 3], normal: [f32; 3]) -> u32 {
    let at = mesh.vertices.len() as u32;
    mesh.vertices.push(Vertex {
        pos,
        normal,
        uv: [pos[0] * 0.5 + 0.5, 0.5 - pos[1] * 0.5],
        tangent: [0.0; 4],
    });
    at
}

/// Adds triangle `a b c`, wound counter-clockwise seen from the side its
/// vertices face, whichever order it was given in.
fn tri(mesh: &mut MeshData, [a, b, c]: [u32; 3]) {
    let pos = |i: u32| glam::Vec3::from(mesh.vertices[i as usize].pos);
    let normal = |i: u32| glam::Vec3::from(mesh.vertices[i as usize].normal);
    let facing = normal(a) + normal(b) + normal(c);
    let wound = (pos(b) - pos(a)).cross(pos(c) - pos(a));
    let corners = if wound.dot(facing) >= 0.0 {
        [a, b, c]
    } else {
        [a, c, b]
    };
    mesh.indices.extend(corners);
}

/// Turns `profile` about the Z axis into a smooth surface of revolution,
/// one band of quads between each pair of neighbouring rings. A ring of
/// radius 0 is a single point the next ring fans out from.
fn lathe(mesh: &mut MeshData, profile: &[Ring]) {
    // The first vertex of each ring's circle (or its point), in order.
    let mut starts = Vec::with_capacity(profile.len());
    for ring in profile {
        starts.push(mesh.vertices.len() as u32);
        let [radial, along] = ring.normal;
        let len = radial.hypot(along).max(1e-6);
        let (radial, along) = (radial / len, along / len);
        if ring.r == 0.0 {
            push(mesh, [0.0, 0.0, ring.z], [0.0, 0.0, along.signum()]);
            continue;
        }
        for k in 0..SEGMENTS {
            let [cos, sin] = around(k);
            let pos = [cos * ring.r, sin * ring.r, ring.z];
            push(mesh, pos, [cos * radial, sin * radial, along]);
        }
    }
    for n in 1..profile.len() {
        let (inner, outer) = (starts[n - 1], starts[n]);
        for k in 0..SEGMENTS {
            let next = (k + 1) % SEGMENTS;
            let (o, o2) = (outer + k, outer + next);
            if profile[n - 1].r == 0.0 {
                tri(mesh, [inner, o, o2]);
                continue;
            }
            let (i, i2) = (inner + k, inner + next);
            tri(mesh, [i, o, o2]);
            tri(mesh, [i, o2, i2]);
        }
    }
}

/// The coin's material: polished metal in `gilt`.
fn material(gilt: Color32) -> MaterialUniform {
    let linear = egui::Rgba::from(gilt);
    MaterialUniform {
        base_color_factor: glam::Vec4::new(linear.r(), linear.g(), linear.b(), 1.0),
        metallic_factor: METALLIC,
        roughness_factor: ROUGHNESS,
        ao_strength: 0.0,
        _pad0: 0.0,
    }
}

/// The coin as the toast draws it: its offscreen view and the egui texture
/// showing it. Dropping it frees the texture and takes the model out of
/// the shared renderer.
pub struct CoinView {
    renderer: Renderer3d,
    view: ModelView,
    texture: TextureId,
}

impl CoinView {
    /// Give `data` to `renderer`, make a transparent `pixels` view of it,
    /// render it once and register it with egui. UI thread. `None` when the
    /// renderer wouldn't make the view.
    fn new(renderer: &Renderer3d, data: ModelData, pixels: PixelSize) -> Option<Self> {
        let (device, queue) = (renderer.device(), renderer.queue());
        let view = {
            let mut scene = renderer
                .scene()
                .renderer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let model = scene.insert_model(data);
            let options = ViewOptions {
                background: ViewBackground::Transparent,
            };
            match scene.create_model_view_with(device, model, pixels, options) {
                Some(mut view) => {
                    scene.render_model_view(device, queue, &mut view);
                    view
                }
                None => {
                    scene.remove_model(model);
                    return None;
                }
            }
        };
        let texture = renderer.register_native_texture(view.texture_view());
        Some(Self {
            renderer: renderer.clone(),
            view,
            texture,
        })
    }

    /// The egui texture the coin is drawn into.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// Spin the coin to `yaw` radians and render it again if that moved it.
    /// UI thread; costs nothing at an unchanged angle.
    pub fn turn(&mut self, yaw: f32) {
        self.view.set_yaw(yaw);
        if !self.view.needs_render() {
            return;
        }
        let renderer = &self.renderer;
        let scene = renderer
            .scene()
            .renderer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scene.render_model_view(renderer.device(), renderer.queue(), &mut self.view);
    }
}

impl Drop for CoinView {
    fn drop(&mut self) {
        let mut scene = self
            .renderer
            .scene()
            .renderer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.renderer.free_texture(self.texture);
        scene.remove_model(self.view.model());
    }
}

/// Where the coin is in being built.
#[derive(Default)]
pub enum Coin {
    /// Not built yet: no renderer has been offered, or the palette's metal
    /// changed and the old coin was let go.
    #[default]
    Unbuilt,
    /// Uploading on a worker, in `gilt`.
    Loading {
        /// The metal it is being built in.
        gilt: Color32,
        /// The upload, `None` if it failed.
        upload: Promise<Option<ModelData>>,
    },
    /// Built in `gilt` and ready to draw.
    Ready {
        /// The metal it was built in.
        gilt: Color32,
        /// The coin's view and texture, boxed as it is much larger than
        /// the other states.
        view: Box<CoinView>,
    },
    /// The upload or the view failed; the toast keeps its 2D medallion
    /// rather than trying every frame.
    Failed,
}

impl std::fmt::Debug for Coin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Coin::Unbuilt => "Unbuilt",
            Coin::Loading { .. } => "Loading",
            Coin::Ready { .. } => "Ready",
            Coin::Failed => "Failed",
        })
    }
}

impl Coin {
    /// The coin to draw this frame in `gilt`, at `pixels`, if it is built.
    /// Otherwise starts building it on `renderer` (and wakes `ctx` when the
    /// worker is done), or finishes a build that has landed. `None` without
    /// a renderer, while it builds, or once it has failed: draw the 2D
    /// medallion then.
    pub fn poll(
        &mut self,
        renderer: Option<&Renderer3d>,
        gilt: Color32,
        pixels: PixelSize,
        ctx: &egui::Context,
    ) -> Option<&mut CoinView> {
        let renderer = renderer?;
        let stale = match self {
            Coin::Loading { gilt: built, .. } | Coin::Ready { gilt: built, .. } => *built != gilt,
            Coin::Unbuilt | Coin::Failed => false,
        };
        if stale {
            *self = Coin::Unbuilt;
        }
        if matches!(self, Coin::Unbuilt) {
            *self = Coin::Loading {
                gilt,
                upload: upload(renderer, gilt, ctx),
            };
        }
        if let Coin::Loading { .. } = self {
            let Coin::Loading { gilt, upload } = std::mem::take(self) else {
                unreachable!("matched Loading above");
            };
            *self = match upload.try_take() {
                Err(upload) => Coin::Loading { gilt, upload },
                Ok(Some(data)) => match CoinView::new(renderer, data, pixels) {
                    Some(view) => Coin::Ready {
                        gilt,
                        view: Box::new(view),
                    },
                    None => Coin::Failed,
                },
                Ok(None) => Coin::Failed,
            };
        }
        match self {
            Coin::Ready { view, .. } => Some(view),
            Coin::Unbuilt | Coin::Loading { .. } | Coin::Failed => None,
        }
    }

    /// Whether the coin is built and drawing.
    pub fn is_ready(&self) -> bool {
        matches!(self, Coin::Ready { .. })
    }
}

/// Builds the coin in `gilt` on a worker, uploading it through `renderer`
/// (building the shared renderer if nothing has yet), and wakes `ctx` when
/// it is done.
fn upload(renderer: &Renderer3d, gilt: Color32, ctx: &egui::Context) -> Promise<Option<ModelData>> {
    let renderer = renderer.clone();
    let ctx = ctx.clone();
    Promise::spawn_thread("cashflow_coin", move || {
        let uploaded = renderer
            .model_uploader()
            .upload_mesh(coin_mesh(), material(gilt));
        ctx.request_repaint();
        uploaded
            .inspect_err(|e| tracing::warn!("building the level-up coin: {e}"))
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn vertex(mesh: &MeshData, i: u32) -> &Vertex {
        &mesh.vertices[i as usize]
    }

    /// The coin is radius 1 across X and Y and its thickness along Z, so it
    /// stands facing the camera and a yaw flips it.
    #[test]
    fn coin_stands_facing_along_z() {
        let mesh = coin_mesh();
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &mesh.vertices {
            min = min.min(Vec3::from(v.pos));
            max = max.max(Vec3::from(v.pos));
        }
        assert!((max.x - 1.0).abs() < 1e-5 && (min.x + 1.0).abs() < 1e-5);
        assert!((max.y - 1.0).abs() < 1e-3 && (min.y + 1.0).abs() < 1e-3);
        assert!((max.z - HALF_THICKNESS).abs() < 1e-6);
        assert!((min.z + HALF_THICKNESS).abs() < 1e-6);
    }

    /// Every index names a vertex, and every triangle is wound to face the
    /// way its vertices' normals point.
    #[test]
    fn every_triangle_faces_along_its_normals() {
        let mesh = coin_mesh();
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!(mesh.indices.len() / 3 > SEGMENTS as usize * 8);
        for t in mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| {
                assert!((i as usize) < mesh.vertices.len());
                Vec3::from(vertex(&mesh, i).pos)
            });
            let wound = (b - a).cross(c - a);
            for i in t {
                let normal = Vec3::from(vertex(&mesh, *i).normal);
                assert!(wound.dot(normal) > 0.0, "triangle {t:?} faces away");
            }
        }
    }

    /// Each face's field sits below its rim, so the rim stands proud.
    #[test]
    fn the_field_is_recessed_inside_the_rim() {
        let mesh = coin_mesh();
        let highest = |inside: bool| {
            mesh.vertices
                .iter()
                .filter(|v| (Vec3::from(v.pos).truncate().length() < RIM_INNER - 1e-4) == inside)
                .map(|v| v.pos[2])
                .fold(f32::MIN, f32::max)
        };
        assert_eq!(highest(false), HALF_THICKNESS);
        assert!((highest(false) - highest(true) - FIELD_DEPTH).abs() < 1e-6);
    }

    /// The metal is the palette's colour, made linear.
    #[test]
    fn material_takes_the_palette_metal() {
        let m = material(Color32::from_rgb(244, 203, 97));
        let c = m.base_color_factor;
        assert!((c.x - 0.90).abs() < 0.01, "{c}");
        assert!((c.y - 0.60).abs() < 0.01, "{c}");
        assert!((c.z - 0.12).abs() < 0.01, "{c}");
        assert_eq!(m.metallic_factor, METALLIC);
    }

    /// With no renderer nothing is built, and the toast draws in 2D.
    #[test]
    fn without_a_renderer_there_is_no_coin() {
        let mut coin = Coin::default();
        let ctx = egui::Context::default();
        let gilt = Color32::from_rgb(244, 203, 97);
        assert!(coin.poll(None, gilt, PixelSize::new(8, 8), &ctx).is_none());
        assert!(matches!(coin, Coin::Unbuilt));
    }
}
