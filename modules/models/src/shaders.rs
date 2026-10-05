//! The shader of each batch of a skin, chosen at load as WotLK chooses it: from the blending of its
//! material, the coordinates of its textures and, when the model has them, its combiners
//! (`sub_836980`); then the layers of a submesh merged into its first where they can be drawn in
//! one (`sub_837680`); then the names of its vertex and pixel shaders. Translated from the
//! implementation of the Wowser project published on wowdev, *M2/.skin/WotLK shader selection*
//! (see THIRD_PARTY.md); the formulas of the pixel shaders are those of *M2/Rendering*.

use uniwow_api::formats::{Model, Skin};

/// The flag of a model whose batches take their combiners from `combiner_combos`.
const COMBINERS: u32 = 0x08;
/// A shader already chosen, not to be chosen again; alone, a layer merged into its first.
const CHOSEN: u16 = 0x8000;
/// The coordinates of the environment, among the combos of coordinates (-1).
const ENVIRONMENT: i32 = -1;

/// Where a texture takes its coordinates: the first set, the second, or the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coords {
    T1 = 0,
    T2 = 1,
    Env = 2,
}

/// The pixel shaders of WotLK the models use, by the number the shader of `models` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combiner {
    Opaque = 0,
    Mod,
    Decal,
    Add,
    Mod2x,
    Fade,
    OpaqueOpaque,
    OpaqueMod,
    OpaqueAdd,
    OpaqueMod2x,
    OpaqueMod2xNa,
    OpaqueAddNa,
    ModOpaque,
    ModAdd,
    ModMod2x,
    ModMod2xNa,
    ModAddNa,
    ModMod,
    AddMod,
    Mod2xMod2x,
    OpaqueMod2xNaAlpha,
    OpaqueAddAlpha,
    OpaqueAddAlphaAlpha,
}

/// How a batch is drawn: its pixel shader, where its textures take their coordinates, and its
/// textures by their index among the model's, one or two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shader {
    pub combiner: Combiner,
    pub coords: [Coords; 2],
    pub textures: Vec<usize>,
}

/// What WotLK computes for a batch at load.
#[derive(Clone, Debug)]
struct Runtime {
    shader: u16,
    ops: usize,
    textures: Vec<usize>,
}

/// The version of the models of 3.3.5a, whose shaders are chosen so; a later one names its own.
const WOTLK: u32 = 264;

/// The shaders of the batches of `skin` of `model`, in their order: none for a layer merged into
/// its first, which is not drawn. A shader WotLK names none for, and every batch of a model of a
/// later version, is drawn with its first texture alone, opaque or modulated by its blending.
pub fn select(model: &Model, skin: &Skin) -> Vec<Option<Shader>> {
    let mapping = |at: usize| model.uv_combos.get(at).map(|value| i32::from(*value as i16));
    let mut runtime: Vec<Runtime> = skin
        .batches
        .iter()
        .map(|batch| {
            let ops = usize::from(batch.texture_count).min(2);
            Runtime {
                shader: batch.shader,
                ops,
                textures: (0..ops)
                    .map(|op| usize::from(model.texture_combos[usize::from(batch.texture_combo) + op]))
                    .collect(),
            }
        })
        .collect();
    if model.version == WOTLK {
        choose(model, skin, &mut runtime, &mapping);
        merge(model, skin, &mut runtime, &mapping);
    }
    skin.batches
        .iter()
        .zip(&runtime)
        .map(|(batch, runtime)| {
            if runtime.shader == CHOSEN {
                return None;
            }
            let first = mapping(usize::from(batch.uv_combo));
            let named = if model.version != WOTLK {
                None
            } else if runtime.shader & CHOSEN == 0 {
                table(runtime.shader, runtime.ops, first).or_else(|| table(0x11, runtime.ops, first))
            } else {
                chosen(runtime.shader)
            };
            let blending = model.materials[usize::from(batch.material)].blending;
            let (combiner, coords) = named.unwrap_or(if blending == 0 {
                (Combiner::Opaque, [Coords::T1; 2])
            } else {
                (Combiner::Mod, [Coords::T1; 2])
            });
            let textures = match combiner as u32 {
                kind if kind <= Combiner::Fade as u32 => runtime.textures.iter().take(1).copied().collect(),
                _ => runtime.textures.clone(),
            };
            Some(Shader {
                combiner,
                coords,
                textures,
            })
        })
        .collect()
}

