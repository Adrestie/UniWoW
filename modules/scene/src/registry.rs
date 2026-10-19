//! The registry of the entities: the pieces the providers published, by slot, and the index of
//! their stable ids, made again by one job at a time from the changes gathered meanwhile. A piece
//! changed alone is swapped alone: the others are shared, never copied; the index is kept in two
//! copies, the one published and the one behind, so that a job changes in place the copy no
//! reader holds any more, the edits it missed applied first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use uniwow_api::formats::TileId;
use uniwow_api::scene::{Area, EntityId, Index, Kind, Listing, Piece, PieceKey, Placed, Snapshot, TileAreas, chunk_at};

/// A change asked of the registry, gathered until its next job.
pub enum Change {
    Publish(PieceKey, Arc<Piece>),
    Withdraw(PieceKey),
    WithdrawProvider(String, Option<u32>),
}

/// Where the registry finds the areas: `AreaTable.dbc`, and the areas of the chunks of a tile.
pub trait AreaSource {
    /// The areas by id; none while the client is not open.
    fn table(&self) -> Option<HashMap<u32, Area>>;
    /// The area of each chunk of the tile `tile` of the map `map`, at `y * 16 + x` of its index;
    /// `Ok(None)` where the map has no such tile, `Err` while the client is not open.
    fn tile(&self, map: u32, tile: TileId) -> Result<Option<Arc<[u32]>>, ()>;
}

/// What a job of the registry changed.
#[derive(Debug, Default)]
pub struct Summary {
    pub generation: u64,
    pub entities: usize,
    pub added: Vec<EntityId>,
    pub changed: Vec<EntityId>,
    pub removed: Vec<EntityId>,
    pub pieces: usize,
    pub took: Duration,
}

enum Edit {
    Add(EntityId, Listing),
    Remove(EntityId, Listing),
    /// A listing moved to another row of its slot, keeping its place among the listings.
    Move(EntityId, Listing, Listing),
}

