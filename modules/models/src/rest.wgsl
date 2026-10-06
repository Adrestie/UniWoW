// The vertices at rest (after common.wgsl), on a device without the pool: its vertex shaders read
// no storage buffer.

struct Posed {
    position: vec3<f32>,
    normal: vec3<f32>,
};

fn first_bone(index: u32) -> u32 {
    return 0u;
}

fn posed(first: u32, position: vec3<f32>, normal: vec3<f32>, indices: vec4<u32>, weights: vec4<f32>) -> Posed {
    return Posed(position, normal);
}

fn dressed(first: u32, slot: u32, colour: vec4<f32>) -> Dressed {
    return at_rest(colour);
}