/// The shader of each batch from its blending and its coordinates, or its combiners (`sub_836980`).
fn choose(model: &Model, skin: &Skin, runtime: &mut [Runtime], mapping: &dyn Fn(usize) -> Option<i32>) {
    let combiners = model.flags & COMBINERS != 0;
    for (batch, runtime) in skin.batches.iter().zip(runtime) {
        if runtime.shader & CHOSEN != 0 {
            continue;
        }
        let blending = model.materials[usize::from(batch.material)].blending;
        if !combiners {
            let first = mapping(usize::from(batch.uv_combo));
            let mut shader = 0;
            if blending != 0 {
                shader = 1;
                if first == Some(ENVIRONMENT) {
                    shader |= 8;
                }
            }
            shader *= 16;
            if first == Some(1) {
                shader |= 0x4000;
            }
            runtime.shader = shader;
        } else if runtime.ops > 0 {
            let mut ops = [0u16; 2];
            let mut shader = 0;
            for (op, slot) in ops.iter_mut().enumerate().take(runtime.ops) {
                let mut combiner = model
                    .combiner_combos
                    .get(usize::from(runtime.shader) + op)
                    .copied()
                    .unwrap_or(0);
                if op == 0 && blending == 0 {
                    combiner = 0;
                }
                let coordinates = mapping(usize::from(batch.uv_combo) + op);
                *slot = if coordinates == Some(ENVIRONMENT) {
                    combiner | 8
                } else {
                    combiner
                };
                if coordinates == Some(1) && op + 1 == runtime.ops {
                    shader |= 0x4000;
                }
            }
            runtime.shader = shader | ops[1] | (ops[0] * 16);
        }
    }
}

/// The layers of a submesh merged into its first where they can be drawn in one (`sub_837680`).
fn merge(model: &Model, skin: &Skin, runtime: &mut [Runtime], mapping: &dyn Fn(usize) -> Option<i32>) {
    if skin.batches.iter().all(|batch| batch.layer == 0) {
        return;
    }
    let material = |index: usize| &model.materials[usize::from(skin.batches[index].material)];
    let weight = |index: usize| model.weight_combos[usize::from(skin.batches[index].weight_combo)];
    let mut state = [0u8; 2];
    let mut first = 0;
    let mut previous: Option<usize> = None;
    let mut shared = false;
    for current in 0..skin.batches.len() {
        let batch = &skin.batches[current];
        if previous.is_some_and(|before| skin.batches[before].material == batch.material) {
            shared = true;
            continue;
        }
        previous = Some(current);
        let here = mapping(usize::from(batch.uv_combo));
        let next = mapping(usize::from(batch.uv_combo) + 1);
        let low = runtime[current].shader & 7;
        let blending = material(current).blending;
        if batch.layer == 0 {
            state = [0, 0];
            if runtime[current].ops >= 1 && blending == 0 {
                runtime[current].shader &= 0xFF8F;
            }
            first = current;
        }
        let flags_differ = (material(current).flags ^ material(first).flags) & 0x01 != 0;
        let same_weight = weight(first) == weight(current);
        let mut skip = false;
        if state[0] != 0 {
            if state[0] == 1 {
                if matches!(blending, 1 | 2)
                    && runtime[current].ops == 1
                    && !flags_differ
                    && runtime[current].textures.first() == runtime[first].textures.first()
                    && same_weight
                {
                    runtime[current].shader = CHOSEN;
                    runtime[first].shader = CHOSEN | 1;
                    state[0] = 3;
                    continue;
                }
                state[0] = 0;
            } else {
                skip = true;
            }
        }
        if !skip
            && blending == 0
            && runtime[current].ops == 2
            && matches!(low, 4 | 6)
            && here == Some(0)
            && next == Some(ENVIRONMENT)
        {
            state[0] = 1;
        }
        if state[1] != 0 {
            if state[1] == 1 {
                if !matches!(blending, 4 | 6)
                    || runtime[current].ops != 1
                    || here.is_some_and(|at| (0..=2).contains(&at))
                {
                    state[1] = 0;
                } else if same_weight {
                    state[1] = 2;
                    runtime[current].shader = CHOSEN;
                    runtime[first].shader = if blending != 4 { 0xE } else { CHOSEN | 2 };
                    runtime[first].ops = 2;
                    let added = runtime[current].textures.first().copied();
                    let kept = runtime[first].textures.first().copied();
                    runtime[first].textures = kept.into_iter().chain(added).collect();
                    continue;
                }
            } else {
                if state[1] != 2 {
                    continue;
                }
                if !matches!(blending, 1 | 2)
                    || runtime[current].ops != 1
                    || flags_differ
                    || runtime[first].textures.first() != runtime[current].textures.first()
                {
                    state[1] = 0;
                } else if same_weight {
                    state[1] = 3;
                    runtime[current].shader = CHOSEN;
                    runtime[first].shader = if runtime[first].shader == (CHOSEN | 2) {
                        CHOSEN | 3
                    } else {
                        CHOSEN | 1
                    };
                    continue;
                }
            }
        }
        if blending == 0 && runtime[current].ops == 1 && here == Some(0) {
            state[1] = 1;
        }
    }
    // The batches sharing the material of the one before take what it became.
    if shared {
        let mut previous: Option<usize> = None;
        for current in 0..skin.batches.len() {
            match previous {
                Some(before) if skin.batches[before].material == skin.batches[current].material => {
                    runtime[current] = runtime[before].clone();
                }
                _ => previous = Some(current),
            }
        }
    }
}

