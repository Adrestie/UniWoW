//! Tests of the tree of the Hierarchy on snapshots the tests make.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uniwow_api::formats::TileId;
use uniwow_api::scene::{
    Area, Entity, EntityId, Index, Kind, Listing, PieceBuilder, PieceKey, Placed, Snapshot, box_around, flags,
    piece_bounds,
};

use crate::tree::{Filter, build};

const TILE: f32 = 1600.0 / 3.0;
const ORIGIN: f32 = 32.0 * TILE;
const ELWYNN: TileId = TileId { x: 32, y: 48 };

/// The middle of the tile `tile` in the world.
fn middle(tile: TileId) -> [f32; 3] {
    [
        ORIGIN - TILE * (tile.y as f32 + 0.5),
        ORIGIN - TILE * (tile.x as f32 + 0.5),
        0.0,
    ]
}

/// A snapshot of the entities `entities` of the map 0, each with its area, in one piece.
fn snapshot(entities: &[(Entity, u32)]) -> Snapshot {
    let mut builder = PieceBuilder::new(vec![ELWYNN]);
    for (entity, _) in entities {
        builder.push(entity.clone());
    }
    let piece = Arc::new(builder.build());
    let key = PieceKey::tile("test", 0, ELWYNN);
    let index: Index = piece
        .ids
        .iter()
        .enumerate()
        .map(|(row, id)| {
            (
                *id,
                vec![Listing {
                    slot: 0,
                    row: row as u32,
                }],
            )
        })
        .collect();
    let placed = Arc::new(Placed {
        key: key.clone(),
        generation: 1,
        bounds: piece_bounds(&piece),
        areas: entities.iter().map(|(_, area)| *area).collect(),
        piece,
    });
    let area = |parent, name: &str| Area {
        parent,
        map: 0,
        name: name.to_owned(),
    };
    Snapshot {
        generation: 1,
        entities: index.len(),
        slots: Arc::new(vec![Some(placed)]),
        keys: Arc::new([(key, 0)].into_iter().collect()),
        index: Arc::new(index),
        areas: Arc::new(HashMap::from([
            (12, area(0, "Elwynn Forest")),
            (87, area(12, "Goldshire")),
        ])),
        ..Snapshot::default()
    }
}

