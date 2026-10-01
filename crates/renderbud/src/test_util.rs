//! Test fixtures: a glTF binary built in code, so tests that need a model
//! don't depend on a committed blob of unknown origin.

/// A unit cube (corners at ±0.5) as a `.glb`: 24 vertices with flat normals,
/// 12 triangles and one material of `base_color` (linear RGBA).
pub fn cube_glb(base_color: [f32; 4]) -> Vec<u8> {
    // Each face: its normal and the two in-plane axes, wound so the
    // triangles face outward.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ];

    let mut positions: Vec<f32> = Vec::with_capacity(24 * 3);
    let mut normals: Vec<f32> = Vec::with_capacity(24 * 3);
    let mut indices: Vec<u16> = Vec::with_capacity(36);
    for (n, u, v) in faces {
        let base = (positions.len() / 3) as u16;
        for (su, sv) in [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
            for k in 0..3 {
                positions.push(n[k] * 0.5 + u[k] * su + v[k] * sv);
            }
            normals.extend_from_slice(&n);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let mut bin: Vec<u8> = Vec::new();
    bin.extend(positions.iter().flat_map(|f| f.to_le_bytes()));
    let normals_offset = bin.len();
    bin.extend(normals.iter().flat_map(|f| f.to_le_bytes()));
    let indices_offset = bin.len();
    bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
    let indices_len = bin.len() - indices_offset;
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let [r, g, b, a] = base_color;
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2,"material":0}}]}}],"materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[{r},{g},{b},{a}],"metallicFactor":0.0,"roughnessFactor":0.6}}}}],"buffers":[{{"byteLength":{bin_len}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{normals_offset},"target":34962}},{{"buffer":0,"byteOffset":{normals_offset},"byteLength":{normals_len},"target":34962}},{{"buffer":0,"byteOffset":{indices_offset},"byteLength":{indices_len},"target":34963}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":24,"type":"VEC3","min":[-0.5,-0.5,-0.5],"max":[0.5,0.5,0.5]}},{{"bufferView":1,"componentType":5126,"count":24,"type":"VEC3"}},{{"bufferView":2,"componentType":5123,"count":36,"type":"SCALAR"}}]}}"#,
        bin_len = bin.len(),
        normals_len = indices_offset - normals_offset,
    );
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }

    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb: Vec<u8> = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    glb
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cube parses as glTF with one triangle primitive of 12 triangles,
    /// every index in range and the material colour it was given.
    #[test]
    fn cube_glb_parses_as_a_twelve_triangle_mesh() {
        let (doc, buffers, _) = gltf::import_slice(cube_glb([1.0, 0.5, 0.25, 1.0])).unwrap();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        assert_eq!(prim.mode(), gltf::mesh::Mode::Triangles);
        let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
        assert_eq!(reader.read_positions().unwrap().count(), 24);
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices.len(), 36);
        assert!(indices.iter().all(|&i| i < 24));
        let color = prim.material().pbr_metallic_roughness().base_color_factor();
        assert_eq!(color, [1.0, 0.5, 0.25, 1.0]);
    }
}
