// The triangles of a spell effect: meshes skinned, particles and ribbons, built on the CPU in the
// space of the model; textured, tinted by their colour, blended as the M2 says.

struct Globals {
    view_proj: mat4x4<f32>,
    // From the model to the world: where the effect stands.
    model: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var sampled: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = globals.view_proj * globals.model * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

fn texel(in: VertexOut) -> vec4<f32> {
    return textureSample(image, sampled, in.uv) * in.color;
}

// 0: opaque.
@fragment
fn fs_opaque(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(texel(in).rgb, 1.0);
}

// 1: alpha key, cut at 224 / 255 as the client's.
@fragment
fn fs_key(in: VertexOut) -> @location(0) vec4<f32> {
    let c = texel(in);
    if c.a < 0.878 {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}

// 2 alpha, 3 additive without alpha, 4 additive: the blending does the rest.
@fragment
fn fs_blend(in: VertexOut) -> @location(0) vec4<f32> {
    return texel(in);
}

// 5: modulate, faded toward white by the alpha.
@fragment
fn fs_mod(in: VertexOut) -> @location(0) vec4<f32> {
    let c = texel(in);
    return vec4<f32>(mix(vec3<f32>(1.0), c.rgb, c.a), 1.0);
}

// 6: modulate 2x, faded toward grey (which keeps what is under it) by the alpha.
@fragment
fn fs_mod2x(in: VertexOut) -> @location(0) vec4<f32> {
    let c = texel(in);
    return vec4<f32>(mix(vec3<f32>(0.5), c.rgb, c.a), 1.0);
}