fn apply(index: &mut Index, edits: &[Edit]) {
    for edit in edits {
        match edit {
            Edit::Add(id, listing) => index.entry(*id).or_default().push(*listing),
            Edit::Remove(id, listing) => {
                if let Some(listings) = index.get_mut(id) {
                    listings.retain(|found| found != listing);
                    if listings.is_empty() {
                        index.remove(id);
                    }
                }
            }
            Edit::Move(id, from, to) => {
                let listings = index.entry(*id).or_default();
                match listings.iter_mut().find(|found| **found == *from) {
                    Some(found) => *found = *to,
                    None => listings.push(*to),
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Registry {
    generation: u64,
    slots: Vec<Option<Arc<Placed>>>,
    free: Vec<u32>,
    keys: BTreeMap<PieceKey, u32>,
    current: Arc<Index>,
    spare: Arc<Index>,
    /// The edits applied to `current`, not yet to `spare`.
    behind: Vec<Edit>,
    areas: Option<Arc<HashMap<u32, Area>>>,
    tiles: HashMap<(u32, TileId), Option<Arc<[u32]>>>,
    shared_tiles: Arc<TileAreas>,
}

/// Whether the entity of two rows differs in what a snapshot gives of it.
fn differs(old: &Piece, old_row: usize, new: &Piece, new_row: usize) -> bool {
    old.flags[old_row] != new.flags[new_row]
        || old.label_of(old_row) != new.label_of(new_row)
        || old.extra[old_row] != new.extra[new_row]
        || old.origins[old_row] != new.origins[new_row]
        || old.bounds[old_row] != new.bounds[new_row]
        || (old.parents[old_row] == u32::MAX) != (new.parents[new_row] == u32::MAX)
        || (old.parents[old_row] != u32::MAX
            && old.ids[old.parents[old_row] as usize] != new.ids[new.parents[new_row] as usize])
}

impl Registry {
    /// The area of each entity of `piece`, of the map `map`: that of the chunk under its origin; an
    /// area's own, a weather's zone.
    fn areas_of(&mut self, map: u32, piece: &Piece, source: &dyn AreaSource) -> Box<[u32]> {
        piece
            .origins
            .iter()
            .zip(piece.ids.iter())
            .map(|(origin, id)| {
                // An area lies in itself, a weather in its zone.
                if matches!(id.kind, Kind::Area | Kind::Weather) {
                    return id.key as u32;
                }
                let Some((tile, [x, y])) = chunk_at(origin[0], origin[1]) else {
                    return 0;
                };
                let areas = match self.tiles.get(&(map, tile)) {
                    Some(areas) => areas.clone(),
                    None => match source.tile(map, tile) {
                        Ok(areas) => {
                            self.tiles.insert((map, tile), areas.clone());
                            areas
                        }
                        Err(()) => None,
                    },
                };
                areas.map_or(0, |areas| areas[(y * 16 + x) as usize])
            })
            .collect()
    }

    fn slot_for(&mut self, key: &PieceKey) -> u32 {
        if let Some(slot) = self.keys.get(key) {
            return *slot;
        }
        let slot = self.free.pop().unwrap_or_else(|| {
            self.slots.push(None);
            self.slots.len() as u32 - 1
        });
        self.keys.insert(key.clone(), slot);
        slot
    }

    /// Applies `changes`, the later of a piece winning; the snapshot made and what changed.
    pub fn apply(&mut self, changes: Vec<Change>, source: &dyn AreaSource) -> (Snapshot, Summary) {
        let started = Instant::now();
        let mut summary = Summary::default();
        // The areas once the client is open: those of the pieces already in, found again.
        let mut refresh = Vec::new();
        if self.areas.is_none()
            && let Some(table) = source.table()
        {
            self.areas = Some(Arc::new(table));
            refresh = self.keys.iter().map(|(key, slot)| (key.clone(), *slot)).collect();
        }
        let mut wanted: BTreeMap<PieceKey, Option<Arc<Piece>>> = BTreeMap::new();
        for change in changes {
            match change {
                Change::Publish(key, piece) => {
                    wanted.insert(key, Some(piece));
                }
                Change::Withdraw(key) => {
                    wanted.insert(key, None);
                }
                Change::WithdrawProvider(provider, map) => {
                    let keys: Vec<PieceKey> = self
                        .keys
                        .keys()
                        .chain(wanted.keys())
                        .filter(|key| key.provider == provider && map.is_none_or(|map| key.map == map))
                        .cloned()
                        .collect();
                    for key in keys {
                        wanted.insert(key, None);
                    }
                }
            }
        }
        for (key, slot) in refresh {
            if wanted.contains_key(&key) {
                continue;
            }
            if let Some(Some(placed)) = self.slots.get(slot as usize).cloned() {
                let areas = self.areas_of(key.map, &placed.piece, source);
                self.slots[slot as usize] = Some(Arc::new(Placed {
                    key,
                    generation: placed.generation,
                    piece: placed.piece.clone(),
                    areas,
                }));
            }
        }
        let generation = self.generation + 1;
        let mut edits = Vec::new();
        for (key, piece) in wanted {
            let old = self.keys.get(&key).and_then(|slot| self.slots[*slot as usize].clone());
            if old.is_none() && piece.is_none() {
                continue;
            }
            summary.pieces += 1;
            let slot = self.slot_for(&key);
            let old_rows: HashMap<EntityId, usize> = old
                .as_ref()
                .map(|old| old.piece.ids.iter().enumerate().map(|(row, id)| (*id, row)).collect())
                .unwrap_or_default();
            let mut kept = HashSet::new();
            if let Some(piece) = &piece {
                for (row, id) in piece.ids.iter().enumerate() {
                    let to = Listing { slot, row: row as u32 };
                    match old_rows.get(id) {
                        Some(old_row) => {
                            kept.insert(*id);
                            edits.push(Edit::Move(
                                *id,
                                Listing {
                                    slot,
                                    row: *old_row as u32,
                                },
                                to,
                            ));
                            let old = old.as_ref().map(|old| &old.piece);
                            if old.is_some_and(|old| differs(old, *old_row, piece, row)) {
                                summary.changed.push(*id);
                            }
                        }
                        None => {
                            edits.push(Edit::Add(*id, to));
                            summary.added.push(*id);
                        }
                    }
                }
            }
            for (id, row) in &old_rows {
                if !kept.contains(id) {
                    edits.push(Edit::Remove(*id, Listing { slot, row: *row as u32 }));
                    summary.removed.push(*id);
                }
            }
            match piece {
                Some(piece) => {
                    let areas = self.areas_of(key.map, &piece, source);
                    self.slots[slot as usize] = Some(Arc::new(Placed {
                        key,
                        generation,
                        piece,
                        areas,
                    }));
                }
                None => {
                    self.slots[slot as usize] = None;
                    self.keys.remove(&key);
                    self.free.push(slot);
                }
            }
        }
        // The copy behind brought level, then changed: copied only while a reader still holds it.
        let mut index = std::mem::take(&mut self.spare);
        let copy = Arc::make_mut(&mut index);
        apply(copy, &self.behind);
        apply(copy, &edits);
        self.spare = std::mem::replace(&mut self.current, index);
        self.behind = edits;
        self.generation = generation;
        let tiles: TileAreas = self
            .tiles
            .iter()
            .filter_map(|(key, areas)| Some((*key, areas.clone()?)))
            .collect();
        if tiles.len() != self.shared_tiles.len() {
            self.shared_tiles = Arc::new(tiles);
        }
        summary.generation = generation;
        summary.entities = self.current.len();
        let snapshot = Snapshot {
            generation,
            slots: Arc::new(self.slots.clone()),
            keys: Arc::new(self.keys.clone()),
            index: self.current.clone(),
            areas: self.areas.clone().unwrap_or_default(),
            tiles: self.shared_tiles.clone(),
            entities: summary.entities,
        };
        summary.took = started.elapsed();
        (snapshot, summary)
    }
}