fn entity(id: EntityId, parent: Option<u32>, label: &'static str, origin: [f32; 3]) -> Entity<'static> {
    Entity {
        id,
        flags: flags::CLIENT,
        parent,
        label,
        extra: 0,
        origin,
        bounds: box_around(origin, 1.0),
    }
}

fn goldshire() -> Snapshot {
    let at = middle(ELWYNN);
    let far = middle(TileId { x: 40, y: 48 });
    snapshot(&[
        (entity(EntityId::building(0, 9), None, "Goldshire_Inn", at), 87),
        (entity(EntityId::room(0, 9, 1), Some(0), "The Kitchen", at), 87),
        (entity(EntityId::building_doodad(0, 9, 4), Some(1), "Chair01", at), 87),
        (entity(EntityId::doodad(0, 4521), None, "ElwynnTree03", at), 12),
        (entity(EntityId::chunk(0, ELWYNN, [3, 15]), None, "", at), 87),
        (entity(EntityId::doodad(0, 7), None, "Rock", at), 0),
        (entity(EntityId::live(0xf1), None, "Marshal Dughan", at), 87),
        (entity(EntityId::doodad(0, 99), None, "FarTree", far), 12),
    ])
}

/// The labels of the rows unfolded wholly, indented by their depth.
fn shown(tree: &crate::tree::Tree) -> Vec<String> {
    let all: HashSet<String> = tree.nodes.iter().map(|node| node.key.clone()).collect();
    tree.rows(&all)
        .into_iter()
        .map(|(node, depth)| format!("{}{}", "  ".repeat(depth as usize), tree.nodes[node as usize].label))
        .collect()
}

#[test]
fn the_main_view_lists_the_map_its_zones_and_subzones_and_what_lies_in_them_and_live_beside() {
    let snapshot = goldshire();
    let tree = build(
        &snapshot,
        &Filter::default(),
        &HashMap::new(),
        "Eastern Kingdoms",
        &Default::default(),
        &Default::default(),
        None,
    );
    assert_eq!(
        shown(&tree),
        [
            "Eastern Kingdoms",
            "  Elwynn Forest",
            "    Goldshire",
            "      Terrain (1)",
            "        Chunk 3, 15",
            "      Buildings (1)",
            "        Goldshire_Inn #9",
            "          The Kitchen",
            "            Chair01 #4",
            "    Doodads (2)",
            "      ElwynnTree03 #4521",
            "      FarTree #99",
            "  No area",
            "    Doodads (1)",
            "      Rock #7",
            "Live",
            "  Marshal Dughan",
        ]
    );
    assert_eq!(tree.entities, 8);
}

#[test]
fn a_search_keeps_the_matches_with_their_parents_and_the_scope_leaves_the_far_ones_out() {
    let snapshot = goldshire();
    let filter = Filter {
        text: "chair".to_owned(),
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &filter,
        &HashMap::new(),
        "Eastern Kingdoms",
        &Default::default(),
        &Default::default(),
        None,
    );
    assert_eq!(
        shown(&tree),
        [
            "Eastern Kingdoms",
            "  Elwynn Forest",
            "    Goldshire",
            "      Buildings (1)",
            "        Goldshire_Inn #9",
            "          The Kitchen",
            "            Chair01 #4",
        ]
    );
    let scoped = Filter {
        scope: Some((0, ELWYNN, 2)),
        kinds: vec![Kind::Doodad],
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &scoped,
        &HashMap::new(),
        "",
        &Default::default(),
        &Default::default(),
        None,
    );
    let labels = shown(&tree);
    assert!(labels.contains(&"      ElwynnTree03 #4521".to_owned()), "{labels:?}");
    assert!(!labels.iter().any(|label| label.contains("FarTree")), "8 tiles away");
    // A name given is shown and searched.
    let names = HashMap::from([(EntityId::doodad(0, 4521), "The old oak".to_owned())]);
    let named = Filter {
        text: "old oak".to_owned(),
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &named,
        &names,
        "",
        &Default::default(),
        &Default::default(),
        None,
    );
    assert!(shown(&tree).contains(&"      The old oak".to_owned()));
}

#[test]
fn the_entities_of_a_group_are_listed_under_it_not_in_their_place() {
    use uniwow_api::scene::{Group, Groups};
    let at = middle(ELWYNN);
    let snapshot = snapshot(&[
        (entity(EntityId::group(3), None, "Camp", at), 87),
        (entity(EntityId::doodad(0, 1), None, "Tent", at), 87),
        (entity(EntityId::doodad(0, 2), None, "Fire", at), 87),
        (entity(EntityId::doodad(0, 4521), None, "ElwynnTree03", at), 87),
    ]);
    let groups = Groups {
        generation: 1,
        groups: vec![Group {
            id: 3,
            name: "Camp".to_owned(),
            map: 0,
            children: vec![EntityId::doodad(0, 1), EntityId::doodad(0, 2)],
            prefab: None,
        }],
        prefabs: Vec::new(),
    };
    let tree = build(
        &snapshot,
        &Filter::default(),
        &HashMap::new(),
        "Map",
        &groups,
        &Default::default(),
        None,
    );
    assert_eq!(
        shown(&tree),
        [
            "Map",
            "  Elwynn Forest",
            "    Goldshire",
            "      Groups (1)",
            "        Camp",
            "          Fire #2",
            "          Tent #1",
            "      Doodads (1)",
            "        ElwynnTree03 #4521",
        ]
    );
}

#[test]
fn the_files_list_each_tile_with_what_it_stores_and_by_type_a_list_a_kind() {
    use crate::tree::View;
    let at = middle(ELWYNN);
    // A doodad listed by two tiles: one piece of each.
    let mut snapshot = snapshot(&[
        (entity(EntityId::doodad(0, 7), None, "Fence", at), 87),
        (entity(EntityId::building(0, 9), None, "Goldshire_Inn", at), 87),
        (entity(EntityId::chunk(0, ELWYNN, [1, 1]), None, "", at), 87),
    ]);
    let other = TileId { x: 33, y: 48 };
    let mut builder = PieceBuilder::new(vec![other]);
    builder.push(entity(EntityId::doodad(0, 7), None, "Fence", at));
    let piece = Arc::new(builder.build());
    let key = PieceKey::tile("test", 0, other);
    let mut slots = (*snapshot.slots).clone();
    slots.push(Some(Arc::new(Placed {
        key: key.clone(),
        generation: 1,
        bounds: piece_bounds(&piece),
        areas: Box::new([87]),
        piece,
    })));
    let mut keys = (*snapshot.keys).clone();
    keys.insert(key, 1);
    let mut index = (*snapshot.index).clone();
    index
        .get_mut(&EntityId::doodad(0, 7))
        .unwrap()
        .push(Listing { slot: 1, row: 0 });
    snapshot.slots = Arc::new(slots);
    snapshot.keys = Arc::new(keys);
    snapshot.index = Arc::new(index);
    let files = Filter {
        view: View::Files,
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &files,
        &HashMap::new(),
        "",
        &Default::default(),
        &Default::default(),
        None,
    );
    let labels = shown(&tree);
    assert!(labels.contains(&"Tile 32_48".to_owned()));
    assert!(
        labels.contains(&"    Fence #7 (also in 33_48)".to_owned()),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"    Fence #7 (also in 32_48)".to_owned()),
        "{labels:?}"
    );
    assert!(labels.contains(&"    Chunk 1, 1".to_owned()));
    let by_type = Filter {
        view: View::ByType,
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &by_type,
        &HashMap::new(),
        "",
        &Default::default(),
        &Default::default(),
        None,
    );
    assert_eq!(
        shown(&tree),
        [
            "Chunks (1)",
            "  Chunk 1, 1",
            "Doodads (1)",
            "  Fence #7",
            "Buildings (1)",
            "  Goldshire_Inn #9"
        ]
    );
}

