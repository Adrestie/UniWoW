// The horizon, from the heights of the WDL in one draw, its tiles drawn in detail left out; and
// the sky behind everything, in the colour of the fog.

// A bit for each tile drawn in detail, at `y * 64 + x`.
@group(1) @binding(0) var<uniform> drawn: array<vec4<u32>, 32>;

// The ground of the horizon, before its light and its fog.
const GROUND: vec3<f32> = vec3<f32>(0.20, 0.19, 0.15);

struct HorizonIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec4<f32>,
    @location(2) tile: u32,
};

struct HorizonOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world: vec3<f32>,
    // How much it fades into the fog near a tile without heights.
    @location(2) fade: f32,
};

@vertex
fn vs_horizon(in: HorizonIn) -> HorizonOut {
    var out: HorizonOut;
    out.normal = in.normal.xyz;
    out.world = in.position;
    out.fade = in.normal.w;
    let word = drawn[in.tile / 128u][(in.tile / 32u) % 4u];
    if ((word >> (in.tile % 32u)) & 1u) != 0u {
        // Every vertex of the tile at one point: its triangles draw nothing.
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    }
    return out;
}

@fragment
fn fs_horizon(in: HorizonOut) -> @location(0) vec4<f32> {
    let amount = max(fog_amount(in.world), in.fade);
    return vec4<f32>(mix(GROUND * light(in.normal), camera.fog.rgb, amount), 1.0);
}

struct SkyOut {
    // The same depth on every machine, which the test of the depth for equality needs.
    @builtin(position) @invariant clip: vec4<f32>,
};

// A triangle over the whole view, at infinity, where the depth is 0.
@vertex
fn vs_sky(@builtin(vertex_index) index: u32) -> SkyOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: SkyOut;
    out.clip = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_sky() -> @location(0) vec4<f32> {
    return vec4<f32>(camera.fog.rgb, 1.0);
}
