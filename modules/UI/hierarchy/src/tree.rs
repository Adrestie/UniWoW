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
    /// The places of the entities it lists that the scene does not hold, known of their providers
    /// far from the camera: framed to be loaded.
    pub known: HashMap<EntityId, [f32; 3]>,
}

/// What the tree keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// The view the tree is made for.
    pub view: View,
    /// In the name or the label, in lower case.
    pub text: String,
    /// The kinds, every one when empty.
    pub kinds: Vec<Kind>,
    /// The map, the tile of the camera and the tiles around it; none for every entity held.
    pub scope: Option<(u32, TileId, u32)>,
    /// Kept whatever the scope: the entities selected.
    pub kept: Vec<EntityId>,
}

/// The views of the Hierarchy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// The places of the game: map, zones, subzones.
    #[default]
    Main,
    /// The tiles, each with what its file stores.
    Files,
    /// A flat list a kind.
    ByType,
    /// The spawns of the server by their template.
    Server,
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
            // An entity listed under several nodes is found by its first.
            self.by_entity.entry(entity).or_insert(at);
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

/// The node of the place `area` under `root`, its chain of zones made the first time; `No area`
/// for none.
fn place_of(tree: &mut Tree, folders: &mut HashMap<String, u32>, snapshot: &Snapshot, root: u32, area: u32) -> u32 {
    let chain = snapshot.place(area);
    if chain.is_empty() {
        return tree.folder(folders, "area:0".to_owned(), "No area", Some(root), Some(0));
    }
    let mut place = root;
    for (area, name) in chain {
        place = tree.folder(folders, format!("area:{area}"), name, Some(place), Some(area));
    }
    place
}

/// The folder of the entities of `kind` of the place `place`: one a kind, in the order of `rank`.
fn category(tree: &mut Tree, folders: &mut HashMap<String, u32>, place: u32, kind: Kind) -> u32 {
    let area = tree.nodes[place as usize].area.unwrap_or(0);
    // The rooms and the doodads of a building stay under it; the others by their kind.
    let (label, order) = match kind {
        Kind::Chunk => ("Terrain", 0),
        Kind::Water => ("Water", 1),
        Kind::Group | Kind::Instance => ("Groups", 2),
        Kind::Building | Kind::Room => ("Buildings", 3),
        Kind::Doodad | Kind::BuildingDoodad => ("Doodads", 4),
        Kind::Creature => ("Creatures", 5),
        Kind::GameObject => ("Game objects", 6),
        Kind::Light | Kind::LightZone => ("Lights", 7),
        Kind::Weather => ("Weather", 8),
        _ => ("Others", 9),
    };
    tree.folder(folders, format!("kind:{area}:{order}"), label, Some(place), None)
}

/// The name of `entity` shown: the one the user gave, else its default.
pub fn name_of(entity: &EntityRef, names: &HashMap<EntityId, String>) -> String {
    names.get(&entity.id()).cloned().unwrap_or_else(|| entity.name())
}

/// The label of a kind in the views.
fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Map => "Maps",
        Kind::Area => "Areas",
        Kind::Chunk => "Chunks",
        Kind::Water => "Water",
        Kind::Doodad => "Doodads",
        Kind::Building => "Buildings",
        Kind::Room => "Rooms",
        Kind::BuildingDoodad => "Doodads of the buildings",
        Kind::Creature => "Creatures",
        Kind::GameObject => "Game objects",
        Kind::Live => "Live",
        Kind::Light => "Lights",
        Kind::LightZone => "Zones of light",
        Kind::Weather => "Weathers",
        Kind::Group => "Groups",
        Kind::Prefab => "Prefabs",
        Kind::Instance => "Instances",
        Kind::Waypoint => "Waypoints",
    }
}

/// The entities a filter keeps, without their parents.
fn kept_alone<'a>(snapshot: &'a Snapshot, filter: &Filter, names: &HashMap<EntityId, String>) -> Vec<EntityRef<'a>> {
    snapshot
        .entities()
        .filter(|entity| {
            let in_scope = filter.scope.is_none_or(|(shown, tile, tiles)| {
                let [x, y, _] = entity.origin();
                entity.placed.key.map == shown
                    && chunk_at(x, y).is_some_and(|(at, _)| at.x.abs_diff(tile.x).max(at.y.abs_diff(tile.y)) <= tiles)
            });
            let matches = (filter.kinds.is_empty() || filter.kinds.contains(&entity.id().kind))
                && (filter.text.is_empty()
                    || name_of(entity, names).to_lowercase().contains(&filter.text)
                    || entity.label().to_lowercase().contains(&filter.text));
            (in_scope && matches) || filter.kept.contains(&entity.id())
        })
        .collect()
}

