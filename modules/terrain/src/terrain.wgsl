// The terrain of 3.3.5a: up to four textures a chunk, blended by its alpha maps, darkened by its
// baked shadow, coloured by its vertex colours, lit by a fixed sun until the lights of the map come.

struct Camera {
    view_proj: mat4x4<f32>,
    sun: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// The texels of blending of the 256 chunks of a tile: three alpha maps in red, green and blue,
// the shadow in alpha.
@group(1) @binding(0) var blend: texture_2d_array<f32>;
@group(1) @binding(1) var blend_sampler: sampler;

@group(2) @binding(0) var layer0: texture_2d<f32>;
@group(2) @binding(1) var layer1: texture_2d<f32>;
@group(2) @binding(2) var layer2: texture_2d<f32>;
@group(2) @binding(3) var layer3: texture_2d<f32>;
@group(2) @binding(4) var layer_sampler: sampler;

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
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.normal = in.normal.xyz;
    out.colour = in.colour.rgb;
    out.uv = in.uv;
    out.chunk = in.chunk;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let weights = textureSample(blend, blend_sampler, in.uv, in.chunk);
    // A texture repeats eight times across a chunk.
    let tiled = in.uv * 8.0;
    var colour = textureSample(layer0, layer_sampler, tiled).rgb * (1.0 - clamp(weights.r + weights.g + weights.b, 0.0, 1.0));
    colour += textureSample(layer1, layer_sampler, tiled).rgb * weights.r;
    colour += textureSample(layer2, layer_sampler, tiled).rgb * weights.g;
    colour += textureSample(layer3, layer_sampler, tiled).rgb * weights.b;
    let light = 0.45 + 0.55 * max(dot(normalize(in.normal), camera.sun.xyz), 0.0);
    let shadow = 1.0 - 0.4 * weights.a;
    // A vertex colour of 127 leaves the texture as it is.
    return vec4<f32>(colour * in.colour * 2.0 * light * shadow, 1.0);
}
