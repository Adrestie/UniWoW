//! The order of loading, decided on the interface thread at each frame: the tiles wanted around
//! the camera, nearest first, full near it and light beyond; as far as the GPU budget of the view
//! lets them reach, shared with the other layers; the loads to start, at most a job per worker but
//! one and never beyond the budget; those to cancel; and the tiles to release, those no longer
//! wanted first, then the farthest. Nothing here depends on where the camera looks: turning it
//! loads and releases nothing.

use std::collections::{HashMap, HashSet};

use uniwow_api::viewport::{Allowance, Demand};

use crate::model::{TILE, TileId};

/// How far from the point `eye` the centre of `tile` lies, on the ground, in yards.
pub fn distance(tile: TileId, eye: [f32; 2]) -> f32 {
    let [x, y] = tile.centre();
    ((x - eye[0]).powi(2) + (y - eye[1]).powi(2)).sqrt()
}

/// The tiles of the map, `tiles` at `y * 64 + x`, whose centre is within `radius` tiles of `eye`,
/// the nearest first, with their distance in tiles: the nearer, the larger on screen.
pub fn wanted(tiles: &[bool], eye: [f32; 2], radius: u32) -> Vec<(TileId, f32)> {
    let reach = radius as f32 + 0.5;
    let mut wanted: Vec<(TileId, f32)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, exists)| **exists)
        .map(|(index, _)| {
            let tile = TileId {
                x: index as u32 % 64,
                y: index as u32 / 64,
            };
            (tile, distance(tile, eye) / TILE)
        })
        .filter(|(_, distance)| *distance <= reach)
        .collect();
    wanted.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    wanted
}

/// A tile drawn with all its vertices and its model kept, or light: the corners of its chunks only,
/// its blending reduced, no model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Full,
    Light,
}

/// Within the first distance of the camera, in tiles from their centre, the tiles are full, a
/// little beyond where they need finer than the corners of their chunks; a full one stays so within
/// the second.
pub const FULL: [f32; 2] = [7.0, 8.0];

/// The kind a tile `distance` tiles away should be, `now` the kind it has or is loading as, and
/// `changed` whether its model holds changes, which keep it full.
pub fn kind(distance: f32, now: Option<Kind>, changed: bool) -> Kind {
    let limit = if now == Some(Kind::Full) { FULL[1] } else { FULL[0] };
    if changed || distance <= limit {
        Kind::Full
    } else {
        Kind::Light
    }
}

/// A tile held, drawn or ready to be.
#[derive(Clone, Copy, Debug)]
pub struct Held {
    pub kind: Kind,
    pub bytes: u64,
    pub changed: bool,
}

/// What a tile of each kind is expected to take on the GPU.
#[derive(Clone, Copy, Debug)]
pub struct Costs {
    pub full: u64,
    pub light: u64,
}

/// What a tile of each kind takes, before any is held: a full tile's vertices (1.2 MB), its
/// triangles (0.65 MB) and its blending (4 MB); a light tile's corners and its blending (0.26 MB).
pub const DEFAULT_COSTS: Costs = Costs {
    full: 6 << 20,
    light: 300 << 10,
};

/// What a tile of each kind is expected to take: the mean of those held, or `DEFAULT_COSTS`.
pub fn costs(held: &HashMap<TileId, Held>) -> Costs {
    let mean = |kind: Kind, default: u64| {
        let (sum, count) = held
            .values()
            .filter(|held| held.kind == kind)
            .fold((0, 0), |(sum, count), held| (sum + held.bytes, count + 1));
        sum.checked_div(count).unwrap_or(default)
    };
    Costs {
        full: mean(Kind::Full, DEFAULT_COSTS.full),
        light: mean(Kind::Light, DEFAULT_COSTS.light),
    }
}

impl Costs {
    fn of(&self, kind: Kind) -> u64 {
        match kind {
            Kind::Full => self.full,
            Kind::Light => self.light,
        }
    }
}

/// What the planning reads.
pub struct Inputs<'a> {
    /// The tiles within reach, the nearest first, with their distance in tiles.
    pub wanted: &'a [(TileId, f32)],
    pub held: &'a HashMap<TileId, Held>,
    /// The loads running, by tile, with the kind each loads.
    pub loading: &'a HashMap<TileId, Kind>,
    /// The tiles that could not be read, never asked again.
    pub refused: &'a HashSet<TileId>,
    /// All the terrain takes on the GPU, and what it may take of the budget of the view: the budget
    /// less what the other layers hold.
    pub used: u64,
    pub budget: u64,
    /// What the budget of the view allows every layer: how far the loads go, and what is kept.
    pub allowance: Allowance,
    pub costs: Costs,
    /// The loads that may run at once.
    pub slots: usize,
}

/// What to do this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The loads to start, the nearest first, with their kind.
    pub start: Vec<(TileId, Kind)>,
    pub cancel: Vec<TileId>,
    pub release: Vec<TileId>,
    /// When the budget holds fewer tiles than are wanted: the distance in tiles of the farthest it
    /// holds, the reach left.
    pub limited: Option<f32>,
}

