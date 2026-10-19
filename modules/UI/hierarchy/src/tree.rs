//! The tree of the main view of the Hierarchy, made by a job from a snapshot of the registry: the
//! map, its zones and their subzones as the chain of the areas, *No area*; in each place *Terrain*
//! (its chunks) and *Water*, then the buildings (each with its rooms, each room with the doodads it
//! holds), the doodads, and the rest, each sorted by name; beside the map, *Live*. Only the entities
//! a filter keeps, with the game parents of those it keeps.

use std::collections::{HashMap, HashSet};

use uniwow_api::formats::TileId;
use uniwow_api::scene::{EntityId, EntityRef, Kind, Snapshot, chunk_at};

/// A node of the tree: an entity, a place or a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// What names it across the trees made again, for the nodes unfolded: an entity's id, `area:87`,
    /// `terrain:87`.
    pub key: String,
    pub label: String,
    pub entity: Option<EntityId>,
    /// The area of a place.
    pub area: Option<u32>,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub roots: Vec<u32>,
    pub by_entity: HashMap<EntityId, u32>,
    /// The entities it lists.
    pub entities: usize,
}

/// What the tree keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// In the name or the label, in lower case.
    pub text: String,
    /// The kinds, every one when empty.
    pub kinds: Vec<Kind>,
    /// The map, the tile of the camera and the tiles around it; none for every entity held.
    pub scope: Option<(u32, TileId, u32)>,
    /// Kept whatever the scope: the entities selected.
    pub kept: Vec<EntityId>,
}

/// The order of the entities of a place by their kind: the terrain and the water in their folders
/// first, then the buildings, the doodads, and the rest.
fn rank(kind: Kind) -> u8 {
    match kind {
        Kind::Chunk => 0,
        Kind::Water => 1,
        Kind::Group | Kind::Instance => 2,
        Kind::Building => 3,
        Kind::Room => 4,
        Kind::BuildingDoodad | Kind::Doodad => 5,
        Kind::Creature | Kind::GameObject => 6,
        _ => 7,
    }
}

impl Tree {
    fn add(&mut self, node: Node) -> u32 {
        let at = self.nodes.len() as u32;
        match node.parent {
            Some(parent) => self.nodes[parent as usize].children.push(at),
            None => self.roots.push(at),
        }
        if let Some(entity) = node.entity {
            self.by_entity.insert(entity, at);
        }
        self.nodes.push(node);
        at
    }

    /// The node of `key` under `parent`, made the first time.
    fn folder(
        &mut self,
        folders: &mut HashMap<String, u32>,
        key: String,
        label: &str,
        parent: Option<u32>,
        area: Option<u32>,
    ) -> u32 {
        if let Some(found) = folders.get(&key) {
            return *found;
        }
        let at = self.add(Node {
            key: key.clone(),
            label: label.to_owned(),
            entity: None,
            area,
            parent,
            children: Vec::new(),
        });
        folders.insert(key, at);
        at
    }

    /// The chain of `node`'s parents, from the root down to it.
    pub fn path(&self, node: u32) -> Vec<u32> {
        let mut path = vec![node];
        let mut at = self.nodes[node as usize].parent;
        while let Some(parent) = at {
            path.push(parent);
            at = self.nodes[parent as usize].parent;
        }
        path.reverse();
        path
    }

    /// The rows shown with the nodes `unfolded` unfolded: each node and its depth, in order.
    pub fn rows(&self, unfolded: &HashSet<String>) -> Vec<(u32, u16)> {
        let mut rows = Vec::new();
        let mut stack: Vec<(u32, u16)> = self.roots.iter().rev().map(|root| (*root, 0)).collect();
        while let Some((node, depth)) = stack.pop() {
            rows.push((node, depth));
            let at = &self.nodes[node as usize];
            if unfolded.contains(&at.key) {
                stack.extend(at.children.iter().rev().map(|child| (*child, depth + 1)));
            }
        }
        rows
    }
}

