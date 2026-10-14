// The sky, painted before the world on the dome of the sky of the light, or in the colour of the
// fog without it; and the horizon over it, from the heights of the WDL in one draw, in the colour of
// the fog, as Noggit paints it, writing its depth; its tiles drawn in detail left out where they are
// drawn, short of the end of the fog.

// A bit for each tile drawn in detail, at `y * 64 + x`.
@group(1) @binding(0) var<uniform> drawn: array<vec4<u32>, 32>;

struct HorizonIn {
    @location(0) position: vec3<f32>,
    @location(1) tile: u32,
};

struct HorizonOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    // Whether its tile is drawn in detail.
    @location(1) @interpolate(flat) detailed: u32,
};

@vertex
fn vs_horizon(in: HorizonIn) -> HorizonOut {
    var out: HorizonOut;
    out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.world = in.position;
    let word = drawn[in.tile / 128u][(in.tile / 32u) % 4u];
    out.detailed = (word >> (in.tile % 32u)) & 1u;
    return out;
}

@fragment
fn fs_horizon(in: HorizonOut) -> @location(0) vec4<f32> {
    // A tile drawn in detail shows itself short of the end of the fog; the horizon past it, where
    // the tile is not drawn.
    if in.detailed != 0u && !beyond_fog(in.world) {
        discard;
    }
    return vec4<f32>(camera.fog_colour.rgb, 1.0);
}

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    // Where on the screen, from -1 to 1.
    @location(0) screen: vec2<f32>,
};

// A triangle over the whole view.
@vertex
fn vs_sky(@builtin(vertex_index) index: u32) -> SkyOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: SkyOut;
    out.screen = corner * 2.0 - 1.0;
    out.clip = vec4<f32>(out.screen, 0.0, 1.0);
    return out;
}

// The colour of the sky towards `towards`, in gamma, on the dome Noggit draws: between the two
// rings around its height, 90°, 18°, 10°, 3°, 0°, -30° and -90° high, by where it crosses the chord
// between them, as the faces of the dome mix the colours of their corners.
fn sky_colour(towards: vec3<f32>) -> vec3<f32> {
    var rings = array<f32, 7>(90.0, 18.0, 10.0, 3.0, 0.0, -30.0, -90.0);
    let ray = vec2<f32>(length(towards.xy), towards.z);
    var ring = 0u;
    loop {
        let below = radians(rings[ring + 1u]);
        if ring >= 5u || ray.y * cos(below) - ray.x * sin(below) >= 0.0 {
            break;
        }
        ring += 1u;
    }
    let above = vec2<f32>(cos(radians(rings[ring])), sin(radians(rings[ring])));
    let under = vec2<f32>(cos(radians(rings[ring + 1u])), sin(radians(rings[ring + 1u])));
    let across = ray.x * (under.y - above.y) - ray.y * (under.x - above.x);
    let share = clamp((ray.y * above.x - ray.x * above.y) / across, 0.0, 1.0);
    return mix(camera.sky[ring].rgb, camera.sky[min(ring + 1u, 5u)].rgb, share);
}

@fragment
fn fs_sky(in: SkyOut) -> @location(0) vec4<f32> {
    // Without the light of a map, the colour of the fog.
    if camera.sky[0].w == 0.0 {
        return vec4<f32>(camera.fog_colour.rgb, 1.0);
    }
    let towards = camera.sky_from_screen * vec4<f32>(in.screen, 1.0, 0.0);
    return vec4<f32>(sky_colour(towards.xyz), 1.0);
}
