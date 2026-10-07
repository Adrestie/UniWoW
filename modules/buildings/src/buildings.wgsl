// The buildings drawn from what they share: for each batch drawn, by the number of its draw
// (`instance_index`, its first instance pointing into the entries of the frame), its entry gives
// its placement among those of the frame, its material, the flags of its group and its kind; its
// textures are read from the arrays by slot (the bindings of the arrays and `sampled` follow,
// written for the count of slots). The pixel shaders of 3.3.5a combine its textures in gamma; a
// batch outside is lit by the sun and the ambient light of the view, one inside by its vertex
// colours and the ambient colour of its building, one of a transition by both, blended by the alpha
// of its vertex colours; then fogged.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour, and the light everywhere.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
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

struct Instance {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    // The ambient colour of its building.
    ambient: vec4<f32>,
};

struct Material {
    // The codes of its two textures (the slot in the high 16 bits, the layer in the low ones,
    // NONE for white), how they wrap (the first in the two low bits, across 1 and up and down 2,
    // the second in the next two), and its shader.
    textures: vec4<u32>,
    // The width and height of the classes of its two textures.
    sizes: vec4<f32>,
    // Its alpha key (0 for none), whether unlit and unfogged, the colour of its fog.
    shading: vec4<f32>,
    // Its flags and its blending.
    flags: vec4<u32>,
};

@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(1) @binding(1) var<storage, read> entries: array<vec4<u32>>;
@group(1) @binding(2) var<storage, read> materials: array<Material>;
@group(1) @binding(3) var layer_sampler: sampler;

const NONE: u32 = 0xFFFFFFFFu;
// The share of the fog at its middle; the alpha under which a pixel without an alpha key is not
// drawn.
const NEAR_FOG: f32 = 0.55;
const LEAST_ALPHA: f32 = 1.0 / 255.0;
// The blending of an opaque material.
const OPAQUE: u32 = 0u;
// The flags of a group: its vertex colours, outside; of a material: lit as outside.
const HAS_COLOURS: u32 = 0x4u;
const OUTSIDE: u32 = 0x8u;
const LIT_OUTSIDE: u32 = 0x8u;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec4<f32>,
    @location(2) uv1: vec2<f32>,
    @location(3) uv2: vec2<f32>,
    @location(4) colour1: vec4<f32>,
    @location(5) colour2: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv1: vec2<f32>,
    @location(2) uv2: vec2<f32>,
    @location(3) env: vec2<f32>,
    @location(4) colour1: vec4<f32>,
    @location(5) blend: f32,
    @location(6) world: vec3<f32>,
    @location(7) @interpolate(flat) material: u32,
    @location(8) @interpolate(flat) group: u32,
    @location(9) @interpolate(flat) ambient: vec3<f32>,
};

// The coordinates of the environment, as WotLK maps them: the position and the normal in the space
// of the camera, its depth growing away from the eye.
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
fn vs_main(in: VertexIn, @builtin(instance_index) drawn: u32) -> VertexOut {
    let entry = entries[drawn];
    let instance = instances[entry.x];
    let at = vec4<f32>(in.position, 1.0);
    let world = vec3<f32>(dot(instance.row0, at), dot(instance.row1, at), dot(instance.row2, at));
    let direction = vec4<f32>(in.normal.xyz, 0.0);
    let normal = vec3<f32>(dot(instance.row0, direction), dot(instance.row1, direction), dot(instance.row2, direction));
    var out: VertexOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.normal = normal;
    out.uv1 = in.uv1;
    out.uv2 = in.uv2;
    out.env = sphere_map(world, normalize(normal));
    out.colour1 = in.colour1;
    out.blend = in.colour2.a;
    out.world = world;
    out.material = entry.y;
    out.group = entry.z;
    out.ambient = instance.ambient.rgb;
    return out;
}

// The texel of the texture `code` of `size` at `uv`, its gradients given, wrapping on each axis as
// `wrap` says and held to its edge on the others, as a sampler clamping to the edge; white for none.
fn texel(code: u32, wrap: u32, size: vec2<f32>, uv: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
    if code == NONE {
        return vec4<f32>(1.0);
    }
    let texels = max(size, vec2<f32>(1.0));
    let across = ddx * texels;
    let up = ddy * texels;
    let coarsest = max(max(length(across), length(up)), 1.0);
    let reach = min((abs(across) + abs(up) + vec2<f32>(coarsest)) * 0.5 / texels, vec2<f32>(0.5));
    let held = clamp(uv, reach, vec2<f32>(1.0) - reach);
    let at = vec2<f32>(select(held.x, uv.x, (wrap & 1u) != 0u), select(held.y, uv.y, (wrap & 2u) != 0u));
    return sampled(code >> 16u, i32(code & 0xFFFFu), at, ddx, ddy);
}

