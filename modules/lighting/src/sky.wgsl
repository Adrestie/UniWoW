// The sun, the moon and the clouds over the models of the sky (`vs_heavens`, `fs_heavens`), the
// glare of the sun and of the moon over everything (`vs_glare`, `fs_glare`), and how much of each
// body the depth of the first pass leaves in sight, which its glare follows (`cs_seen`). Each body
// is a quad facing the eye, `DISTANCE` yards from it, at infinity. The heavens are painted before
// the world; the glare is drawn last, where the depth is still that of the clearing; neither writes
// the depth.

struct Heavens {
    view_proj: mat4x4<f32>,
    // From a place on the screen to the direction it is seen in from the eye.
    sky_from_screen: mat4x4<f32>,
    // The axes of the camera, across, up and back; how much of the sky the sun, the moon and the
    // clouds are drawn over, in `across.w`.
    across: vec4<f32>,
    up: vec4<f32>,
    back: vec4<f32>,
    // Towards the sun, the side of its quad; its colour in gamma and its alpha. The moon's alike.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    moon: vec4<f32>,
    moon_colour: vec4<f32>,
    // The side of the quad of each glare, and its alpha before its envelope.
    glares: array<vec4<f32>, 2>,
    // What the envelope of the glare of the sun, then of the moon, goes towards before the share of
    // the body in sight; how fast each rises, a second.
    flare: vec4<f32>,
    // How fast an envelope falls, a second; the seconds since the frame before.
    fall: vec4<f32>,
    // The colour of a flash of lightning, in gamma, and how far it turns the clouds to it.
    lightning: vec4<f32>,
};
@group(0) @binding(0) var<uniform> heavens: Heavens;

const DISTANCE: f32 = 12.0;
const PI: f32 = 3.14159265;

// The radius of the tile at a ring of the dome of the clouds (`clouds::on_dome`).
const RING_RADIUS: f32 = 0.041666667;

struct ScreenOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) screen: vec2<f32>,
};

// A triangle over the whole view.
@vertex
fn vs_heavens(@builtin(vertex_index) index: u32) -> ScreenOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: ScreenOut;
    out.screen = corner * 2.0 - 1.0;
    out.clip = vec4<f32>(out.screen, 0.0, 1.0);
    return out;
}

// Where the quad of a body towards `body.xyz`, of side `body.w`, is crossed by the ray `towards`:
// its coordinates on the quad, outside 0 to 1 where it misses it.
fn on_quad(towards: vec3<f32>, body: vec4<f32>) -> vec2<f32> {
    let forward = -heavens.back.xyz;
    let centre = body.xyz * DISTANCE;
    let facing = dot(towards, forward);
    if facing <= 1e-4 || dot(body.xyz, forward) <= 0.0 || body.w <= 0.0 {
        return vec2<f32>(-1.0);
    }
    let hit = towards * (dot(centre, forward) / facing) - centre;
    return vec2<f32>(dot(hit, heavens.across.xyz) / body.w + 0.5, 0.5 - dot(hit, heavens.up.xyz) / body.w);
}

// The alpha the client gives a corner of the quad of a body `height` yards over the eye, 12 yards
// from it (`0x6d1960`): 2.5 times its height within the 0.4 yard over the horizon, the alpha of the
// body `alpha` above.
fn corner_alpha(height: f32, alpha: f32) -> f32 {
    let ramp = 2.5 * height;
    return select(alpha, clamp(ramp, 0.0, 1.0), ramp < 1.0);
}

