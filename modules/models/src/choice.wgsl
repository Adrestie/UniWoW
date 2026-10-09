// The instances drawn from the pool, chosen by the GPU twice a frame. `choose_first`, before the
// first pass of the view: each instance of the groups the CPU found in sight that was drawn at the
// frame before, in sight itself and within the reach of its size, at the level of skin its
// distance chooses, kept from the frame before until past its limit by the margin. `choose_second`,
// between the passes: each instance chosen so and not hidden by the pyramid of the depth the first
// pass left, those the first drew not drawn again; what it keeps is what the next frame draws first.
// The instances of each record (a batch of a look at a level) counted. Then a prefix sum over the
// records, a workgroup a block of 256 (`blocks`, `tops_*`, `place`): the places of the records'
// instances, and of their draws among those of their state; `pack`: the arguments of the records
// drawn, packed in the order of their state's records. `tops_second` also keeps, in their order,
// the templates of the blended batches the CPU sorted the farthest first where the level chosen is
// theirs: the blended are drawn in the second pass only. `scatter_*`: each instance's entries
// written at its records' places.

struct Params {
    // The sides of the view (x >= -w, x <= w, y >= -w, y <= w) and the eye's (w > 0).
    planes: array<vec4<f32>, 5>,
    // The eye, and how far an instance is drawn, in radii.
    eye: vec4<f32>,
    // The limits of the levels, in radii, and the margin.
    limits: vec4<f32>,
    // The records, the templates, the groups, and where the entries of the records begin.
    sizes: vec4<u32>,
    // The opaque regions, the blended regions, whether the draws are packed, where the statistics
    // are in the work buffer.
    regions: vec4<u32>,
    // Where the opaque regions, the looks, the references and the records begin in the tables.
    statics: vec4<u32>,
    // Where the blended regions, the groups and the templates begin in those of the frame.
    frames: vec4<u32>,
    // Where the cursors, the sums of the records, those of the templates and the counts of the
    // draws begin in the work buffer.
    work: vec4<u32>,
    // Where the sums of the blocks begin in the work buffer, how many blocks, and where their
    // totals are.
    blocks: vec4<u32>,
    // The view, to find where a box falls on the pyramid.
    view_proj: mat4x4<f32>,
};

