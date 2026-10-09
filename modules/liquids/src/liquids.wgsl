// The liquids: each vertex its place in the world, its coordinates, its depth and the slot of its
// type in the table; the frame of its texture turning with the time, one every 60 ms, as Noggit
// draws them (read for the facts only). Water: its texel added to its colour, near where shallow
// and far where deep, its alpha so; its coordinates scaled and turned by its animation. Magma and
// slime: their texel brightened, opaque and unlit, their coordinates running by their animation.
// Then fogged. The colours of the water and the gain of the magma, until the light of the map gives
// them (9.7), are fixed values taken from captures of the game at noon. The bindings of the arrays
// and `sampled` follow, written for the count of slots.

struct Camera {
    view_proj: mat4x4<f32>,
    // The eye, and the time of the frame in seconds.
    eye: vec4<f32>,
    // The colour of the fog; where it starts, its middle and where it covers all, and the rate of
    // the game's fog, 0 for the editor's.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct LiquidType {
    // The codes of its frames (the slot in the high 16 bits, the layer in the low ones).
    frames: array<vec4<u32>, 8>,
    // How many frames, whether water, its material.
    info: vec4<u32>,
    // The two numbers of its animation.
    animation: vec4<f32>,
};
@group(1) @binding(0) var<storage, read> types: array<LiquidType>;
@group(1) @binding(1) var liquid_sampler: sampler;

const NONE: u32 = 0xFFFFFFFFu;
// The colour of the water, shallow and deep, in gamma, its alpha so.
const SHALLOW: vec4<f32> = vec4<f32>(0.24, 0.35, 0.31, 0.65);
const DEEP: vec4<f32> = vec4<f32>(0.18, 0.28, 0.27, 0.9);
// How much brighter than its texture the game draws the magma and slime, in gamma.
const MAGMA_GAIN: f32 = 1.35;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) depth: f32,
    @location(3) slot: u32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) depth: f32,
    @location(2) world: vec3<f32>,
    @location(3) @interpolate(flat) slot: u32,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    out.depth = in.depth;
    out.world = in.position;
    out.slot = in.slot;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let kind = types[in.slot];
    let milliseconds = camera.eye.w * 1000.0;
    let count = max(kind.info.x, 1u);
    let frame = u32(ceil(milliseconds / 60.0)) % count;
    let code = kind.frames[frame / 4u][frame % 4u];
    let water = kind.info.y != 0u;
    var uv = in.uv;
    if water {
        let turn = radians(kind.animation.y);
        let scaled = in.uv * kind.animation.x;
        uv = vec2<f32>(
            cos(turn) * scaled.x - sin(turn) * scaled.y,
            sin(turn) * scaled.x + cos(turn) * scaled.y
        );
    } else {
        uv = in.uv + kind.animation.xy * milliseconds / 2880.0;
    }
    let ddx = dpdx(uv);
    let ddy = dpdy(uv);
    var texel = vec4<f32>(1.0);
    if code != NONE {
        texel = sampled(code >> 16u, i32(code & 0xFFFFu), uv, ddx, ddy);
    }
    var gamma = min(texel.rgb * MAGMA_GAIN, vec3<f32>(1.0));
    var alpha = 1.0;
    if water {
        let colour = mix(SHALLOW, DEEP, clamp(in.depth, 0.0, 1.0));
        gamma = clamp(texel.rgb + colour.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        alpha = colour.a;
    }
    let rgb = fog_mix(linear(gamma), camera.fog_colour.rgb, fog_amount(in.world));
    return vec4<f32>(rgb, alpha);
}
