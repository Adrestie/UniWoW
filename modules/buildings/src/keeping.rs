//! The buildings of the tiles held, by their unique id: a building listed by several tiles is one,
//! kept while one of them is held, as the client keeps one by its unique id.

use std::collections::{HashMap, HashSet};

use uniwow_api::formats::{Building, TileId};

#[derive(Default)]
pub struct Kept {
    /// The unique ids each tile held lists.
    tiles: HashMap<TileId, Vec<u32>>,
    /// Each building kept, as the first tile listing it placed it, and the tiles listing it.
    buildings: HashMap<u32, (Building, HashSet<TileId>)>,
}

impl Kept {
    /// Holds the tile `tile` listing `buildings`; the unique ids of those it brings, and of those it
    /// held before that no tile lists any more.
    pub fn hold(&mut self, tile: TileId, buildings: Vec<Building>) -> (Vec<u32>, Vec<u32>) {
        let mut gone = self.release(tile);
        let mut brought = Vec::new();
        let mut listed = Vec::with_capacity(buildings.len());
        for building in buildings {
            let id = building.unique_id;
            listed.push(id);
            let (_, tiles) = self.buildings.entry(id).or_insert_with(|| {
                brought.push(id);
                (building, HashSet::new())
            });
            tiles.insert(tile);
        }
        // Released then listed again: neither gone nor brought.
        let back: Vec<u32> = gone
            .iter()
            .filter(|id| self.buildings.contains_key(id))
            .copied()
            .collect();
        gone.retain(|id| !back.contains(id));
        brought.retain(|id| !back.contains(id));
        self.tiles.insert(tile, listed);
        (brought, gone)
    }

    /// Lets the tile `tile` go; the unique ids of the buildings no tile held lists any more.
    pub fn release(&mut self, tile: TileId) -> Vec<u32> {
        let mut gone = Vec::new();
        for id in self.tiles.remove(&tile).unwrap_or_default() {
            if let Some((_, tiles)) = self.buildings.get_mut(&id) {
                tiles.remove(&tile);
                if tiles.is_empty() {
                    self.buildings.remove(&id);
                    gone.push(id);
                }
            }
        }
        gone
    }

    /// Lets every tile go; the unique ids of the buildings kept.
    pub fn clear(&mut self) -> Vec<u32> {
        self.tiles.clear();
        self.buildings.drain().map(|(id, _)| id).collect()
    }

    pub fn get(&self, id: u32) -> Option<&Building> {
        self.buildings.get(&id).map(|(building, _)| building)
    }

    pub fn holds(&self, tile: TileId) -> bool {
        self.tiles.contains_key(&tile)
    }

    /// The tiles held.
    pub fn tiles(&self) -> HashSet<TileId> {
        self.tiles.keys().copied().collect()
    }

    /// How many buildings are kept.
    pub fn len(&self) -> usize {
        self.buildings.len()
    }
}