struct Instance {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<storage, read> statics: array<u32>;
@group(0) @binding(3) var<storage, read> frames: array<u32>;
@group(0) @binding(4) var<storage, read> before: array<u32>;
// The level of each instance plus one, 0 where it is not drawn.
@group(0) @binding(5) var<storage, read_write> levels: array<u32>;
// The counts of the records' instances, then the cursors, the sums, the counts of the draws and
// the statistics (`Params::work`).
@group(0) @binding(6) var<storage, read_write> work: array<atomic<u32>>;
@group(0) @binding(7) var<storage, read_write> entries: array<vec2<u32>>;
@group(0) @binding(8) var<storage, read_write> args: array<u32>;
// The depth the first pass left, the farthest of each texel (`viewport::Pyramid`).
@group(1) @binding(0) var pyramid: texture_2d<f32>;

const LOOK: u32 = 10u;
const RECORD: u32 = 5u;
const GROUP: u32 = 3u;
const TEMPLATE: u32 = 7u;
const ARGS: u32 = 5u;
const THREADS: u32 = 256u;

// The level of an instance `ratio` of its radius from the eye, `previous` the level before plus
// one (0 for none), never past the last of `count`: as `layer::level`.
fn level_of(ratio: f32, previous: u32, count: u32) -> u32 {
    var plain = 0u;
    for (var limit = 0u; limit < 3u; limit++) {
        if ratio >= params.limits[limit] {
            plain += 1u;
        }
    }
    var level = plain;
    if previous != 0u {
        let kept = previous - 1u;
        if plain > kept && ratio < params.limits[kept] * (1.0 + params.limits.w) {
            level = kept;
        } else if plain < kept && ratio > params.limits[kept - 1u] * (1.0 - params.limits.w) {
            level = kept;
        }
    }
    return min(level, max(count, 1u) - 1u);
}

// Whether the box of `half` around `origin` may be in sight: no side of the view has it all
// beyond, as `layer::in_sight`.
fn in_sight(origin: vec3<f32>, half: f32) -> bool {
    for (var side = 0u; side < 5u; side++) {
        let plane = params.planes[side];
        let most = dot(plane.xyz, origin) + plane.w + half * (abs(plane.x) + abs(plane.y) + abs(plane.z));
        if (side < 4u && most < 0.0) || (side == 4u && most <= 0.0) {
            return false;
        }
    }
    return true;
}

// The box of the instance `index` of a look of `radius`: its origin, and its half side, the radius
// at its largest scale.
fn box_of(index: u32, radius: f32) -> vec4<f32> {
    let instance = instances[index];
    let scale = max(
        length(vec3<f32>(instance.row0.x, instance.row1.x, instance.row2.x)),
        max(
            length(vec3<f32>(instance.row0.y, instance.row1.y, instance.row2.y)),
            length(vec3<f32>(instance.row0.z, instance.row1.z, instance.row2.z)),
        ),
    );
    return vec4<f32>(instance.row0.w, instance.row1.w, instance.row2.w, radius * scale);
}

// The level plus one of the instance `index`, of a look of `radius` and `count` levels; 0 out of
// sight or beyond the reach of its size.
fn chosen(index: u32, radius: f32, count: u32) -> u32 {
    let shape = box_of(index, radius);
    let origin = shape.xyz;
    let size = shape.w;
    let away = length(max(abs(params.eye.xyz - origin) - vec3<f32>(size), vec3<f32>(0.0)));
    if away > params.eye.w * max(size, 1.0) || !in_sight(origin, size) {
        return 0u;
    }
    return level_of(away / max(size, 0.5), before[index], count) + 1u;
}

// Whether the box of the instance `index`, of a look of `radius`, may be seen past the depth the
// first pass left: its nearest depth (reverse Z: the greatest) not less than the least of the
// pyramid over the rectangle it covers, read at the level where that rectangle spans two texels at
// most. A box reaching behind the eye is seen.
fn seen(index: u32, radius: f32) -> bool {
    let shape = box_of(index, radius);
    var low = vec2<f32>(1.0);
    var high = vec2<f32>(-1.0);
    var nearest = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        let side = vec3<f32>((vec3<u32>(corner) >> vec3<u32>(0u, 1u, 2u)) & vec3<u32>(1u)) * 2.0 - 1.0;
        let clip = params.view_proj * vec4<f32>(shape.xyz + side * shape.w, 1.0);
        if clip.w <= 0.0 {
            return true;
        }
        let ndc = clip.xyz / clip.w;
        low = min(low, ndc.xy);
        high = max(high, ndc.xy);
        nearest = max(nearest, ndc.z);
    }
    // From the view's -1 to 1, y up, to its pixels, y down.
    let size = vec2<f32>(textureDimensions(pyramid, 0u));
    let first = clamp(vec2<f32>(low.x, -high.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)) * size;
    let last = min(clamp(vec2<f32>(high.x, -low.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)) * size, size - 1.0);
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

// The group of the workgroup `workgroup`, a workgroup a group over rows of `across` workgroups;
// past the groups of the frame, the last with no instance, so that its workgroup reaches the
// barriers with the others yet does nothing.
fn group_of_workgroup(workgroup: vec3<u32>, across: u32) -> vec3<u32> {
    let at = workgroup.y * across + workgroup.x;
    let group = group_of(min(at, params.sizes.z - 1u));
    return vec3<u32>(group.x, select(0u, group.y, at < params.sizes.z), group.z);
}

// The group `group` of the frame: its first instance, its count, where its look is in the tables.
fn group_of(group: u32) -> vec3<u32> {
    let at = params.frames.y + group * GROUP;
    return vec3<u32>(frames[at], frames[at + 1u], params.statics.y + frames[at + 2u] * LOOK);
}

// The instances chosen at each level, then those the pyramid hid, in a workgroup.
var<workgroup> chosen_levels: array<atomic<u32>, 5>;

fn begin_choosing(thread: u32) {
    if thread < 5u {
        atomicStore(&chosen_levels[thread], 0u);
    }
    workgroupBarrier();
}

// The instances chosen at each level, and those hidden, added to the statistics.
fn end_choosing(thread: u32) {
    workgroupBarrier();
    if thread < 4u {
        atomicAdd(&work[params.regions.w + 4u + thread], atomicLoad(&chosen_levels[thread]));
    } else if thread == 4u {
        atomicAdd(&work[params.regions.w + 3u], atomicLoad(&chosen_levels[4u]));
    }
}

// An instance of the group `group` drawn at `level` plus one: counted among the records of its
// look at that level.
fn count_chosen(group: vec3<u32>, level: u32) {
    atomicAdd(&chosen_levels[level - 1u], 1u);
    let first = statics[group.z + 2u * level];
    let references = statics[group.z + 2u * level + 1u];
    for (var j = 0u; j < references; j++) {
        atomicAdd(&work[statics[params.statics.z + first + j]], 1u);
    }
}

@compute @workgroup_size(64)
fn choose_first(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) workgroups: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    begin_choosing(local.x);
    let group = group_of_workgroup(workgroup, workgroups.x);
    let radius = bitcast<f32>(statics[group.z]);
    let count = statics[group.z + 1u];
    for (var k = local.x; k < group.y; k += 64u) {
        let index = group.x + k;
        var level = 0u;
        if before[index] != 0u {
            level = chosen(index, radius, count);
        }
        levels[index] = level;
        if level != 0u {
            count_chosen(group, level);
        }
    }
    end_choosing(local.x);
}