/// The shaders of the table, by the shader computed, its count of textures and the coordinates of
/// its first; none for a pair of combiners WotLK has no shader for. A model of 3.3.5a without
/// combos of coordinates, brought back from a later client, takes the first set.
fn table(shader: u16, ops: usize, first: Option<i32>) -> Option<(Combiner, [Coords; 2])> {
    let (high, low) = ((shader >> 4) & 7, shader & 7);
    let (high_env, low_env) = ((shader >> 4) & 8 != 0, shader & 8 != 0);
    if ops <= 1 {
        let coords = if high_env {
            Coords::Env
        } else if matches!(first, Some(0) | None) {
            Coords::T1
        } else {
            Coords::T2
        };
        let combiner = match high {
            0 => Combiner::Opaque,
            2 => Combiner::Decal,
            3 => Combiner::Add,
            4 => Combiner::Mod2x,
            5 => Combiner::Fade,
            _ => Combiner::Mod,
        };
        return Some((combiner, [coords, Coords::T1]));
    }
    let coords = match (high_env, low_env) {
        (true, true) => [Coords::Env, Coords::Env],
        (true, false) => [Coords::Env, Coords::T2],
        (false, true) => [Coords::T1, Coords::Env],
        (false, false) => [Coords::T1, Coords::T2],
    };
    let combiner = match (high, low) {
        (0, 0) => Combiner::OpaqueOpaque,
        (0, 3) => Combiner::OpaqueAdd,
        (0, 4) => Combiner::OpaqueMod2x,
        (0, 6) => Combiner::OpaqueMod2xNa,
        (0, 7) => Combiner::OpaqueAddNa,
        (0, _) => Combiner::OpaqueMod,
        (1, 0) => Combiner::ModOpaque,
        (1, 3) => Combiner::ModAdd,
        (1, 4) => Combiner::ModMod2x,
        (1, 6) => Combiner::ModMod2xNa,
        (1, 7) => Combiner::ModAddNa,
        (1, _) => Combiner::ModMod,
        (3, 1) => Combiner::AddMod,
        (4, 4) => Combiner::Mod2xMod2x,
        _ => return None,
    };
    Some((combiner, coords))
}

/// The shaders a merge of layers chose; none for another.
fn chosen(shader: u16) -> Option<(Combiner, [Coords; 2])> {
    let combiner = match shader & !CHOSEN {
        1 => Combiner::OpaqueMod2xNaAlpha,
        2 => Combiner::OpaqueAddAlpha,
        3 => Combiner::OpaqueAddAlphaAlpha,
        _ => return None,
    };
    Some((combiner, [Coords::T1, Coords::Env]))
}
