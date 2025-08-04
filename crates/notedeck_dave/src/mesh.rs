use eframe::egui_wgpu::wgpu;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

impl Vertex {
    pub const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
        0 => Float32x3, // position
        1 => Float32x3  // normal
    ];
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &Self::ATTRS,
    };
}

// 6 faces * 4 verts. Each face has a constant normal.
#[rustfmt::skip]
pub const CUBE_VERTICES: [Vertex; 24] = [
    // -Z (back)
    Vertex { pos: [-0.5,-0.5,-0.5], normal: [0.0, 0.0,-1.0] },
    Vertex { pos: [ 0.5,-0.5,-0.5], normal: [0.0, 0.0,-1.0] },
    Vertex { pos: [ 0.5, 0.5,-0.5], normal: [0.0, 0.0,-1.0] },
    Vertex { pos: [-0.5, 0.5,-0.5], normal: [0.0, 0.0,-1.0] },

    // +Z (front)
    Vertex { pos: [-0.5,-0.5, 0.5], normal: [0.0, 0.0, 1.0] },
    Vertex { pos: [ 0.5,-0.5, 0.5], normal: [0.0, 0.0, 1.0] },
    Vertex { pos: [ 0.5, 0.5, 0.5], normal: [0.0, 0.0, 1.0] },
    Vertex { pos: [-0.5, 0.5, 0.5], normal: [0.0, 0.0, 1.0] },

    // -X (left)
    Vertex { pos: [-0.5,-0.5,-0.5], normal: [-1.0, 0.0, 0.0] },
    Vertex { pos: [-0.5, 0.5,-0.5], normal: [-1.0, 0.0, 0.0] },
    Vertex { pos: [-0.5, 0.5, 0.5], normal: [-1.0, 0.0, 0.0] },
    Vertex { pos: [-0.5,-0.5, 0.5], normal: [-1.0, 0.0, 0.0] },

    // +X (right)
    Vertex { pos: [ 0.5,-0.5,-0.5], normal: [1.0, 0.0, 0.0] },
    Vertex { pos: [ 0.5, 0.5,-0.5], normal: [1.0, 0.0, 0.0] },
    Vertex { pos: [ 0.5, 0.5, 0.5], normal: [1.0, 0.0, 0.0] },
    Vertex { pos: [ 0.5,-0.5, 0.5], normal: [1.0, 0.0, 0.0] },

    // -Y (bottom)
    Vertex { pos: [-0.5,-0.5,-0.5], normal: [0.0,-1.0, 0.0] },
    Vertex { pos: [-0.5,-0.5, 0.5], normal: [0.0,-1.0, 0.0] },
    Vertex { pos: [ 0.5,-0.5, 0.5], normal: [0.0,-1.0, 0.0] },
    Vertex { pos: [ 0.5,-0.5,-0.5], normal: [0.0,-1.0, 0.0] },

    // +Y (top)
    Vertex { pos: [-0.5, 0.5,-0.5], normal: [0.0, 1.0, 0.0] },
    Vertex { pos: [-0.5, 0.5, 0.5], normal: [0.0, 1.0, 0.0] },
    Vertex { pos: [ 0.5, 0.5, 0.5], normal: [0.0, 1.0, 0.0] },
    Vertex { pos: [ 0.5, 0.5,-0.5], normal: [0.0, 1.0, 0.0] },
];

// 6 faces * 2 triangles * 3 indices — all CCW when viewed from the outside
pub const CUBE_INDICES: [u16; 36] = [
    // -Z (back)   normal (0, 0,-1)
    0, 3, 2, 0, 2, 1, // +Z (front)  normal (0, 0, 1)
    4, 5, 6, 4, 6, 7, // -X (left)   normal (-1,0, 0)
    8, 11, 10, 8, 10, 9, // +X (right)  normal ( 1,0, 0)
    12, 13, 14, 12, 14, 15, // -Y (bottom) normal (0,-1, 0)
    16, 18, 17, 16, 19, 18, // +Y (top)    normal (0, 1, 0)
    20, 21, 22, 20, 22, 23,
];