@compute @workgroup_size(64)
fn choose_second(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) workgroups: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    begin_choosing(local.x);
    let group = group_of_workgroup(workgroup, workgroups.x);
    let radius = bitcast<f32>(statics[group.z]);
    let count = statics[group.z + 1u];
    for (var k = local.x; k < group.y; k += 64u) {
        let index = group.x + k;
        var level = chosen(index, radius, count);
        if level != 0u && !seen(index, radius) {
            atomicAdd(&chosen_levels[4u], 1u);
            level = 0u;
        }
        levels[index] = level;
        // What the first phase drew is not drawn again.
        if level != 0u && before[index] == 0u {
            count_chosen(group, level);
        }
    }
    end_choosing(local.x);
}

// Each instance drawn in the phase: its entries written at its records' places; in the second,
// not those the first drew.
fn scatter_in(workgroup: vec3<u32>, across: u32, local: vec3<u32>, second: bool) {
    let group = group_of_workgroup(workgroup, across);
    for (var k = local.x; k < group.y; k += 64u) {
        let index = group.x + k;
        let level = levels[index];
        if level == 0u || (second && before[index] != 0u) {
            continue;
        }
        let first = statics[group.z + 2u * level];
        let references = statics[group.z + 2u * level + 1u];
        for (var j = 0u; j < references; j++) {
            let record = statics[params.statics.z + first + j];
            let place = atomicAdd(&work[params.work.x + record], 1u);
            entries[place] = vec2<u32>(index, statics[params.statics.w + record * RECORD + 3u]);
        }
    }
}

@compute @workgroup_size(64)
fn scatter_first(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) workgroups: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    scatter_in(workgroup, workgroups.x, local, false);
}

@compute @workgroup_size(64)
fn scatter_second(
    @builtin(workgroup_id) workgroup: vec3<u32>,
    @builtin(num_workgroups) workgroups: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    scatter_in(workgroup, workgroups.x, local, true);
}

var<workgroup> sums: array<u32, 256>;
var<workgroup> drawn: array<u32, 256>;
var<workgroup> totals: vec2<u32>;

// The exclusive sums of `sums` and `drawn` over the workgroup, in place, by its first thread; their
// totals.
fn sum_up(thread: u32) -> vec2<u32> {
    workgroupBarrier();
    if thread == 0u {
        var all = 0u;
        var draws = 0u;
        for (var k = 0u; k < THREADS; k++) {
            let more = sums[k];
            sums[k] = all;
            all += more;
            let other = drawn[k];
            drawn[k] = draws;
            draws += other;
        }
        totals = vec2<u32>(all, draws);
    }
    return workgroupUniformLoad(&totals);
}