#[test]
fn the_server_lists_each_template_with_its_spawns() {
    use crate::tree::View;
    let at = middle(ELWYNN);
    let guard = |guid: u64| Entity {
        extra: 68,
        ..entity(EntityId::creature(guid), None, "Stormwind City Guard", at)
    };
    let snapshot = snapshot(&[
        (guard(79753), 87),
        (guard(79754), 87),
        (
            Entity {
                extra: 1731,
                ..entity(EntityId::game_object(5714), None, "Copper Vein", at)
            },
            87,
        ),
        (entity(EntityId::doodad(0, 1), None, "Tree", at), 87),
    ]);
    let server = Filter {
        view: View::Server,
        ..Filter::default()
    };
    let tree = build(
        &snapshot,
        &server,
        &HashMap::new(),
        "",
        &Default::default(),
        &Default::default(),
        None,
    );
    assert_eq!(
        shown(&tree),
        [
            "Creatures",
            "  Stormwind City Guard (creature_template 68)",
            "    Stormwind City Guard #79753",
            "    Stormwind City Guard #79754",
            "Game objects",
            "  Copper Vein (gameobject_template 1731)",
            "    Copper Vein #5714",
        ]
    );
}

#[test]
fn the_rows_follow_the_folds_and_the_path_of_a_node_unfolds_to_it() {
    let snapshot = goldshire();
    let tree = build(
        &snapshot,
        &Filter::default(),
        &HashMap::new(),
        "Map",
        &Default::default(),
        &Default::default(),
        None,
    );
    let mut unfolded = HashSet::from(["map".to_owned()]);
    let rows = tree.rows(&unfolded);
    // The map, its zone and No area; Live folded.
    assert_eq!(rows.len(), 4);
    let chair = tree.by_entity[&EntityId::building_doodad(0, 9, 4)];
    let path = tree.path(chair);
    for node in &path[..path.len() - 1] {
        unfolded.insert(tree.nodes[*node as usize].key.clone());
    }
    let rows = tree.rows(&unfolded);
    assert!(rows.iter().any(|(node, depth)| *node == chair && *depth == 6));
}