// The pixel shaders of 3.3.5a, in gamma: diffuse (0), specular (1) and metal (2) as diffuse, their
// highlight left out; environment (3) and environment metal (5), the second texture mapped on the
// environment and added by the alpha of the first; opaque (4); two layers (6), the second texture
// blended over the first by the alpha of the second colours.
fn combine(shader: u32, one: vec4<f32>, two: vec4<f32>, blend: f32) -> vec4<f32> {
    switch shader {
        case 3u, 5u: { return vec4<f32>(one.rgb + two.rgb * one.a, one.a); }
        case 4u: { return vec4<f32>(one.rgb, 1.0); }
        case 6u: { return vec4<f32>(mix(two.rgb, one.rgb, blend), one.a); }
        default: { return one; }
    }
}

// The linear value of a value in gamma, as an sRGB target encodes it back.
fn linear(gamma: vec3<f32>) -> vec3<f32> {
    let low = gamma / 12.92;
    let high = pow((gamma + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, gamma <= vec3<f32>(0.04045));
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

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let material = materials[in.material];
    let shader = material.textures.w;
    let ddx_one = dpdx(in.uv1);
    let ddy_one = dpdy(in.uv1);
    let second = select(in.uv2, in.env, shader == 3u || shader == 5u);
    let ddx_two = dpdx(second);
    let ddy_two = dpdy(second);
    let one = texel(material.textures.x, material.textures.z & 3u, material.sizes.xy, in.uv1, ddx_one, ddy_one);
    let two = texel(material.textures.y, (material.textures.z >> 2u) & 3u, material.sizes.zw, second, ddx_two, ddy_two);
    let combined = combine(shader, one, two, in.blend);
    var alpha = clamp(combined.a, 0.0, 1.0);
    let shading = material.shading;
    // An opaque batch keeps every pixel, the alpha of its textures a mask of their own; an
    // alpha-keyed one those over its key; a blended one those it shows. The first two are opaque.
    var reference = LEAST_ALPHA;
    if shading.x > 0.0 {
        reference = shading.x;
    } else if material.flags.y == OPAQUE {
        reference = -1.0;
    }
    if alpha < reference {
        discard;
    }
    if reference != LEAST_ALPHA {
        alpha = 1.0;
    }
    let fog = select(fog_amount(in.world), 0.0, shading.z > 0.5);
    // A mod2x batch doubles what is drawn in gamma, as the models draw it.
    if shading.w > 2.5 {
        let gamma = mix(combined.rgb, vec3<f32>(0.5), fog);
        return vec4<f32>(pow(gamma * 2.0, vec3<f32>(2.2)) * 0.5, alpha);
    }
    var rgb = linear(clamp(combined.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    if shading.y < 0.5 {
        let lit = max(dot(normalize(in.normal), camera.sun.xyz), 0.0);
        let outside = camera.ambient.rgb + camera.sun_colour.rgb * lit;
        // Inside: the vertex colours, halved when fixed, and the ambient colour, both in gamma.
        let coloured = (in.group & HAS_COLOURS) != 0u;
        let inside = linear(select(vec3<f32>(0.0), in.colour1.rgb * 2.0, coloured) + in.ambient);
        // The share of the light outside: by the kind of the batch; for a transition, by the alpha
        // of its vertex colours, or by its group when it has none.
        var weight = 1.0;
        switch (in.group >> 8u) & 3u {
            case 0u: { weight = select(select(0.0, 1.0, (in.group & OUTSIDE) != 0u), in.colour1.a, coloured); }
            case 1u: { weight = 0.0; }
            default: {}
        }
        if (material.flags.x & LIT_OUTSIDE) != 0u {
            weight = 1.0;
        }
        rgb = rgb * mix(inside, outside, weight);
    }
    return vec4<f32>(mix(rgb, fog_colour(shading.w), fog), alpha);
}