// Writes the arguments of a draw at `at`.
fn write_args(at: u32, indices: u32, count: u32, first_index: u32, base_vertex: u32, first_instance: u32) {
    args[at * ARGS] = indices;
    args[at * ARGS + 1u] = count;
    args[at * ARGS + 2u] = first_index;
    args[at * ARGS + 3u] = base_vertex;
    args[at * ARGS + 4u] = first_instance;
}

// Each block of 256 records: the sums, within it, of the instances and of the draws before each
// record, and its totals.
@compute @workgroup_size(256)
fn blocks(@builtin(workgroup_id) workgroup: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let record = workgroup.x * THREADS + local.x;
    var count = 0u;
    if record < params.sizes.x {
        count = atomicLoad(&work[record]);
    }
    let flag = select(0u, 1u, count > 0u);
    sums[local.x] = count;
    drawn[local.x] = flag;
    for (var step = 1u; step < THREADS; step = step << 1u) {
        workgroupBarrier();
        var more = 0u;
        var other = 0u;
        if local.x >= step {
            more = sums[local.x - step];
            other = drawn[local.x - step];
        }
        workgroupBarrier();
        sums[local.x] += more;
        drawn[local.x] += other;
    }
    workgroupBarrier();
    if record < params.sizes.x {
        atomicStore(&work[params.work.x + record], sums[local.x] - count);
        atomicStore(&work[params.work.y + record], drawn[local.x] - flag);
        if count > 0u {
            atomicAdd(&work[params.regions.w + 2u], count * (statics[params.statics.w + record * RECORD] / 3u));
        }
    }
    if local.x == THREADS - 1u {
        atomicStore(&work[params.blocks.x + workgroup.x * 2u], sums[local.x]);
        atomicStore(&work[params.blocks.x + workgroup.x * 2u + 1u], drawn[local.x]);
    }
}

// The places of the blocks, from their totals; then the templates, kept where the level chosen
// for their instance is theirs, in the second phase only, packed in their order.
fn tops_in(local: vec3<u32>, second: bool) {
    let thread = local.x;
    let count = params.blocks.y;
    let span = (count + THREADS - 1u) / THREADS;
    let begin = min(thread * span, count);
    let end = min(begin + span, count);
    var instances_sum = 0u;
    var draws_sum = 0u;
    for (var block = begin; block < end; block++) {
        instances_sum += atomicLoad(&work[params.blocks.x + block * 2u]);
        draws_sum += atomicLoad(&work[params.blocks.x + block * 2u + 1u]);
    }
    sums[thread] = instances_sum;
    drawn[thread] = draws_sum;
    let record_totals = sum_up(thread);
    var place = sums[thread];
    var before_drawn = drawn[thread];
    for (var block = begin; block < end; block++) {
        let instances_of = atomicLoad(&work[params.blocks.x + block * 2u]);
        let draws_of = atomicLoad(&work[params.blocks.x + block * 2u + 1u]);
        atomicStore(&work[params.blocks.x + block * 2u], place);
        atomicStore(&work[params.blocks.x + block * 2u + 1u], before_drawn);
        place += instances_of;
        before_drawn += draws_of;
    }
    let stats = params.regions.w;
    if thread == 0u {
        atomicStore(&work[params.blocks.z], record_totals.y);
        atomicAdd(&work[stats], record_totals.y);
        atomicAdd(&work[stats + 1u], record_totals.x);
    }

    // The templates.
    let templates = params.sizes.y;
    let packed = params.regions.z != 0u;
    let records = params.sizes.x;
    let item_span = (templates + THREADS - 1u) / THREADS;
    let item_begin = min(thread * item_span, templates);
    let item_end = min(item_begin + item_span, templates);
    var kept = 0u;
    var item_triangles = 0u;
    for (var item = item_begin; item < item_end; item++) {
        let at = params.frames.z + item * TEMPLATE;
        if second && levels[frames[at + 4u]] == frames[at + 5u] {
            kept += 1u;
            item_triangles += frames[at] / 3u;
        }
    }
    workgroupBarrier();
    sums[thread] = 0u;
    drawn[thread] = kept;
    let item_totals = sum_up(thread);
    var before_kept = drawn[thread];
    for (var item = item_begin; item < item_end; item++) {
        let at = params.frames.z + item * TEMPLATE;
        atomicStore(&work[params.work.z + item], before_kept);
        before_kept += select(0u, 1u, second && levels[frames[at + 4u]] == frames[at + 5u]);
    }
    storageBarrier();
    workgroupBarrier();
    for (var item = item_begin; item < item_end; item++) {
        let at = params.frames.z + item * TEMPLATE;
        let keep = second && levels[frames[at + 4u]] == frames[at + 5u];
        let region = frames[at + 6u];
        let start = frames[params.frames.x + region * 2u];
        var slot = item;
        if packed {
            if !keep {
                continue;
            }
            slot = start + atomicLoad(&work[params.work.z + item]) - atomicLoad(&work[params.work.z + start]);
        }
        write_args(records + slot, frames[at], select(0u, 1u, keep), frames[at + 1u], frames[at + 2u], frames[at + 3u]);
    }
    if thread == 0u {
        atomicAdd(&work[stats], item_totals.y);
        atomicAdd(&work[stats + 1u], item_totals.y);
    }
    atomicAdd(&work[stats + 2u], item_triangles);
    storageBarrier();
    workgroupBarrier();
    for (var region = thread; region < params.regions.y; region += THREADS) {
        let start = frames[params.frames.x + region * 2u];
        let end_of = start + frames[params.frames.x + region * 2u + 1u];
        var after = item_totals.y;
        if end_of < templates {
            after = atomicLoad(&work[params.work.z + end_of]);
        }
        var at_start = item_totals.y;
        if start < templates {
            at_start = atomicLoad(&work[params.work.z + start]);
        }
        atomicStore(&work[params.work.w + params.regions.x + region], after - at_start);
    }
}