/// The benchmark of step 15.4: a tree of 500,000 entities made from nothing.
#[test]
fn a_tree_of_500_000_entities_is_made_in_a_fraction_of_a_second() {
    let at = middle(ELWYNN);
    let labels = ["ElwynnTree03", "Bush01", "Rock02", "Fence04"];
    let entities: Vec<(Entity, u32)> = (0..500_000u32)
        .map(|n| {
            (
                entity(EntityId::doodad(0, n), None, labels[n as usize % 4], at),
                if n % 2 == 0 { 87 } else { 12 },
            )
        })
        .collect();
    let snapshot = snapshot(&entities);
    let started = std::time::Instant::now();
    let tree = build(
        &snapshot,
        &Filter::default(),
        &HashMap::new(),
        "Map",
        &Default::default(),
        &Default::default(),
        None,
    );
    let took = started.elapsed();
    assert_eq!(tree.entities, 500_000);
    eprintln!(
        "a tree of 500,000 entities made in {:.0} ms",
        took.as_secs_f64() * 1000.0
    );
    assert!(took.as_secs() < 10, "{took:?}");
}

#[test]
fn the_whole_map_is_listed_its_zones_far_away_and_the_spawns_known_but_not_loaded() {
    use uniwow_api::scene::{Catalogue, Known};
    let mut snapshot = goldshire();
    let area = |parent, map, name: &str| Area {
        parent,
        map,
        name: name.to_owned(),
    };
    snapshot.areas = Arc::new(HashMap::from([
        (12, area(0, 0, "Elwynn Forest")),
        (87, area(12, 0, "Goldshire")),
        (40, area(0, 0, "Westfall")),
        (108, area(40, 0, "Sentinel Hill")),
        (3537, area(0, 571, "Borean Tundra")),
    ]));
    let far_guard = Known {
        id: EntityId::creature(79666),
        label: "Stormwind City Guard".to_owned(),
        extra: 68,
        area: 108,
        origin: [-10_500.0, 1_000.0, 40.0],
    };
    // A spawn the scene holds already is listed once, by the scene.
    let held = Known {
        id: EntityId::doodad(0, 4521),
        ..far_guard.clone()
    };
    let known = Catalogue {
        generation: 1,
        by_provider: [("spawns".to_owned(), Arc::new(vec![far_guard, held]))]
            .into_iter()
            .collect(),
    };
    let tree = build(
        &snapshot,
        &Filter::default(),
        &HashMap::new(),
        "Eastern Kingdoms",
        &Default::default(),
        &known,
        Some(0),
    );
    let labels = shown(&tree);
    assert!(
        labels.contains(&"  Westfall".to_owned()),
        "a zone far from the camera: {labels:?}"
    );
    assert!(labels.contains(&"    Sentinel Hill".to_owned()));
    assert!(labels.contains(&"      Creatures (1)".to_owned()));
    assert!(labels.contains(&"        ○ Stormwind City Guard #79666".to_owned()));
    assert!(
        !labels.iter().any(|label| label.contains("Borean")),
        "a zone of another map"
    );
    assert_eq!(labels.iter().filter(|label| label.contains("ElwynnTree03")).count(), 1);
    assert_eq!(
        tree.known.get(&EntityId::creature(79666)),
        Some(&[-10_500.0, 1_000.0, 40.0])
    );
    // The places first, then the categories in their order.
    let elwynn = labels.iter().position(|label| label == "  Elwynn Forest").unwrap();
    assert_eq!(labels[elwynn + 1], "    Goldshire");
}
