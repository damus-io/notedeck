struct VertexOut {
    @location(0) color: vec4<f32>,
    @builtin(position) position: vec4<f32>,
};

struct Uniforms {
    @size(16) angle: f32, // pad to 16 bytes
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) v_idx: u32) -> VertexOut {
    // Cube vertices hardcoded in the shader
    var positions = array<vec3<f32>, 8>(
        vec3<f32>(-0.5, -0.5, 0.5),  // front bottom left
        vec3<f32>(0.5, -0.5, 0.5),   // front bottom right
        vec3<f32>(0.5, 0.5, 0.5),    // front top right
        vec3<f32>(-0.5, 0.5, 0.5),   // front top left
        vec3<f32>(-0.5, -0.5, -0.5), // back bottom left
        vec3<f32>(0.5, -0.5, -0.5),  // back bottom right
        vec3<f32>(0.5, 0.5, -0.5),   // back top right
        vec3<f32>(-0.5, 0.5, -0.5)   // back top left
    );
    
    // Cube indices hardcoded in the shader
    var indices = array<u32, 36>(
        // front face
        0, 1, 2, 2, 3, 0,
        // back face
        4, 5, 6, 6, 7, 4,
        // right face
        1, 5, 6, 6, 2, 1,
        // left face
        0, 4, 7, 7, 3, 0,
        // top face
        3, 2, 6, 6, 7, 3,
        // bottom face
        0, 1, 5, 5, 4, 0
    );
    
    var out: VertexOut;
    var idx = indices[v_idx];
    var pos = positions[idx];
    
    // simple rotation around Y axis
    var cosA = cos(uniforms.angle);
    var sinA = sin(uniforms.angle);
    var rotated_x = pos.x * cosA + pos.z * sinA;
    var rotated_z = -pos.x * sinA + pos.z * cosA;
    
    // With proper perspective transformation:
    var z_pos = rotated_z - 2.0;  // Move cube away from camera
    var w = -z_pos;  // Set w to -z for perspective division
    out.position = vec4<f32>(rotated_x, pos.y, rotated_z, w);
    
    // simple white shading based on position
    var shade = 0.5 + 0.5 * (rotated_z + pos.y);
    out.color = vec4<f32>(shade, shade, shade, 1.0);
    
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
