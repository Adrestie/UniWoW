// What the shaders of the models share: the camera, a vertex placed by its instance, the
// environment mapped as WotLK maps it, the pixel shaders of WotLK, which combine the textures in
// gamma, and the end of a pixel: its alpha tested as WotLK tests it, then lit by the sun in the
// linear space of the view and fogged as the view says.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour on the ground and the light everywhere, in gamma.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    // The eye, and how far an instance is drawn: this many times its radius.
    eye: vec4<f32>,
    // The colour of the fog; where it starts, its middle and where it covers all, and the rate of
    // the game's fog, 0 for the editor's.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
    // The axes of the camera, across, up and back.
    across: vec4<f32>,
    up: vec4<f32>,
    back: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// The least radius an instance's reach counts; the alpha under which a pixel of a batch without an
// alpha key is not drawn.
const LEAST_RADIUS: f32 = 1.0;
const LEAST_ALPHA: f32 = 1.0 / 255.0;

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

// A vertex placed in the world.
struct Placed {
    clip: vec4<f32>,
    world: vec3<f32>,
    normal: vec3<f32>,
    env: vec2<f32>,
};

// The vertex at `position` with `normal` of a model of `radius`, placed by the rows of its
// instance's transform: at one point with every other, drawing nothing, when its instance is
// beyond the reach of its size or of `alpha` 0.
fn place(
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    alpha: f32,
    radius: f32,
    position: vec3<f32>,
    normal: vec3<f32>,
) -> Placed {
    let at = vec4<f32>(position, 1.0);
    let world = vec3<f32>(dot(row0, at), dot(row1, at), dot(row2, at));
    let origin = vec3<f32>(row0.w, row1.w, row2.w);
    let scale = length(vec3<f32>(row0.x, row1.x, row2.x));
    let reach = camera.eye.w * max(radius * scale, LEAST_RADIUS);
    var placed: Placed;
    if distance(origin, camera.eye.xyz) > reach || alpha <= 0.0 {
        placed.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        placed.clip = camera.view_proj * vec4<f32>(world, 1.0);
    }
    let direction = vec4<f32>(normal, 0.0);
    placed.normal = vec3<f32>(dot(row0, direction), dot(row1, direction), dot(row2, direction));
    placed.env = sphere_map(world, placed.normal);
    placed.world = world;
    return placed;
}

// What the material of an instance is at the moment of its animation (`dressed`): its colour, and
// the rows of the transforms of the coordinates of its two textures.
struct Dressed {
    colour: vec4<f32>,
    one_u: vec3<f32>,
    one_v: vec3<f32>,
    two_u: vec3<f32>,
    two_v: vec3<f32>,
};

// The material of `colour` at rest, its coordinates unmoved.
fn at_rest(colour: vec4<f32>) -> Dressed {
    let u = vec3<f32>(1.0, 0.0, 0.0);
    let v = vec3<f32>(0.0, 1.0, 0.0);
    return Dressed(colour, u, v, u, v);
}

// The coordinates `source` names, the first set (0), the second (1) or those of the environment
// (2), moved by the rows `u` and `v` of their transform.
fn coordinates(source: u32, first: vec2<f32>, second: vec2<f32>, env: vec2<f32>, u: vec3<f32>, v: vec3<f32>) -> vec2<f32> {
    var at = first;
    if source == 2u {
        at = env;
    } else if source == 1u {
        at = second;
    }
    let point = vec3<f32>(at, 1.0);
    return vec2<f32>(dot(u, point), dot(v, point));
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

// The pixel of a batch whose textures are `combined` in gamma, `element` the alpha of the batch and
// its instance; `flags` its alpha key (0 for none), whether unlit and unfogged, and the colour of its
// fog (0 the view's, 1 black, 2 white, 3 grey for mod2x). Not drawn under its alpha as WotLK tests
// it: the alpha key times `element`, or 1/255; nor, unfogged, beyond the end of the fog of the game,
// which would not hide it.
fn shade(combined: vec4<f32>, element: f32, flags: vec4<f32>, normal: vec3<f32>, world: vec3<f32>) -> vec4<f32> {
    let alpha = clamp(combined.a, 0.0, 1.0);
    let reference = select(LEAST_ALPHA, flags.x * element, flags.x > 0.0);
    if alpha < reference || (flags.z > 0.5 && beyond_fog(world)) {
        discard;
    }
    let fog = select(fog_amount(world), 0.0, flags.z > 0.5);
    // A mod2x batch doubles what is drawn in gamma: its colour, grey in the fog, made such that the
    // target's own doubling in linear gives the same, as far as a colour of 1 reaches.
    if flags.w > 2.5 {
        let gamma = mix(combined.rgb, vec3<f32>(0.5), fog);
        return vec4<f32>(pow(gamma * 2.0, vec3<f32>(2.2)) * 0.5, alpha);
    }
    // In the linear space of the view, as the terrain: lit, then fogged as the view mixes its fog.
    var rgb = linear(clamp(combined.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    if flags.y < 0.5 {
        rgb = rgb * light(normal);
    }
    return vec4<f32>(fog_mix(rgb, fog_colour(flags.w), fog), alpha);
}