// The body of quad `body` seen at `place` on it, premultiplied, in gamma: its texel tinted by its
// colour; its alpha that of the corners of the quad as the client gives them and interpolates them,
// from its lowest, or the horizon where it cuts the quad, none under it, to its highest, by the
// height of the point seen. The camera never rolls: across the quad, the height stays.
fn body_seen(body: vec4<f32>, place: vec2<f32>, texel: vec4<f32>, tint: vec4<f32>) -> vec4<f32> {
    let inside = all(place >= vec2<f32>(0.0)) && all(place <= vec2<f32>(1.0));
    let centre = body.z * DISTANCE;
    let reach = 0.5 * body.w * abs(heavens.up.z);
    let height = centre - body.w * heavens.up.z * (place.y - 0.5);
    let lowest = max(centre - reach, 0.0);
    let highest = centre + reach;
    let share = clamp((height - lowest) / max(highest - lowest, 1e-6), 0.0, 1.0);
    let faded = mix(corner_alpha(lowest, tint.a), corner_alpha(highest, tint.a), share);
    let alpha = select(0.0, texel.a * faded, inside);
    return vec4<f32>(tint.rgb * texel.rgb * alpha, alpha);
}

// Where `towards` meets the dome of the clouds: the place on the tile, and the alpha of its rings
// there; none under the horizon. The co-latitudes of the rings, in turns of half a circle, and
// their alphas, as `clouds::on_dome` gives them.
fn on_dome(towards: vec3<f32>) -> vec3<f32> {
    var rings = array<f32, 12>(0.0, 0.025, 0.05, 0.075, 0.1, 0.125, 0.15, 0.175, 0.205, 0.23, 0.245, 0.25);
    var ring_alphas = array<f32, 12>(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.50196078, 0.0, 0.0);
    let lowered = 0.70710678;
    let along = -lowered * towards.z + sqrt(lowered * lowered * towards.z * towards.z + 1.0 - lowered * lowered);
    let turns = acos(clamp(along * towards.z + lowered, -1.0, 1.0)) / PI;
    var ring = 11.0;
    var alpha = 0.0;
    for (var at = 0u; at < 11u; at++) {
        if turns <= rings[at + 1u] {
            let share = clamp((turns - rings[at]) / (rings[at + 1u] - rings[at]), 0.0, 1.0);
            ring = f32(at) + share;
            alpha = mix(ring_alphas[at], ring_alphas[at + 1u], share);
            break;
        }
    }
    let across = length(towards.xy);
    var horizontal = vec2<f32>(0.0);
    if across > 1e-6 {
        horizontal = towards.xy / across;
    }
    return vec3<f32>(vec2<f32>(0.5) + horizontal * ring * RING_RADIUS, select(0.0, alpha, towards.z > 0.0));
}

@group(0) @binding(1) var sun_texture: texture_2d<f32>;
@group(0) @binding(2) var moon_texture: texture_2d<f32>;
@group(0) @binding(3) var clouds_texture: texture_2d<f32>;
@group(0) @binding(4) var sky_sampler: sampler;