/// The view *Files*: the tiles in scope, each with the chunks, the water, the buildings and the
/// doodads its file stores, an entity listed by several tiles under each, marked.
fn build_files(snapshot: &Snapshot, filter: &Filter, names: &HashMap<EntityId, String>) -> Tree {
    let mut tree = Tree::default();
    let mut folders = HashMap::new();
    let mut kept = kept_alone(snapshot, filter, names);
    kept.retain(|entity| {
        matches!(
            entity.id().kind,
            Kind::Chunk | Kind::Water | Kind::Doodad | Kind::Building
        )
    });
    let mut tiles: Vec<(EntityRef, Vec<uniwow_api::formats::TileId>)> = kept
        .into_iter()
        .map(|entity| {
            let tiles = entity.tiles();
            (entity, tiles)
        })
        .collect();
    tiles.retain(|(_, tiles)| !tiles.is_empty());
    tiles.sort_by_key(|(entity, _)| (entity.id().kind, name_of(entity, names).to_lowercase()));
    for (entity, stored) in &tiles {
        let id = entity.id();
        for tile in stored {
            let at = format!("{}_{}", tile.x, tile.y);
            let root = tree.folder(&mut folders, format!("file:{at}"), &format!("Tile {at}"), None, None);
            let folder = tree.folder(
                &mut folders,
                format!("file:{at}:{}", id.kind.name()),
                kind_label(id.kind),
                Some(root),
                None,
            );
            let others: Vec<String> = stored
                .iter()
                .filter(|other| *other != tile)
                .map(|other| format!("{}_{}", other.x, other.y))
                .collect();
            let label = match others.is_empty() {
                true => name_of(entity, names),
                false => format!("{} (also in {})", name_of(entity, names), others.join(", ")),
            };
            tree.add(Node {
                key: format!("file:{at}:{id}"),
                label,
                entity: Some(id),
                area: None,
                parent: Some(folder),
                children: Vec::new(),
            });
        }
        tree.entities += 1;
    }
    tree.roots
        .sort_by(|a, b| tree.nodes[*a as usize].label.cmp(&tree.nodes[*b as usize].label));
    tree
}

/// The view *By type*: a flat list a kind, each sorted by name.
fn build_by_type(snapshot: &Snapshot, filter: &Filter, names: &HashMap<EntityId, String>) -> Tree {
    let mut tree = Tree::default();
    let mut folders = HashMap::new();
    let mut kept = kept_alone(snapshot, filter, names);
    kept.sort_by_key(|entity| (entity.id().kind, name_of(entity, names).to_lowercase()));
    let mut counts: HashMap<Kind, usize> = HashMap::new();
    for entity in &kept {
        *counts.entry(entity.id().kind).or_default() += 1;
    }
    for entity in &kept {
        let id = entity.id();
        let label = format!("{} ({})", kind_label(id.kind), counts[&id.kind]);
        let folder = tree.folder(&mut folders, format!("type:{}", id.kind.name()), &label, None, None);
        tree.add(Node {
            key: id.to_string(),
            label: name_of(entity, names),
            entity: Some(id),
            area: None,
            parent: Some(folder),
            children: Vec::new(),
        });
        tree.entities += 1;
    }
    tree
}

/// The view *Server*: the creatures, then the game objects, each template with its spawns.
fn build_server(snapshot: &Snapshot, filter: &Filter, names: &HashMap<EntityId, String>) -> Tree {
    let mut tree = Tree::default();
    let mut folders = HashMap::new();
    let mut kept = kept_alone(snapshot, filter, names);
    kept.retain(|entity| matches!(entity.id().kind, Kind::Creature | Kind::GameObject));
    kept.sort_by_key(|entity| {
        (
            entity.id().kind,
            entity.label().to_lowercase(),
            entity.extra(),
            entity.id(),
        )
    });
    for entity in &kept {
        let id = entity.id();
        let (root_key, root_label, table) = match id.kind {
            Kind::GameObject => ("server:gameobjects", "Game objects", "gameobject"),
            _ => ("server:creatures", "Creatures", "creature"),
        };
        let root = tree.folder(&mut folders, root_key.to_owned(), root_label, None, None);
        let template = tree.folder(
            &mut folders,
            format!("server:{table}:{}", entity.extra()),
            &format!("{} ({table}_template {})", entity.label(), entity.extra()),
            Some(root),
            None,
        );
        tree.add(Node {
            key: id.to_string(),
            label: name_of(entity, names),
            entity: Some(id),
            area: None,
            parent: Some(template),
            children: Vec::new(),
        });
        tree.entities += 1;
    }
    tree
}

