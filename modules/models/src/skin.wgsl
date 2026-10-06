// The vertices posed by their bones (after common.wgsl), as the thread of the animations wrote
// them for the frame.

// For each instance of the frame, its first bone plus one; 0 at rest.
@group(2) @binding(0) var<storage, read> bone_table: array<u32>;
// The bones, each the first three rows of its matrix.
@group(2) @binding(1) var<storage, read> bones: array<vec4<f32>>;

struct Posed {
    position: vec3<f32>,
    normal: vec3<f32>,
};

// The first bone, plus one, of the instance `index` of the frame.
fn first_bone(index: u32) -> u32 {
    return bone_table[index];
}

// The vertex at `position` with `normal` posed by the bones from `first`, plus one, each of its
// four `indices` by its share of their `weights`; at rest for 0 or without weights.
fn posed(first: u32, position: vec3<f32>, normal: vec3<f32>, indices: vec4<u32>, weights: vec4<f32>) -> Posed {
    let total = dot(weights, vec4<f32>(1.0));
    if first == 0u || total <= 0.0 {
        return Posed(position, normal);
    }
    var rows = array<vec4<f32>, 3>(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    for (var which = 0u; which < 4u; which++) {
        let weight = weights[which] / total;
        if weight > 0.0 {
            let at = (first - 1u + indices[which]) * 3u;
            rows[0] += bones[at] * weight;
            rows[1] += bones[at + 1u] * weight;
            rows[2] += bones[at + 2u] * weight;
        }
    }
    let point = vec4<f32>(position, 1.0);
    let direction = vec4<f32>(normal, 0.0);
    return Posed(
        vec3<f32>(dot(rows[0], point), dot(rows[1], point), dot(rows[2], point)),
        vec3<f32>(dot(rows[0], direction), dot(rows[1], direction), dot(rows[2], direction)),
    );
}
