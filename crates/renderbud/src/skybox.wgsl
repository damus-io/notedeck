struct Globals {
    time: f32,
    _pad0: f32,
    resolution: vec2<f32>,

    cam_pos: vec3<f32>,
    _pad3: f32,

    light_dir: vec3<f32>,
    _pad1: f32,

    light_color: vec3<f32>,
    _pad2: f32,

    fill_light_dir: vec3<f32>,
    _pad4: f32,

    fill_light_color: vec3<f32>,
    _pad5: f32,

    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(3) @binding(1) var ibl_sampler: sampler;
@group(3) @binding(2) var prefiltered_map: texture_cube<f32>;

struct VSOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ray_dir: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VSOut {
    var out: VSOut;

    // Fullscreen triangle: vertices at (-1,-1), (3,-1), (-1,3)
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    out.clip = vec4<f32>(x, y, 1.0, 1.0);

    // Unproject to get ray direction
    let near = globals.inv_view_proj * vec4<f32>(x, y, 0.0, 1.0);
    let far = globals.inv_view_proj * vec4<f32>(x, y, 1.0, 1.0);
    out.ray_dir = normalize(far.xyz / far.w - near.xyz / near.w);

    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    // Sample prefiltered map at slight blur level (mip 1 of 5)
    let hdr = textureSampleLevel(prefiltered_map, ibl_sampler, in.ray_dir, 1.0).rgb;

    // Reinhard tonemap
    let col = hdr / (hdr + vec3<f32>(1.0));

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
