// Shadow depth pass: renders scene from light's perspective (depth only)

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

struct Object {
    model: mat4x4<f32>,
    normal: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> object: Object;

struct VSIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

@vertex
fn vs_main(v: VSIn) -> @builtin(position) vec4<f32> {
    let world4 = object.model * vec4<f32>(v.pos, 1.0);
    return globals.light_view_proj * world4;
}
