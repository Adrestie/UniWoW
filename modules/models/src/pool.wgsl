// The models drawn from what they share (after common.wgsl and skin.wgsl): for each instance
// drawn, by the number of its draw (`instance_index`, its first instance pointing into the entries
// of the frame), its entry gives its place in the arena of the instances and its material; its
// vertices are posed by its bones when the thread of the animations wrote them; its textures are
// read from the arrays by slot (the bindings of the arrays and `sampled` follow, written for the
// count of slots).

struct Instance {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    // Its alpha.
    extra: vec4<f32>,
};

struct Material {
    colour: vec4<f32>,
    // Its alpha key, whether unlit and unfogged, the colour of its fog (`shade`).
    flags: vec4<f32>,
    // The radius of its model.
    model: vec4<f32>,
    // Its pixel shader, and where its two textures take their coordinates.
    combine: vec4<u32>,
    // The codes of its two textures (the slot in the high 16 bits, the layer in the low ones,
    // NONE for white), and how they wrap: the first in the two low bits (across 1, up and down
    // 2), the second in the next two.
    textures: vec4<u32>,
    // The width and height of the classes of its two textures.
    sizes: vec4<f32>,
};

@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(1) @binding(1) var<storage, read> entries: array<vec2<u32>>;
@group(1) @binding(2) var<storage, read> materials: array<Material>;
// Repeating: a texture is held to its edge by `texel`.
@group(1) @binding(3) var layer_sampler: sampler;

const NONE: u32 = 0xFFFFFFFFu;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv1: vec2<f32>,
    @location(3) uv2: vec2<f32>,
    @location(4) bones: vec4<u32>,
    @location(5) weights: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    // The coordinates of its two textures, as their transforms move them.
    @location(1) uv_one: vec2<f32>,
    @location(2) uv_two: vec2<f32>,
    @location(3) @interpolate(flat) colour: vec4<f32>,
    @location(4) world: vec3<f32>,
    @location(5) alpha: f32,
    @location(6) @interpolate(flat) material: u32,
};

@vertex
fn vs_main(in: VertexIn, @builtin(instance_index) drawn: u32) -> VertexOut {
    let entry = entries[drawn];
    let instance = instances[entry.x];
    let material = materials[entry.y];
    let bone = first_bone(entry.x);
    let vertex = posed(bone, in.position, in.normal, in.bones, in.weights);
    let placed = place(instance.row0, instance.row1, instance.row2, instance.extra.x, material.model.x, vertex.position, vertex.normal);
    var out: VertexOut;
    out.clip = placed.clip;
    out.normal = placed.normal;
    let look = dressed(bone, material.combine.w, material.colour);
    out.uv_one = coordinates(material.combine.y, in.uv1, in.uv2, placed.env, look.one_u, look.one_v);
    out.uv_two = coordinates(material.combine.z, in.uv1, in.uv2, placed.env, look.two_u, look.two_v);
    out.colour = look.colour;
    // A material of no alpha draws nothing, as WotLK leaves it out.
    if look.colour.a <= 0.0 {
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    out.world = placed.world;
    out.alpha = instance.extra.x;
    out.material = entry.y;
    return out;
}

// The texel of the texture `code` of `size` at `uv`, its gradients given, wrapping on each axis as
// `wrap` says and held to its edge on the others, as a sampler clamping to the edge; white for
// none. Held, the point read stays inside by half of what the filter reads around it: the texel of
// the coarsest level its gradients can choose, and their spread for the anisotropy; so that no
// level reaches the other edge.
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

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let material = materials[in.material];
    // The coordinates and their gradients taken where every pixel runs; the textures in gamma, as
    // WotLK combines them.
    let uv_one = in.uv_one;
    let uv_two = in.uv_two;
    let ddx_one = dpdx(uv_one);
    let ddy_one = dpdy(uv_one);
    let ddx_two = dpdx(uv_two);
    let ddy_two = dpdy(uv_two);
    let one = texel(material.textures.x, material.textures.z & 3u, material.sizes.xy, uv_one, ddx_one, ddy_one);
    let two = texel(material.textures.y, (material.textures.z >> 2u) & 3u, material.sizes.zw, uv_two, ddx_two, ddy_two);
    let element = in.colour.a * in.alpha;
    let combined = combine(material.combine.x, vec4<f32>(in.colour.rgb, element), one, two);
    return shade(combined, element, material.flags, in.normal, in.world);
}
