//! What the buildings tell the budget of the view and what it lets them load, as the terrain: each
//! file by the distance of the nearest of its buildings, held or wanted in that band; the files
//! read the nearest first within the reach the budget allows to load, as many at once as the
//! workers but one, and let go beyond the reach it allows to keep.

use uniwow_api::formats::{Building, ORIGIN};
use uniwow_api::viewport::{Allowance, Demand};

/// What a file wanted to load is not yet known to take: about a building of a few groups.
pub const EXPECTED: u64 = 4 << 20;

/// Where a file stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    Waiting,
    Loading,
    /// On the GPU, taking so many bytes.
    Ready(u64),
    Refused,
}

/// A file as the budget sees it: its key, the distance of its nearest building, where it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct File<K> {
    pub key: K,
    pub distance: f32,
    pub held: Held,
}

/// The rectangle a building covers on the ground, from its bounds in the axes of its file.
pub fn ground(building: &Building) -> [[f32; 2]; 2] {
    let [low, high] = building.bounds;
    [[ORIGIN - high[2], ORIGIN - high[0]], [ORIGIN - low[2], ORIGIN - low[0]]]
}

/// How far `eye` lies from the rectangle `ground`, on the ground; none inside it.
pub fn distance(eye: [f32; 2], ground: [[f32; 2]; 2]) -> f32 {
    let [low, high] = ground;
    let away = |at: f32, low: f32, high: f32| (low - at).max(at - high).max(0.0);
    away(eye[0], low[0], high[0]).hypot(away(eye[1], low[1], high[1]))
}

/// What the files tell the budget, besides `fixed`: those on the GPU held and wanted in the band
/// of their distance, those not yet wanted at `expected` each.
pub fn demand<K>(files: &[File<K>], fixed: u64, expected: u64) -> Demand {
    let mut demand = Demand {
        fixed,
        ..Demand::default()
    };
    for file in files {
        let band = Demand::band(file.distance);
        match file.held {
            Held::Ready(bytes) => {
                demand.held[band] += bytes;
                demand.wanted[band] += bytes;
            }
            Held::Waiting | Held::Loading => demand.wanted[band] += expected,
            Held::Refused => {}
        }
    }
    demand
}

/// What the files to read are expected to take each: what those on the GPU take on average, or
/// `EXPECTED` while none is.
pub fn expected<K>(files: &[File<K>]) -> u64 {
    let (count, bytes) = files.iter().fold((0, 0), |(count, bytes), file| match file.held {
        Held::Ready(held) => (count + 1, bytes + held),
        _ => (count, bytes),
    });
    bytes.checked_div(count).unwrap_or(EXPECTED)
}

/// What the budget lets the files do: those to read, those to let go, and whether some it lets
/// load still wait for their turn.
#[derive(Debug, PartialEq)]
pub struct Plan<K> {
    pub start: Vec<K>,
    pub release: Vec<K>,
    pub waiting: bool,
}

/// The files to read, the nearest first, within the reach `allowance` lets load, so many that no
/// more than `slots` are read at once; and those on the GPU to let go, beyond the reach it lets
/// keep.
pub fn plan<K: Clone>(files: &[File<K>], allowance: &Allowance, slots: usize) -> Plan<K> {
    let loading = files.iter().filter(|file| file.held == Held::Loading).count();
    let mut waiting: Vec<&File<K>> = files
        .iter()
        .filter(|file| file.held == Held::Waiting && file.distance <= allowance.load)
        .collect();
    waiting.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    let free = slots.saturating_sub(loading);
    Plan {
        waiting: waiting.len() > free,
        start: waiting.into_iter().take(free).map(|file| file.key.clone()).collect(),
        release: files
            .iter()
            .filter(|file| matches!(file.held, Held::Ready(_)) && file.distance > allowance.keep)
            .map(|file| file.key.clone())
            .collect(),
    }
}
