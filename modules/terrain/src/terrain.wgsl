// The tiles of 3.3.5a, a draw each: up to four textures a chunk, taken from the arrays of the
// terrain by the codes of its tile, blended by its alpha maps, darkened by its baked shadow,
// coloured by its vertex colours, lit by the sun of the view, fogged.

// The texels of blending of the 256 chunks of a tile: three alpha maps in red, green and blue,
// the shadow in alpha; and the textures of each chunk, a code each: the slot of its array in the
// high 16 bits, its layer in the low ones, or none.
@group(1) @binding(0) var blend: texture_2d_array<f32>;
@group(1) @binding(1) var blend_sampler: sampler;
@group(1) @binding(2) var<uniform> layers: array<vec4<u32>, 256>;

@group(2) @binding(0) var array0: texture_2d_array<f32>;
@group(2) @binding(1) var array1: texture_2d_array<f32>;
@group(2) @binding(2) var array2: texture_2d_array<f32>;
@group(2) @binding(3) var array3: texture_2d_array<f32>;
@group(2) @binding(4) var array4: texture_2d_array<f32>;
@group(2) @binding(5) var array5: texture_2d_array<f32>;
@group(2) @binding(6) var array6: texture_2d_array<f32>;
@group(2) @binding(7) var array7: texture_2d_array<f32>;
@group(2) @binding(8) var array8: texture_2d_array<f32>;
@group(2) @binding(9) var array9: texture_2d_array<f32>;
@group(2) @binding(10) var array10: texture_2d_array<f32>;
@group(2) @binding(11) var array11: texture_2d_array<f32>;
@group(2) @binding(12) var layer_sampler: sampler;

const NONE: u32 = 0xffffffffu;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec4<f32>,
    @location(2) colour: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) chunk: u32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) colour: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) @interpolate(flat) chunk: u32,
    @location(4) world: vec3<f32>,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.normal = in.normal.xyz;
    out.colour = in.colour.rgb;
    out.uv = in.uv;
    out.chunk = in.chunk;
    out.world = in.position;
    return out;
}

// The texture `code` names at `uv`, with the derivatives of `uv` taken where every pixel runs:
// a texture is sampled here in a branch, which only a gradient given allows.
fn layer_colour(code: u32, uv: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec3<f32> {
    if code == NONE {
        return vec3<f32>(1.0);
    }
    let layer = i32(code & 0xffffu);
    switch code >> 16u {
        case 0u: { return textureSampleGrad(array0, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 1u: { return textureSampleGrad(array1, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 2u: { return textureSampleGrad(array2, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 3u: { return textureSampleGrad(array3, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 4u: { return textureSampleGrad(array4, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 5u: { return textureSampleGrad(array5, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 6u: { return textureSampleGrad(array6, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 7u: { return textureSampleGrad(array7, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 8u: { return textureSampleGrad(array8, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 9u: { return textureSampleGrad(array9, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 10u: { return textureSampleGrad(array10, layer_sampler, uv, layer, ddx, ddy).rgb; }
        case 11u: { return textureSampleGrad(array11, layer_sampler, uv, layer, ddx, ddy).rgb; }
        default: { return vec3<f32>(1.0); }
    }
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let weights = textureSample(blend, blend_sampler, in.uv, in.chunk);
    // A texture repeats eight times across a chunk.
    let tiled = in.uv * 8.0;
    let ddx = dpdx(tiled);
    let ddy = dpdy(tiled);
    let codes = layers[in.chunk];
    var colour = layer_colour(codes.x, tiled, ddx, ddy) * (1.0 - clamp(weights.r + weights.g + weights.b, 0.0, 1.0));
    colour += layer_colour(codes.y, tiled, ddx, ddy) * weights.r;
    colour += layer_colour(codes.z, tiled, ddx, ddy) * weights.g;
    colour += layer_colour(codes.w, tiled, ddx, ddy) * weights.b;
    let shadow = 1.0 - 0.4 * weights.a;
    // A vertex colour of 127 leaves the texture as it is.
    let lit = colour * in.colour * 2.0 * light(in.normal) * shadow;
    return vec4<f32>(fogged(lit, in.world), 1.0);
}
