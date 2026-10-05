//! The order of loading, decided on the interface thread at each frame: the tiles wanted around
//! the camera, nearest first; the loads to start, at most a job per worker but one; those to
//! cancel, their tile out of the zone; and, beyond the GPU budget, the tiles to release.

use std::collections::HashSet;

use crate::model::{TILE, TileId};

/// How far from the point `eye` the centre of `tile` lies, on the ground.
pub fn distance(tile: TileId, eye: [f32; 2]) -> f32 {
    let [x, y] = tile.centre();
    ((x - eye[0]).powi(2) + (y - eye[1]).powi(2)).sqrt()
}

/// The tiles of the map, `tiles` at `y * 64 + x`, whose centre is within `radius` tiles of `eye`,
/// the nearest first: the nearer, the larger on screen.
pub fn wanted(tiles: &[bool], eye: [f32; 2], radius: u32) -> Vec<TileId> {
    let reach = (radius as f32 + 0.5) * TILE;
    let mut wanted: Vec<(f32, TileId)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, exists)| **exists)
        .map(|(index, _)| {
            let tile = TileId {
                x: index as u32 % 64,
                y: index as u32 / 64,
            };
            (distance(tile, eye), tile)
        })
        .filter(|(distance, _)| *distance <= reach)
        .collect();
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    wanted.into_iter().map(|(_, tile)| tile).collect()
}

/// What to do this frame: the tiles to load, the nearest first, and the loads to cancel.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub start: Vec<TileId>,
    pub cancel: Vec<TileId>,
}

/// The plan for `wanted`, given the tiles `loading` and those `present` (loaded, waiting to be
/// handed over, or refused): at most `slots` loads running at once.
pub fn plan(wanted: &[TileId], loading: &HashSet<TileId>, present: &HashSet<TileId>, slots: usize) -> Plan {
    let mut cancel: Vec<TileId> = loading.iter().filter(|tile| !wanted.contains(tile)).copied().collect();
    cancel.sort();
    let free = slots.saturating_sub(loading.len() - cancel.len());
    let start = wanted
        .iter()
        .filter(|tile| !loading.contains(tile) && !present.contains(tile))
        .take(free)
        .copied()
        .collect();
    Plan { start, cancel }
}

/// A tile on the GPU, for the budget: its place, its bytes, and the last frame it was in sight.
#[derive(Clone, Copy, Debug)]
pub struct Kept {
    pub tile: TileId,
    pub bytes: u64,
    pub seen: u64,
}

/// The tiles to release to come under `budget` bytes when `used` are taken: of those not in sight
/// at the frame `frame`, the longest unseen first, then the farthest from `eye`. Those in sight
/// are kept, even beyond the budget.
pub fn release(kept: &[Kept], frame: u64, eye: [f32; 2], used: u64, budget: u64) -> Vec<TileId> {
    let mut candidates: Vec<&Kept> = kept.iter().filter(|tile| tile.seen < frame).collect();
    candidates.sort_by(|a, b| {
        a.seen
            .cmp(&b.seen)
            .then(distance(b.tile, eye).total_cmp(&distance(a.tile, eye)))
    });
    let mut used = used;
    let mut released = Vec::new();
    for tile in candidates {
        if used <= budget {
            break;
        }
        used = used.saturating_sub(tile.bytes);
        released.push(tile.tile);
    }
    released
}