/// What the terrain tells the budget of the view: what it takes outside its tiles, `fixed`; the
/// bytes of the tiles held, by their distance from `eye`; and those of the tiles wanted, at the kind
/// each wants, as they take when held so, as `costs` expects otherwise.
pub fn demand(inputs: &Inputs, eye: [f32; 2], fixed: u64) -> Demand {
    let mut demand = Demand {
        fixed,
        ..Demand::default()
    };
    for (tile, held) in inputs.held {
        demand.held[Demand::band(distance(*tile, eye))] += held.bytes;
    }
    for &(tile, tiles) in inputs.wanted {
        if inputs.refused.contains(&tile) {
            continue;
        }
        let held = inputs.held.get(&tile);
        let now = held
            .map(|held| held.kind)
            .or_else(|| inputs.loading.get(&tile).copied());
        let kind = kind(tiles, now, held.is_some_and(|held| held.changed));
        let bytes = held
            .filter(|held| held.kind == kind)
            .map_or(inputs.costs.of(kind), |held| held.bytes);
        demand.wanted[Demand::band(tiles * TILE)] += bytes;
    }
    demand
}

/// The plan for this frame.
pub fn plan(inputs: &Inputs) -> Plan {
    let Inputs {
        held, loading, costs, ..
    } = *inputs;
    let desired: Vec<(TileId, f32, Kind)> = inputs
        .wanted
        .iter()
        .filter(|(tile, _)| !inputs.refused.contains(tile))
        .map(|&(tile, distance)| {
            let now = held
                .get(&tile)
                .map(|held| held.kind)
                .or_else(|| loading.get(&tile).copied());
            let changed = held.get(&tile).is_some_and(|held| held.changed);
            (tile, distance, kind(distance, now, changed))
        })
        .collect();
    // The tiles wanted the budget of the view lets the loads reach, the nearest first, and those it
    // keeps: between the two, a tile at the edge is neither loaded nor released.
    let within = |reach: f32| {
        desired
            .iter()
            .take_while(|(_, distance, _)| distance * TILE < reach)
            .count()
    };
    let (loaded, kept) = (within(inputs.allowance.load), within(inputs.allowance.keep));
    let to_load: HashMap<TileId, Kind> = desired[..loaded].iter().map(|(tile, _, kind)| (*tile, *kind)).collect();
    let to_keep: HashSet<TileId> = desired[..kept].iter().map(|(tile, ..)| *tile).collect();
    let wanted_at: HashMap<TileId, f32> = desired.iter().map(|(tile, distance, _)| (*tile, *distance)).collect();

    let mut cancel: Vec<TileId> = loading
        .iter()
        .filter(|(tile, kind)| to_load.get(tile) != Some(kind))
        .map(|(tile, _)| *tile)
        .collect();
    cancel.sort();
    let in_flight: u64 = loading
        .iter()
        .filter(|(tile, _)| !cancel.contains(tile))
        .map(|(_, kind)| costs.of(*kind))
        .sum();
    let missing: Vec<(TileId, Kind)> = desired[..loaded]
        .iter()
        .filter(|(tile, _, kind)| {
            held.get(tile).map(|held| held.kind) != Some(*kind) && loading.get(tile) != Some(kind)
        })
        .map(|(tile, _, kind)| (*tile, *kind))
        .collect();

    // Released while the tiles held, those loading and those missing would go beyond the budget:
    // the tiles no longer wanted, the farthest first; then those the budget no longer holds; then
    // those held beyond the share of the loads. Never a tile the loads want, nor one whose model
    // holds changes.
    let mut candidates: Vec<(u8, f32, TileId, u64)> = held
        .iter()
        .filter(|(tile, held)| !to_load.contains_key(tile) && !held.changed)
        .map(|(tile, held)| {
            let rank = match wanted_at.get(tile) {
                None => 0,
                Some(_) if !to_keep.contains(tile) => 1,
                Some(_) => 2,
            };
            let far = wanted_at.get(tile).copied().unwrap_or(f32::MAX);
            (rank, far, *tile, held.bytes)
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)).then(a.2.cmp(&b.2)));
    let needed: u64 = missing.iter().map(|(_, kind)| costs.of(*kind)).sum();
    let mut projected = inputs.used + in_flight + needed;
    let mut release = Vec::new();
    let mut released = 0;
    for (_, _, tile, bytes) in candidates {
        if projected <= inputs.budget {
            break;
        }
        projected = projected.saturating_sub(bytes);
        released += bytes;
        release.push(tile);
    }

    // Started in order while a slot is free and the budget holds them.
    let free = inputs.slots.saturating_sub(loading.len() - cancel.len());
    let mut projected = (inputs.used + in_flight).saturating_sub(released);
    let mut start = Vec::new();
    for (tile, kind) in missing {
        if start.len() >= free || projected + costs.of(kind) > inputs.budget {
            break;
        }
        projected += costs.of(kind);
        start.push((tile, kind));
    }
    let limited = (loaded < desired.len()).then(|| loaded.checked_sub(1).map_or(0.0, |last| desired[last].1));
    Plan {
        start,
        cancel,
        release,
        limited,
    }
}