/// The name of `entity` shown: the one the user gave, else its default.
pub fn name_of(entity: &EntityRef, names: &HashMap<EntityId, String>) -> String {
    names.get(&entity.id()).cloned().unwrap_or_else(|| entity.name())
}

/// The tree of the entities of `snapshot` that `filter` keeps, the map named `map`.
pub fn build(snapshot: &Snapshot, filter: &Filter, names: &HashMap<EntityId, String>, map: &str) -> Tree {
    let in_scope = |entity: &EntityRef| {
        let Some((shown, tile, tiles)) = filter.scope else {
            return true;
        };
        let [x, y, _] = entity.origin();
        entity.placed.key.map == shown
            && chunk_at(x, y).is_some_and(|(at, _)| at.x.abs_diff(tile.x).max(at.y.abs_diff(tile.y)) <= tiles)
    };
    let matches = |entity: &EntityRef| {
        (filter.kinds.is_empty() || filter.kinds.contains(&entity.id().kind))
            && (filter.text.is_empty()
                || name_of(entity, names).to_lowercase().contains(&filter.text)
                || entity.label().to_lowercase().contains(&filter.text))
    };
    // The entities kept, then their game parents.
    let mut kept: Vec<EntityRef> = snapshot
        .entities()
        .filter(|entity| (in_scope(entity) && matches(entity)) || filter.kept.contains(&entity.id()))
        .collect();
    let mut seen: HashSet<EntityId> = kept.iter().map(|entity| entity.id()).collect();
    let mut index = 0;
    while index < kept.len() {
        if let Some(parent) = kept[index].parent()
            && seen.insert(parent)
            && let Some(found) = snapshot.get(&parent)
        {
            kept.push(found);
        }
        index += 1;
    }
    let mut tree = Tree::default();
    let mut folders = HashMap::new();
    let root = tree.folder(
        &mut folders,
        "map".to_owned(),
        if map.is_empty() { "Map" } else { map },
        None,
        None,
    );
    // The places first, so that the entities find them; parents before children.
    kept.sort_by_key(|entity| {
        let mut depth = 0;
        let mut at = entity.parent();
        while let Some(parent) = at {
            depth += 1;
            at = snapshot.get(&parent).and_then(|found| found.parent());
            if depth > 16 {
                break;
            }
        }
        depth
    });
    for entity in &kept {
        let id = entity.id();
        let parent = match entity.parent().and_then(|parent| tree.by_entity.get(&parent).copied()) {
            Some(parent) => parent,
            None if id.kind == Kind::Live => tree.folder(&mut folders, "live".to_owned(), "Live", None, None),
            None => {
                let chain = snapshot.place(entity.area());
                let mut place = if chain.is_empty() {
                    tree.folder(&mut folders, "area:0".to_owned(), "No area", Some(root), Some(0))
                } else {
                    root
                };
                for (area, name) in chain {
                    place = tree.folder(&mut folders, format!("area:{area}"), name, Some(place), Some(area));
                }
                let area = tree.nodes[place as usize].area.unwrap_or(0);
                match id.kind {
                    Kind::Chunk => tree.folder(&mut folders, format!("terrain:{area}"), "Terrain", Some(place), None),
                    Kind::Water => tree.folder(&mut folders, format!("water:{area}"), "Water", Some(place), None),
                    _ => place,
                }
            }
        };
        tree.add(Node {
            key: id.to_string(),
            label: name_of(entity, names),
            entity: Some(id),
            area: None,
            parent: Some(parent),
            children: Vec::new(),
        });
        tree.entities += 1;
    }
    // Each node's children: the terrain and the water, the places by name, then the entities by kind
    // and name.
    let order: Vec<(u8, String)> = tree
        .nodes
        .iter()
        .map(|node| {
            let rank = match node.entity {
                None if node.key.starts_with("terrain:") => 0,
                None if node.key.starts_with("water:") => 1,
                None => 2,
                Some(id) => 3 + rank(id.kind),
            };
            (rank, node.label.to_lowercase())
        })
        .collect();
    for node in &mut tree.nodes {
        node.children
            .sort_by(|a, b| order[*a as usize].cmp(&order[*b as usize]));
    }
    tree
}
