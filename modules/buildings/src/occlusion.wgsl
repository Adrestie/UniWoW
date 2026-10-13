// The groups of the buildings tested against the depth, in two phases as the instances of the
// models: `first`, before the first pass of the view, writes the draws of the opaque batches of the
// groups seen at the frame before; `test`, between the passes, tests every group listed against the
// pyramid of the depth the first pass left, those found in sight kept for the next frame; `second`
// writes the draws of the opaque batches of the groups found in sight and not seen at the frame
// before, drawn in `Revealed`, and of the blended batches of the groups found in sight. A draw is
// the command the CPU listed, its count of instances 1 where it is drawn and 0 where it is not.

struct Params {
    // The view, to find where a box falls on the pyramid.
    view_proj: mat4x4<f32>,
    // The opaque draws, every draw, the groups listed.
    counts: vec4<u32>,
};

@group(0) @binding(0) var<uniform> params: Params;
// The commands the CPU listed, five words a draw.
@group(0) @binding(1) var<storage, read> commands: array<u32>;
// The entries of the draws: their placement, material, flags and kind, and their group listed.
@group(0) @binding(2) var<storage, read> entries: array<vec4<u32>>;
// The groups listed, two vectors each: the lowest corner of their box in the world and their place
// in the table of those seen (its bits), then the highest corner.
@group(0) @binding(3) var<storage, read> groups: array<vec4<f32>>;
// Whether each group of the buildings drawn was seen at the frame before, and at this one.
@group(0) @binding(4) var<storage, read> before: array<u32>;
@group(0) @binding(5) var<storage, read_write> now: array<u32>;
// The draws written, five words each.
@group(0) @binding(6) var<storage, read_write> drawn: array<u32>;
// The groups the pyramid hid.
@group(0) @binding(7) var<storage, read_write> hidden: array<atomic<u32>>;
// The depth the first pass left, the farthest of each texel (`viewport::Pyramid`).
@group(1) @binding(0) var pyramid: texture_2d<f32>;

const THREADS: u32 = 64u;

// The index of the thread `local` of the workgroup `workgroup` over rows of `across` workgroups.
fn index_of(workgroup: vec3<u32>, across: u32, local: u32) -> u32 {
    return (workgroup.y * across + workgroup.x) * THREADS + local;
}

// The place of the group listed `group` in the table of those seen.
fn slot_of(group: u32) -> u32 {
    return bitcast<u32>(groups[group * 2u].w);
}

// Writes the draw `draw` as the CPU listed it, drawn when `shown` says so.
fn write_draw(draw: u32, shown: bool) {
    for (var word = 0u; word < 5u; word++) {
        drawn[draw * 5u + word] = commands[draw * 5u + word];
    }
    drawn[draw * 5u + 1u] = select(0u, 1u, shown);
}

// Whether the box from `low` to `high` may be seen past the depth the first pass left: its nearest
// depth (reverse Z: the greatest) not less than the least of the pyramid over the rectangle it
// covers, read at the level where that rectangle spans two texels at most. A box reaching behind
// the eye is seen.
fn seen(low: vec3<f32>, high: vec3<f32>) -> bool {
    var least = vec2<f32>(1.0);
    var most = vec2<f32>(-1.0);
    var nearest = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        let pick = (vec3<u32>(corner) >> vec3<u32>(0u, 1u, 2u)) & vec3<u32>(1u);
        let point = select(low, high, pick == vec3<u32>(1u));
        let clip = params.view_proj * vec4<f32>(point, 1.0);
        if clip.w <= 0.0 {
            return true;
        }
        let ndc = clip.xyz / clip.w;
        least = min(least, ndc.xy);
        most = max(most, ndc.xy);
        nearest = max(nearest, ndc.z);
    }
    // From the view's -1 to 1, y up, to its pixels, y down.
    let size = vec2<f32>(textureDimensions(pyramid, 0u));
    let first = clamp(vec2<f32>(least.x, -most.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)) * size;
    let last = min(clamp(vec2<f32>(most.x, -least.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)) * size, size - 1.0);
    let extent = max(last.x - first.x, last.y - first.y);
    let level = min(u32(ceil(log2(max(extent, 1.0)))), textureNumLevels(pyramid) - 1u);
    let texels = textureDimensions(pyramid, level) - vec2<u32>(1u);
    let start = min(vec2<u32>(first) >> vec2<u32>(level), texels);
    let end = min(vec2<u32>(last) >> vec2<u32>(level), texels);
    var farthest = 1.0;
    for (var y = start.y; y <= end.y; y++) {
        for (var x = start.x; x <= end.x; x++) {
            farthest = min(farthest, textureLoad(pyramid, vec2<u32>(x, y), level).r);
        }
    }
    return nearest >= farthest;
}

@compute @workgroup_size(64)
fn first(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) rows: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let draw = index_of(workgroup, rows.x, local);
    if draw >= params.counts.x {
        return;
    }
    write_draw(draw, before[slot_of(entries[draw].w)] != 0u);
}

@compute @workgroup_size(64)
fn test(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) rows: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let group = index_of(workgroup, rows.x, local);
    if group >= params.counts.z {
        return;
    }
    if seen(groups[group * 2u].xyz, groups[group * 2u + 1u].xyz) {
        now[slot_of(group)] = 1u;
    } else {
        atomicAdd(&hidden[0], 1u);
    }
}

@compute @workgroup_size(64)
fn second(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) rows: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let draw = index_of(workgroup, rows.x, local);
    if draw >= params.counts.y {
        return;
    }
    let slot = slot_of(entries[draw].w);
    let found = now[slot] != 0u;
    // An opaque batch the first pass drew is not drawn again; a blended one is drawn once found.
    let shown = select(found, found && before[slot] == 0u, draw < params.counts.x);
    write_draw(draw, shown);
}