/// The tree of the entities of `snapshot` that `filter` keeps, in its view: the main one, the map
/// named `map`, those of the groups of `groups` under their group.
pub fn build(
    snapshot: &Snapshot,
    filter: &Filter,
    names: &HashMap<EntityId, String>,
    map: &str,
    groups: &uniwow_api::scene::Groups,
    known: &uniwow_api::scene::Catalogue,
    map_id: Option<u32>,
) -> Tree {
    match filter.view {
        View::Files => return build_files(snapshot, filter, names),
        View::ByType => return build_by_type(snapshot, filter, names),
        View::Server => return build_server(snapshot, filter, names),
        View::Main => {}
    }
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
    // The groups kept with the entities they hold kept.
    for group in &groups.groups {
        let id = EntityId::group(group.id);
        if group.children.iter().any(|child| seen.contains(child))
            && seen.insert(id)
            && let Some(found) = snapshot.get(&id)
        {
            kept.push(found);
        }
    }
    // The groups first, then the places, so that the entities find them; parents before children.
    kept.sort_by_key(|entity| {
        if entity.id().kind == Kind::Group {
            return -1;
        }
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
        let in_group = groups
            .of(&id)
            .and_then(|group| tree.by_entity.get(&EntityId::group(group.id)).copied());
        let parent = match entity
            .parent()
            .and_then(|parent| tree.by_entity.get(&parent).copied())
            .or(in_group)
        {
            Some(parent) => parent,
            None if id.kind == Kind::Live => tree.folder(&mut folders, "live".to_owned(), "Live", None, None),
            None => {
                let place = place_of(&mut tree, &mut folders, snapshot, root, entity.area());
                category(&mut tree, &mut folders, place, id.kind)
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
    // The places of the whole map, those far from the camera too, so that the tree is that of the
    // continent; and the entities its providers know there without having loaded them.
    if let Some(map_id) = map_id {
        let mut areas: Vec<u32> = snapshot
            .areas
            .iter()
            .filter(|(_, area)| area.map == map_id)
            .map(|(id, _)| *id)
            .collect();
        areas.sort_unstable();
        for area in areas {
            place_of(&mut tree, &mut folders, snapshot, root, area);
        }
    }
    for listed in known.by_provider.values() {
        for entry in listed.iter() {
            let matches_known = (filter.kinds.is_empty() || filter.kinds.contains(&entry.id.kind))
                && (filter.text.is_empty()
                    || names
                        .get(&entry.id)
                        .is_some_and(|name| name.to_lowercase().contains(&filter.text))
                    || entry.label.to_lowercase().contains(&filter.text));
            if seen.contains(&entry.id) || tree.by_entity.contains_key(&entry.id) || !matches_known {
                continue;
            }
            let place = place_of(&mut tree, &mut folders, snapshot, root, entry.area);
            let parent = category(&mut tree, &mut folders, place, entry.id.kind);
            let name = names.get(&entry.id).cloned().unwrap_or_else(|| entry.name());
            tree.add(Node {
                key: entry.id.to_string(),
                label: format!("○ {name}"),
                entity: Some(entry.id),
                area: None,
                parent: Some(parent),
                children: Vec::new(),
            });
            tree.known.insert(entry.id, entry.origin);
            tree.entities += 1;
        }
    }
    // Each category tells how many it holds.
    for at in 0..tree.nodes.len() {
        if tree.nodes[at].key.starts_with("kind:") {
            let count = tree.nodes[at].children.len();
            let label = format!("{} ({count})", tree.nodes[at].label);
            tree.nodes[at].label = label;
        }
    }
    // Each node's children: the places by name, then the categories in their order, then the
    // entities by kind and name.
    let order: Vec<(u8, String)> = tree
        .nodes
        .iter()
        .map(|node| {
            let rank = match node.entity {
                // A category after the places, in the order of its kind.
                None if node.key.starts_with("kind:") => {
                    10 + node
                        .key
                        .rsplit(':')
                        .next()
                        .and_then(|rank| rank.parse::<u8>().ok())
                        .unwrap_or(0)
                }
                None => 0,
                Some(id) => 30 + rank(id.kind),
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
