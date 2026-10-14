// The buildings drawn from what they share: for each batch drawn, by the number of its draw
// (`instance_index`, its first instance pointing into the entries of the frame), its entry gives
// its placement among those of the frame, its material, the flags of its group and its kind; its
// textures are read from the arrays by slot (the bindings of the arrays and `sampled` follow,
// written for the count of slots). The pixel shaders of 3.3.5a combine its textures in gamma; a
// batch is lit as the public implementation WebWowViewerCpp lights them (`calcLight`, read for its
// facts): inside by the ambient colour of its building and its vertex colours, outside by the
// ambient light and the sun of the view with its vertex colours, a transition between the two by
// the alpha of its vertex colours; then what is added after the light, the environment and the
// glow at night of its material; then fogged.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour on the ground and the light everywhere, in gamma.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    eye: vec4<f32>,
    // The colour of the fog; where it starts, its middle and where it covers all, and the rate of
    // the game's fog, 0 for the editor's.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
    // The axes of the camera, across, up and back.
    across: vec4<f32>,
    up: vec4<f32>,
    back: vec4<f32>,
    // How much the materials glowing at night glow, from 0 by day to 1 by night.
    glow: vec4<f32>,
    // The colour of the highlight of the specular and metal materials, in gamma, black for none.
    highlight: vec4<f32>,
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
    // Its flags, its blending and its emissive colour, red in the low byte.
    flags: vec4<u32>,
};

@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(1) @binding(1) var<storage, read> entries: array<vec4<u32>>;
@group(1) @binding(2) var<storage, read> materials: array<Material>;
@group(1) @binding(3) var layer_sampler: sampler;

const NONE: u32 = 0xFFFFFFFFu;
// The alpha under which a pixel without an alpha key is not drawn.
const LEAST_ALPHA: f32 = 1.0 / 255.0;
// The blending of an opaque material.
const OPAQUE: u32 = 0u;
// The flags of a group: its vertex colours, outside, lit as outside; of a material: lit as outside,
// glowing at night.
const HAS_COLOURS: u32 = 0x4u;
const OUTSIDE: u32 = 0x8u;
const GROUP_LIT_OUTSIDE: u32 = 0x40u;
const LIT_OUTSIDE: u32 = 0x8u;
const GLOWS_AT_NIGHT: u32 = 0x10u;
// The flag a building lit as one gives its groups.
const LIT_AS_ONE: u32 = 0x10u;
// Towards the light of the insides, the opposite of the direction the client gives it,
// (-0.30822, -0.30822, -0.9) in the axes of the world (WebWowViewerCpp, `uInteriorSunDir`).
const TOWARDS_INSIDE_LIGHT: vec3<f32> = vec3<f32>(0.30822, 0.30822, 0.9);

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

// What a pixel shader gives: the colour lit and its alpha, and what is added after the light, in
// gamma.
struct Combined {
    lit: vec4<f32>,
    added: vec3<f32>,
};

// The pixel shaders of 3.3.5a, in gamma: diffuse (0), specular (1) and metal (2) as diffuse, their
// highlight added after the light (`highlight`); environment (3) and environment metal (5), the
// second texture mapped on the environment added after the light, as WebWowViewerCpp adds it
// (`caclWMOFragMat`): by the alpha of the first, and for the metal times the first too; opaque (4);
// two layers (6), the second texture blended over the first by the alpha of the second colours.
fn combine(shader: u32, one: vec4<f32>, two: vec4<f32>, blend: f32) -> Combined {
    switch shader {
        case 3u: { return Combined(one, two.rgb * one.a); }
        case 5u: { return Combined(one, one.rgb * one.a * two.rgb); }
        case 4u: { return Combined(vec4<f32>(one.rgb, 1.0), vec3<f32>(0.0)); }
        case 6u: { return Combined(vec4<f32>(mix(two.rgb, one.rgb, blend), one.a), vec3<f32>(0.0)); }
        default: { return Combined(one, vec3<f32>(0.0)); }
    }
}

// The light everywhere of a face of `normal`, `ambient` from 0.9 to 1.1 times as the face turns to
// `towards`, as the client mixes its colours of the sky and of the ground (`applyAndMixAmbients`).
fn turned(ambient: vec3<f32>, normal: vec3<f32>, towards: vec3<f32>) -> vec3<f32> {
    let facing = clamp(dot(normal, towards), 0.0, 1.0);
    return ambient * (0.9 + 0.2 * facing);
}

