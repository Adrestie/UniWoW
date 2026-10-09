// The models drawn with buffers and textures of their own (after common.wgsl, and skin.wgsl with
// the pool or rest.wgsl without): a batch of a model for every instance of a group, its instances
// read as vertices from the arena of the instances, its one or two textures and what its shader
// reads bound for it; with the pool, its vertices posed by their bones.

struct Batch {
    // Its colour and transparency at rest, its weight included.
    colour: vec4<f32>,
    // Its alpha key, whether unlit and unfogged, the colour of its fog (`shade`).
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
    @location(8) bones: vec4<u32>,
    @location(9) weights: vec4<f32>,
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
};

@vertex
fn vs_main(in: VertexIn, @builtin(instance_index) index: u32) -> VertexOut {
    let bone = first_bone(index);
    let vertex = posed(bone, in.position, in.normal, in.bones, in.weights);
    let placed = place(in.row0, in.row1, in.row2, in.extra.x, batch.model.x, vertex.position, vertex.normal);
    var out: VertexOut;
    out.clip = placed.clip;
    out.normal = placed.normal;
    let look = dressed(bone, batch.combine.w, batch.colour);
    out.uv_one = coordinates(batch.combine.y, in.uv1, in.uv2, placed.env, look.one_u, look.one_v);
    out.uv_two = coordinates(batch.combine.z, in.uv1, in.uv2, placed.env, look.two_u, look.two_v);
    out.colour = look.colour;
    // A material of no alpha draws nothing, as WotLK leaves it out.
    if look.colour.a <= 0.0 {
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    out.world = placed.world;
    out.alpha = in.extra.x;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Both textures sampled where every pixel runs, their coordinates chosen first; in gamma, as
    // WotLK combines them.
    let one = textureSample(first, first_sampler, in.uv_one);
    let two = textureSample(second, second_sampler, in.uv_two);
    let element = in.colour.a * in.alpha;
    let combined = combine(batch.combine.x, vec4<f32>(in.colour.rgb, element), one, two);
    return shade(combined, element, batch.flags, in.normal, in.world);
}
