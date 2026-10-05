// The models: a batch of a model for every instance of a group, its one or two textures combined
// in gamma by the pixel shader WotLK chose for it, with its colour and transparency at rest and
// the instance's alpha; then lit by the sun and fogged as the view says, in its linear space.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour, and the light everywhere.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    // The eye, and how far an instance is drawn: this many times its radius.
    eye: vec4<f32>,
    // The colour of the fog; where it starts, its middle and where it covers all, on the ground.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
    // The axes of the camera, across, up and back.
    across: vec4<f32>,
    up: vec4<f32>,
    back: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct Batch {
    // Its colour and transparency at rest, its weight included.
    colour: vec4<f32>,
    // Its alpha key, the share of its alpha under which a pixel is not drawn, 0 for none; 1 when
    // unlit; 1 when unfogged; the colour of its fog: 0 the view's, 1 black, 2 white, 3 grey.
    flags: vec4<f32>,
    // The radius of the model.
    model: vec4<f32>,
    // Its pixel shader, and where its first and second textures take their coordinates: 0 the
    // first set, 1 the second, 2 the environment.
    combine: vec4<u32>,
};
@group(1) @binding(0) var<uniform> batch: Batch;
@group(1) @binding(1) var first: texture_2d<f32>;
@group(1) @binding(2) var first_sampler: sampler;
@group(1) @binding(3) var second: texture_2d<f32>;
@group(1) @binding(4) var second_sampler: sampler;

// The share of the fog at its middle; the least radius an instance's reach counts; the alpha under
// which a pixel of a batch without an alpha key is not drawn.
const NEAR_FOG: f32 = 0.55;
const LEAST_RADIUS: f32 = 1.0;
const LEAST_ALPHA: f32 = 1.0 / 255.0;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv1: vec2<f32>,
    @location(3) uv2: vec2<f32>,
    // The instance: the rows of its transform, then its alpha.
    @location(4) row0: vec4<f32>,
    @location(5) row1: vec4<f32>,
    @location(6) row2: vec4<f32>,
    @location(7) extra: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv1: vec2<f32>,
    @location(2) uv2: vec2<f32>,
    @location(3) env: vec2<f32>,
    @location(4) world: vec3<f32>,
    @location(5) alpha: f32,
};

// The coordinates of the environment, as WotLK maps them (wowdev, M2/.skin): the position and the
// normal in the space of the camera, its depth growing away from the eye.
fn sphere_map(position: vec3<f32>, normal: vec3<f32>) -> vec2<f32> {
    let towards = position - camera.eye.xyz;
    let vertex = vec3<f32>(
        dot(towards, camera.across.xyz),
        dot(towards, camera.up.xyz),
        -dot(towards, camera.back.xyz)
    );
    let turned = normalize(vec3<f32>(
        dot(normal, camera.across.xyz),
        dot(normal, camera.up.xyz),
        -dot(normal, camera.back.xyz)
    ));
    let from_eye = -normalize(vertex);
    var reflected = from_eye - turned * (2.0 * dot(from_eye, turned));
    reflected.z = reflected.z + 1.0;
    return normalize(reflected).xy * 0.5 + vec2<f32>(0.5);
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let position = vec4<f32>(in.position, 1.0);
    let world = vec3<f32>(dot(in.row0, position), dot(in.row1, position), dot(in.row2, position));
    let origin = vec3<f32>(in.row0.w, in.row1.w, in.row2.w);
    let scale = length(vec3<f32>(in.row0.x, in.row1.x, in.row2.x));
    let reach = camera.eye.w * max(batch.model.x * scale, LEAST_RADIUS);
    var out: VertexOut;
    if distance(origin, camera.eye.xyz) > reach || in.extra.x <= 0.0 {
        // Beyond its reach, or unseen: every vertex at one point, its triangles draw nothing.
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    }
    let normal = vec4<f32>(in.normal, 0.0);
    out.normal = vec3<f32>(dot(in.row0, normal), dot(in.row1, normal), dot(in.row2, normal));
    out.uv1 = in.uv1;
    out.uv2 = in.uv2;
    out.env = sphere_map(world, out.normal);
    out.world = world;
    out.alpha = in.extra.x;
    return out;
}

fn fog_amount(position: vec3<f32>) -> f32 {
    let distance = length(position.xy - camera.eye.xy);
    return NEAR_FOG * smoothstep(camera.fog.x, camera.fog.y, distance)
        + (1.0 - NEAR_FOG) * smoothstep(camera.fog.y, camera.fog.z, distance);
}

