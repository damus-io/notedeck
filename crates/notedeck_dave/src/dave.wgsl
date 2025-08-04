struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    camera_pos: vec3<f32>,
    time: f32,
    is_light: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VSOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) color: vec3<f32>,
};

// Vertex inputs
@vertex
fn vs_main(
    @location(0) in_pos: vec3<f32>,
    @location(1) in_normal: vec3<f32>,
    @location(2) base_pos: vec3<f32>,
    @location(3) scale: f32,
    @location(4) seed: f32,
    @location(5) color: vec3<f32>,
) -> VSOut {
    var out: VSOut;

    let t = uniforms.time;

    // --- Coherent spherical layout ---
    let dir = normalize(base_pos + vec3<f32>(1e-6, 0.0, 0.0)); // avoid NaN if zero
    let radius = 0.4;

    // Gentle, coherent drift so it breathes
    let drift = vec3<f32>(
        0.03 * sin(0.9 * t + seed * 1.3),
        0.02 * sin(1.1 * t + seed * 2.1),
        0.03 * cos(0.7 * t + seed * 0.7)
    );

    // Final instance position on/near the sphere
    //let loose = 0.2 * base_pos + drift;
    let tight = dir * radius + drift;
    //let tight = dir * radius;
    //let coherence = 0.8; // [0..1], or pass as a uniform
    //let pos_ws = mix(loose, tight, coherence);
    let pos_ws = tight;

    // --- Orient cube so its local +Z points outward (along dir) ---
    // Build a stable tangent basis
    var up = vec3<f32>(0.0, 1.0, 0.0);
    if (abs(dot(dir, up)) > 0.92) {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let tangent   = normalize(cross(up, dir));
    let bitangent = cross(dir, tangent);

    // Optional tiny spin around outward axis for sparkle
    let spin = 0.9 * t + seed * 0.9;
    let cs = cos(spin);
    let sn = sin(spin);
    let rot_tangent   =  cs * tangent + sn * bitangent;
    let rot_bitangent = -sn * tangent + cs * bitangent;

    // Rotation matrix whose columns are the local basis
    let R = mat3x3<f32>(rot_tangent, rot_bitangent, dir);

    // Scale + orient local vertex + place at spherical position
    let local = R * (in_pos * scale);
    let world_vec4 = uniforms.model * vec4<f32>(local, 1.0);
    let world = world_vec4 + vec4<f32>(pos_ws, 0.0);

    out.position = uniforms.view_proj * world;

    // Normal from model rotation only (ignoring per-instance rotation for now)
    let nmat = mat3x3<f32>(
        uniforms.model[0].xyz,
        uniforms.model[1].xyz,
        uniforms.model[2].xyz
    );
    out.normal = normalize(nmat * in_normal);
    out.world_pos = world.xyz;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    // Same lighting as you had, but tint by per-instance color
    let material_color = in.color;

    let ambient_strength = 0.2;
    let diffuse_strength = 0.7;
    let specular_strength = 0.2;
    let shininess = 20.0;

    let light_pos = vec3<f32>(2.0, 2.0, 2.0);
    let light_color = vec3<f32>(1.0, 1.0, 1.0);
    let view_pos = uniforms.camera_pos;

    let n = normalize(in.normal);
    let l = normalize(light_pos - in.world_pos);
    let v = normalize(view_pos - in.world_pos);
    let r = reflect(-l, n);

    let ambient = ambient_strength * light_color;
    let diffuse = diffuse_strength * max(dot(n, l), 0.0) * light_color;
    let specular = specular_strength * pow(max(dot(v, r), 0.0), shininess) * light_color;

    let exposure = exp2(1.5);
    var color = (ambient + diffuse + specular) * material_color;

    // --- Distance-based factor (camera-space distance) ---
    let dist       = length(view_pos - in.world_pos);
    let FADE_NEAR  = 1.0;  // start ramping here
    let FADE_FAR   = 2.2;  // fully applied by here
    let fade       = smoothstep(FADE_NEAR, FADE_FAR, dist); // 0..1

    // --- Exposure drift with distance (sign flips by mode) ---
    // Dark mode target exposure at far: lower; Light mode target at far: higher.
    let min_exp    = 1.80; // far-end exposure multiplier in dark mode
    let max_exp    = 1.35; // far-end exposure multiplier in light mode
    let darker     = mix(1.0, min_exp, fade);   // darkens with distance
    let brighter   = mix(1.0, max_exp, fade);   // brightens with distance
    let exp_factor = select(darker, brighter, uniforms.is_light.x > 0.0);

    // Apply exposure + tonemap
    let base_exposure = exp2(1.5);
    color = aces_fitted(color * base_exposure * exp_factor);

    // --- Optional: fade to background so distant points dissolve away ---
    // Background: black in dark mode, white in light mode.
    let bg = select(vec3<f32>(0.0), vec3<f32>(1.0), uniforms.is_light.x > 0.0);
    // If you want white for BOTH modes instead, use:
    // let bg = vec3<f32>(1.0);

    color = mix(color, bg, fade);

    return vec4<f32>(color, 1.0);
}



// ACES-fit tonemap (keeps highlights nicer than Reinhard)
fn aces_fitted(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3(0.0), vec3(1.0));
}
