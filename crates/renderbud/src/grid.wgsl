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
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

struct VSOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) near_point: vec3<f32>,
    @location(1) far_point: vec3<f32>,
};

fn unproject(clip: vec2<f32>, z: f32) -> vec3<f32> {
    let p = globals.inv_view_proj * vec4<f32>(clip, z, 1.0);
    return p.xyz / p.w;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VSOut {
    var out: VSOut;

    // Fullscreen triangle: vertices at (-1,-1), (3,-1), (-1,3)
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    out.clip = vec4<f32>(x, y, 0.0, 1.0);

    // Unproject near and far planes to world space
    out.near_point = unproject(vec2<f32>(x, y), 0.0);
    out.far_point = unproject(vec2<f32>(x, y), 1.0);

    return out;
}

// Compute grid intensity for a given world-space xz coordinate and grid spacing
fn grid_line(coord: vec2<f32>, spacing: f32, line_width: f32) -> f32 {
    let grid = abs(fract(coord / spacing - 0.5) - 0.5) * spacing;
    let dxz = fwidth(coord);
    let width = dxz * line_width;
    let line = smoothstep(width, vec2<f32>(0.0), grid);
    return max(line.x, line.y);
}

fn calc_shadow(world_pos: vec3<f32>) -> f32 {
    let light_clip = globals.light_view_proj * vec4<f32>(world_pos, 1.0);
    let ndc = light_clip.xyz / light_clip.w;
    let shadow_uv = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);

    if shadow_uv.x < 0.0 || shadow_uv.x > 1.0 || shadow_uv.y < 0.0 || shadow_uv.y > 1.0 {
        return 1.0;
    }

    let ref_depth = ndc.z;
    let texel_size = 1.0 / 2048.0;
    var shadow = 0.0;
    for (var y = -1i; y <= 1i; y++) {
        for (var x = -1i; x <= 1i; x++) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow += textureSampleCompareLevel(
                shadow_map, shadow_sampler, shadow_uv + offset, ref_depth,
            );
        }
    }
    return shadow / 9.0;
}

struct FragOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_main(in: VSOut) -> FragOut {
    var out: FragOut;

    // Ray from near to far point
    let ray_dir = in.far_point - in.near_point;

    // Intersect y=0 plane: near.y + t * dir.y = 0
    let t = -in.near_point.y / ray_dir.y;

    // Discard if no intersection (ray parallel or pointing away)
    if t < 0.0 {
        discard;
    }

    // World position on the grid plane
    let world_pos = in.near_point + t * ray_dir;
    let xz = vec2<f32>(world_pos.x, world_pos.z);

    // Distance from camera for fading
    let dist = length(world_pos - globals.cam_pos);

    // Grid lines
    let minor = grid_line(xz, 0.25, 0.5);     // 0.25m subdivisions
    let major = grid_line(xz, 1.0, 1.0);       // 1.0m major lines

    // Combine: major lines are brighter
    let grid_val = max(minor * 0.3, major * 0.6);

    // Fade with distance (start fading at 10m, fully gone at 80m)
    let fade = 1.0 - smoothstep(10.0, 80.0, dist);

    let alpha = grid_val * fade;

    // Discard fully transparent fragments
    if alpha < 0.001 {
        discard;
    }

    // Shadow: darken the grid where objects cast shadows
    let shadow = calc_shadow(world_pos);
    let brightness = mix(0.15, 0.5, shadow);

    out.color = vec4<f32>(brightness, brightness, brightness, alpha);

    // Compute proper depth from world position
    let clip = globals.view_proj * vec4<f32>(world_pos, 1.0);
    out.depth = clip.z / clip.w;

    return out;
}
