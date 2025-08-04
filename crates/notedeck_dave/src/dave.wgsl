
struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    camera_pos: vec4<f32>,  // world-space camera position
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VSOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) in_pos: vec3<f32>,
    @location(1) in_normal: vec3<f32>
) -> VSOut {
    var out: VSOut;
    let world = uniforms.model * vec4<f32>(in_pos, 1.0);
    out.position = uniforms.view_proj * world;

    // normal = (model rotation) * in_normal
    let nmat = mat3x3<f32>(
        uniforms.model[0].xyz,
        uniforms.model[1].xyz,
        uniforms.model[2].xyz
    );
    //out.normal = normalize(transpose(inverse(nmat)) * in_normal);
    out.normal = normalize(nmat * in_normal);
    out.world_pos = world.xyz;
    return out;
}

@fragment
fn fs_main_debug(in: VSOut) -> @location(0) vec4<f32> {
    let g = normalize(cross(dpdx(in.world_pos), dpdy(in.world_pos)));
    let n = normalize(in.normal);
    let shown = 0.5 * (n + vec3<f32>(1.0,1.0,1.0));
    return vec4<f32>(shown, 1.0);
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let material_color = vec3<f32>(1.0, 1.0, 1.0);
    let ambient_strength = 0.2;
    let diffuse_strength = 0.7;
    let specular_strength = 0.2;
    let shininess = 20.0;

    let light_pos = vec3<f32>(2.0, 2.0, 2.0);
    let light_color = vec3<f32>(1.0, 1.0, 1.0);
    let view_pos = uniforms.camera_pos.xyz;

    let ambient = ambient_strength * light_color;

    let n = normalize(in.normal);
    let l = normalize(light_pos - in.world_pos);
    let diff = max(dot(n, l), 0.0);
    let diffuse = diffuse_strength * diff * light_color;

    let v = normalize(view_pos - in.world_pos);
    let r = reflect(-l, n);
    let spec = pow(max(dot(v, r), 0.0), shininess);
    let specular = specular_strength * spec * light_color;

    let result = (ambient + diffuse + specular) * material_color;
    return vec4<f32>(result, 1.0);
}