// The light of a batch of a building, in gamma, as WebWowViewerCpp lights them (`calcLight`): its
// vertex colours `vertex`, doubled back, added to the light everywhere, inside the ambient colour
// `ambient` of the building, turned towards the light of the insides, without a diffuse; outside the
// ambient light of the view turned towards the sun, plus its diffuse by the angle; the two mixed by
// the share `outside`, 0 wholly inside and 1 wholly outside.
fn batch_light(normal: vec3<f32>, vertex: vec3<f32>, ambient: vec3<f32>, outside: f32) -> vec3<f32> {
    let n = normalize(normal);
    let inside = turned(ambient + vertex, n, TOWARDS_INSIDE_LIGHT);
    let facing = clamp(dot(n, camera.sun.xyz), 0.0, 1.0);
    let lit_outside = turned(camera.ambient.rgb + vertex, n, camera.sun.xyz) + camera.sun_colour.rgb * facing;
    return mix(inside, lit_outside, outside);
}

// The highlight of a texel `texel` of the shaders specular (1) and metal (2) on a face of `normal`
// at `world`, in gamma, by the formula of later clients (the public implementation
// WebWowViewerCpp's `calcSpec`, which it leaves out, read for its facts): half way between towards
// the sun and towards the eye, 1.25 times the cosine of that way with the normal to the 8th; times
// what the face reflects, the 5th power of what the cosine of the sun with that way leaves of 1,
// mixed towards 1 by the alpha of the texel, for the metal by its red four times its alpha, beyond
// 1 above it; times the colour of the highlight and the cosine of the sun with the normal.
fn highlight(shader: u32, texel: vec4<f32>, normal: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    let n = normalize(normal);
    let to_sun = camera.sun.xyz;
    let half_way = normalize(to_sun + normalize(camera.eye.xyz - world));
    let shine = 1.25 * pow(clamp(dot(n, half_way), 0.0, 1.0), 8.0);
    let mask = select(texel.a, texel.r * 4.0 * texel.a, shader == 2u);
    let reflected = mix(pow(1.0 - clamp(dot(to_sun, half_way), 0.0, 1.0), 5.0), 1.0, mask);
    return camera.highlight.rgb * shine * reflected * clamp(dot(n, to_sun), 0.0, 1.0);
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
    var alpha = clamp(combined.lit.a, 0.0, 1.0);
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
    // Unfogged, not drawn beyond the end of the fog of the game, which would not hide it.
    if shading.z > 0.5 && beyond_fog(in.world) {
        discard;
    }
    let fog = select(fog_amount(in.world), 0.0, shading.z > 0.5);
    // A mod2x batch doubles what is drawn: its colour, grey in the fog.
    if shading.w > 2.5 {
        return vec4<f32>(mix(combined.lit.rgb, vec3<f32>(0.5), fog), alpha);
    }
    // In gamma, as the client: lit, then fogged.
    var rgb = clamp(combined.lit.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    // The highlight of the specular and metal materials, by their share of the light outside.
    var shining = vec3<f32>(0.0);
    if shading.y < 0.5 {
        // Its vertex colours, halved when fixed; its share of the light outside: 1 in a group lit
        // as outside, for a batch outside (the outer walls of the rooms, their colours left out)
        // and a material lit as outside; the alpha of its colours for a transition, which the fix
        // left there; 0 inside.
        let coloured = (in.group & HAS_COLOURS) != 0u;
        var vertex = select(vec3<f32>(0.0), in.colour1.rgb * 2.0, coloured);
        var outside = 0.0;
        if (in.group & (OUTSIDE | GROUP_LIT_OUTSIDE)) != 0u {
            outside = 1.0;
        } else {
            switch (in.group >> 8u) & 3u {
                case 0u: { outside = select(0.0, in.colour1.a, coloured); }
                case 1u: { outside = 0.0; }
                default: {
                    outside = 1.0;
                    vertex = vec3<f32>(0.0);
                }
            }
        }
        if (material.flags.x & LIT_OUTSIDE) != 0u {
            outside = 1.0;
            vertex = vec3<f32>(0.0);
        }
        var lit = batch_light(in.normal, vertex, in.ambient, outside);
        // A building lit as one: outside, its ambient colour added at the drawing.
        if (in.group & LIT_AS_ONE) != 0u {
            lit = light(in.normal) + in.ambient;
        }
        rgb = rgb * lit;
        if shader == 1u || shader == 2u {
            shining = highlight(shader, one, in.normal, in.world) * outside;
        }
    }
    // Added after the light: the environment, the highlight, and the emissive colour of a material
    // glowing at night by how much it is night, as Noggit Red adds it (read for the facts only).
    var added = combined.added + shining;
    if (material.flags.x & GLOWS_AT_NIGHT) != 0u {
        added = added + unpack4x8unorm(material.flags.z).rgb * camera.glow.x;
    }
    if any(added > vec3<f32>(0.0)) {
        rgb = clamp(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) + added, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(fog_mix(rgb, fog_colour(shading.w), fog), alpha);
}