// The colour of the fog of a batch but a mod2x one: the view's, black, or white.
fn fog_colour(mode: f32) -> vec3<f32> {
    if mode > 1.5 {
        return vec3<f32>(1.0);
    }
    if mode > 0.5 {
        return vec3<f32>(0.0);
    }
    return camera.fog_colour.rgb;
}

fn coordinates(source: u32, in: VertexOut) -> vec2<f32> {
    if source == 2u {
        return in.env;
    }
    if source == 1u {
        return in.uv2;
    }
    return in.uv1;
}

// The pixel shaders of WotLK (wowdev, M2/Rendering), by their number in `shaders.rs`: `colour`
// coming in, `one` and `two` the textures.
fn combine(shader: u32, colour: vec4<f32>, one: vec4<f32>, two: vec4<f32>) -> vec4<f32> {
    switch shader {
        case 0u: { return vec4<f32>(colour.rgb * one.rgb, colour.a); }
        case 1u: { return colour * one; }
        case 2u: { return vec4<f32>(mix(colour.rgb, one.rgb, colour.a), colour.a); }
        case 3u: { return colour + one; }
        case 4u: { return colour * one * 2.0; }
        case 5u: { return vec4<f32>(mix(one.rgb, colour.rgb, colour.a), colour.a); }
        case 6u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb, colour.a); }
        case 7u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb, colour.a * two.a); }
        case 8u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb, colour.a + two.a); }
        case 9u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb * 2.0, colour.a * two.a * 2.0); }
        case 10u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb * 2.0, colour.a); }
        case 11u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb, colour.a); }
        case 12u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb, colour.a * one.a); }
        case 13u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb, colour.a * one.a + two.a); }
        case 14u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb * 2.0, colour.a * one.a * two.a * 2.0); }
        case 15u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb * 2.0, colour.a * one.a); }
        case 16u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb, colour.a * one.a); }
        case 17u: { return colour * one * two; }
        case 18u: { return (colour + one) * two; }
        case 19u: { return vec4<f32>(colour.rgb * one.rgb * two.rgb * 4.0, one.a * two.a * 4.0); }
        case 20u: {
            return vec4<f32>(colour.rgb * one.rgb * mix(two.rgb * 2.0, vec3<f32>(1.0), one.a), colour.a);
        }
        case 21u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb * two.a, colour.a); }
        case 22u: { return vec4<f32>(colour.rgb * one.rgb + two.rgb * two.a * one.a, colour.a); }
        default: { return vec4<f32>(colour.rgb * one.rgb, colour.a); }
    }
}

// The linear value of a value in gamma, as an sRGB target encodes it back.
fn linear(gamma: vec3<f32>) -> vec3<f32> {
    let low = gamma / 12.92;
    let high = pow((gamma + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, gamma <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Both textures sampled where every pixel runs, their coordinates chosen first; in gamma, as
    // WotLK combines them.
    let one = textureSample(first, first_sampler, coordinates(batch.combine.y, in));
    let two = textureSample(second, second_sampler, coordinates(batch.combine.z, in));
    let element = batch.colour.a * in.alpha;
    let combined = combine(batch.combine.x, vec4<f32>(batch.colour.rgb, element), one, two);
    // As WotLK tests it: against the alpha key times the alpha of the batch, or 1/255.
    let alpha = clamp(combined.a, 0.0, 1.0);
    let reference = select(LEAST_ALPHA, batch.flags.x * element, batch.flags.x > 0.0);
    if alpha < reference {
        discard;
    }
    let fog = select(fog_amount(in.world), 0.0, batch.flags.z > 0.5);
    // A mod2x batch doubles what is drawn in gamma: its colour, grey in the fog, made such that
    // the target's own doubling in linear gives the same, as far as a colour of 1 reaches.
    if batch.flags.w > 2.5 {
        let gamma = mix(combined.rgb, vec3<f32>(0.5), fog);
        return vec4<f32>(pow(gamma * 2.0, vec3<f32>(2.2)) * 0.5, alpha);
    }
    // In the linear space of the view, as the terrain: lit, then fogged.
    var rgb = linear(clamp(combined.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    if batch.flags.y < 0.5 {
        let lit = max(dot(normalize(in.normal), camera.sun.xyz), 0.0);
        rgb = rgb * (camera.ambient.rgb + camera.sun_colour.rgb * lit);
    }
    return vec4<f32>(mix(rgb, fog_colour(batch.flags.w), fog), alpha);
}