// The sun, the moon over it and the clouds over both, mixed in gamma as the client blends them,
// faded by the share of the sky they are drawn over; given premultiplied. The clouds
// turned to the colour of a flash of lightning as the sky is.
@fragment
fn fs_heavens(in: ScreenOut) -> @location(0) vec4<f32> {
    let towards = normalize((heavens.sky_from_screen * vec4<f32>(in.screen, 1.0, 0.0)).xyz);
    let sun_place = on_quad(towards, heavens.sun);
    let sun = body_seen(
        heavens.sun,
        sun_place,
        textureSampleLevel(sun_texture, sky_sampler, clamp(sun_place, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0),
        heavens.sun_colour,
    );
    let moon_place = on_quad(towards, heavens.moon);
    let moon = body_seen(
        heavens.moon,
        moon_place,
        textureSampleLevel(moon_texture, sky_sampler, clamp(moon_place, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0),
        heavens.moon_colour,
    );
    let dome = on_dome(towards);
    let texel = textureSampleLevel(clouds_texture, sky_sampler, dome.xy, 0.0);
    let cloud_alpha = texel.a * dome.z;
    let cloud = mix(texel.rgb, heavens.lightning.rgb, heavens.lightning.a);
    var colour = sun;
    colour = moon + colour * (1.0 - moon.a);
    colour = vec4<f32>(cloud * cloud_alpha, cloud_alpha) + colour * (1.0 - cloud_alpha);
    return colour * heavens.across.w;
}

// The envelopes of the glares, the sun's then the moon's, kept from frame to frame.
@group(0) @binding(5) var<storage, read> envelopes: array<f32, 2>;
@group(0) @binding(6) var<storage, read_write> slewed: array<f32, 2>;
@group(0) @binding(7) var sun_glare: texture_2d<f32>;
@group(0) @binding(8) var moon_glare: texture_2d<f32>;
@group(0) @binding(9) var depth_seen: texture_2d<f32>;

struct GlareOut {
    @builtin(position) @invariant clip: vec4<f32>,
    @location(0) place: vec2<f32>,
    @location(1) @interpolate(flat) body: u32,
};

// The quad of the glare of the body `instance`, 0 the sun, 1 the moon, at infinity.
@vertex
fn vs_glare(@builtin(vertex_index) index: u32, @builtin(instance_index) instance: u32) -> GlareOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    let corner = corners[index];
    let body = select(heavens.sun, heavens.moon, instance == 1u);
    let side = heavens.glares[instance].x;
    let point = body.xyz * DISTANCE
        + (heavens.across.xyz * (corner.x - 0.5) - heavens.up.xyz * (corner.y - 0.5)) * side;
    var out: GlareOut;
    let clip = heavens.view_proj * vec4<f32>(point, 0.0);
    out.clip = vec4<f32>(clip.xy, 0.0, clip.w);
    out.place = corner;
    out.body = instance;
    return out;
}

// The glare added to what is drawn, in the colour of its body, by its alpha and its envelope.
@fragment
fn fs_glare(in: GlareOut) -> @location(0) vec4<f32> {
    let sun = textureSampleLevel(sun_glare, sky_sampler, in.place, 0.0);
    let moon = textureSampleLevel(moon_glare, sky_sampler, in.place, 0.0);
    let texel = select(sun, moon, in.body == 1u);
    let tint = select(heavens.sun_colour, heavens.moon_colour, in.body == 1u);
    let alpha = texel.a * heavens.glares[in.body].y * envelopes[in.body];
    return vec4<f32>(tint.rgb * texel.rgb * alpha, 0.0);
}

var<workgroup> seen: atomic<u32>;

// How much of the quad of the body `group.x` is in sight, by 8 × 8 points of it tested against the
// depth of the first pass: on the screen, over the horizon and at infinity; its envelope moved
// towards what it goes towards times that share, by how fast it rises or falls (`0x6cf5ea`).
@compute @workgroup_size(64)
fn cs_seen(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) index: u32) {
    if index == 0u {
        atomicStore(&seen, 0u);
    }
    workgroupBarrier();
    let body_index = group.x;
    let body = select(heavens.sun, heavens.moon, body_index == 1u);
    let offset = (vec2<f32>(f32(index % 8u), f32(index / 8u)) + vec2<f32>(0.5)) / 8.0 - vec2<f32>(0.5);
    let point = body.xyz * DISTANCE + (heavens.across.xyz * offset.x - heavens.up.xyz * offset.y) * body.w;
    let clip = heavens.view_proj * vec4<f32>(point, 0.0);
    var visible = false;
    if clip.w > 0.0 && point.z > 0.0 && body.w > 0.0 {
        let screen = clip.xy / clip.w;
        if all(abs(screen) <= vec2<f32>(1.0)) {
            let size = vec2<f32>(textureDimensions(depth_seen, 0));
            let pixel = min((vec2<f32>(screen.x, -screen.y) * 0.5 + vec2<f32>(0.5)) * size, size - vec2<f32>(1.0));
            visible = textureLoad(depth_seen, vec2<u32>(pixel), 0).r <= 0.0;
        }
    }
    if visible {
        atomicAdd(&seen, 1u);
    }
    workgroupBarrier();
    if index == 0u {
        let share = f32(atomicLoad(&seen)) / 64.0;
        let goal = heavens.flare[body_index] * share;
        let kept = slewed[body_index];
        let seconds = heavens.fall.y;
        slewed[body_index] = kept + clamp(goal - kept, -heavens.fall.x * seconds, heavens.flare[2u + body_index] * seconds);
    }
}