@compute @workgroup_size(256)
fn tops_first(@builtin(local_invocation_id) local: vec3<u32>) {
    tops_in(local, false);
}

@compute @workgroup_size(256)
fn tops_second(@builtin(local_invocation_id) local: vec3<u32>) {
    tops_in(local, true);
}

// Each record: the place of its instances among the entries, and of its draw among the draws
// before it, from its block's.
@compute @workgroup_size(256)
fn place(@builtin(workgroup_id) workgroup: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let record = workgroup.x * THREADS + local.x;
    if record >= params.sizes.x {
        return;
    }
    let block = params.blocks.x + workgroup.x * 2u;
    atomicAdd(&work[params.work.x + record], params.sizes.w + atomicLoad(&work[block]));
    atomicAdd(&work[params.work.y + record], atomicLoad(&work[block + 1u]));
}

// Each record: the arguments of its draw, packed in its state's region after the draws before it;
// the first of each region counts its draws.
@compute @workgroup_size(256)
fn pack(@builtin(workgroup_id) workgroup: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let record = workgroup.x * THREADS + local.x;
    let records = params.sizes.x;
    if record >= records {
        return;
    }
    let count = atomicLoad(&work[record]);
    let at = params.statics.w + record * RECORD;
    let region = statics[at + 4u];
    let start = statics[params.statics.x + region * 2u];
    let before_start = atomicLoad(&work[params.work.y + start]);
    if record == start {
        let end_of = start + statics[params.statics.x + region * 2u + 1u];
        var after = atomicLoad(&work[params.blocks.z]);
        if end_of < records {
            after = atomicLoad(&work[params.work.y + end_of]);
        }
        atomicStore(&work[params.work.w + region], after - before_start);
    }
    var slot = record;
    if params.regions.z != 0u {
        if count == 0u {
            return;
        }
        slot = start + atomicLoad(&work[params.work.y + record]) - before_start;
    }
    write_args(slot, statics[at], count, statics[at + 1u], statics[at + 2u], atomicLoad(&work[params.work.x + record]));
}
