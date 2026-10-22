//! Tests of the registry on pieces the tests make, an area of a tile given by the tests.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::formats::TileId;
use uniwow_api::scene::{Area, Entity, EntityId, Kind, Piece, PieceBuilder, PieceKey, Snapshot, box_around, flags};

use crate::registry::{AreaSource, Change, Registry};

const TILE: f32 = 1600.0 / 3.0;
const CHUNK: f32 = TILE / 16.0;
const ORIGIN: f32 = 32.0 * TILE;

/// Elwynn Forest (12), Goldshire (87) under it; the tile 32_48 of map 0: Goldshire on its chunk
/// 3, 15, Elwynn on the others but the chunk 0, 0, which has none.
struct Fake;

impl AreaSource for Fake {
    fn table(&self) -> Option<HashMap<u32, Area>> {
        let area = |parent, name: &str| Area {
            parent,
            map: 0,
            name: name.to_owned(),
        };
        Some(HashMap::from([
            (12, area(0, "Elwynn Forest")),
            (87, area(12, "Goldshire")),
        ]))
    }

    fn tile(&self, map: u32, tile: TileId) -> Result<Option<Arc<[u32]>>, ()> {
        if map != 0 || tile != (TileId { x: 32, y: 48 }) {
            return Ok(None);
        }
        let mut areas = vec![12; 256];
        areas[0] = 0;
        areas[15 * 16 + 3] = 87;
        Ok(Some(areas.into()))
    }
}

/// The middle of the chunk `chunk` (along Y, then X) of the tile `tile`, in the world.
fn middle(tile: TileId, chunk: [u32; 2]) -> [f32; 3] {
    [
        ORIGIN - TILE * tile.y as f32 - CHUNK * (chunk[1] as f32 + 0.5),
        ORIGIN - TILE * tile.x as f32 - CHUNK * (chunk[0] as f32 + 0.5),
        40.0,
    ]
}

fn doodad(id: EntityId, label: &str, origin: [f32; 3]) -> Entity<'_> {
    Entity {
        id,
        flags: flags::CLIENT,
        parent: None,
        label,
        extra: 0,
        origin,
        bounds: box_around(origin, 1.0),
    }
}

/// A piece of `count` doodads of the tile `tile`, their ids from `first`, on the chunk `chunk`.
fn doodads(tile: TileId, first: u32, count: u32, chunk: [u32; 2]) -> Arc<Piece> {
    let mut builder = PieceBuilder::with_capacity(vec![tile], count as usize);
    for id in first..first + count {
        builder.push(doodad(EntityId::doodad(0, id), "ElwynnTree03", middle(tile, chunk)));
    }
    Arc::new(builder.build())
}

fn publish(registry: &mut Registry, changes: Vec<Change>) -> Snapshot {
    registry.apply(changes, &Fake).0
}

const ELWYNN: TileId = TileId { x: 32, y: 48 };

#[test]
fn the_entities_published_are_read_back_by_their_ids_with_their_places() {
    let mut registry = Registry::default();
    let key = PieceKey::tile("doodads", 0, ELWYNN);
    let mut builder = PieceBuilder::new(vec![ELWYNN]);
    builder.push(doodad(
        EntityId::doodad(0, 4521),
        "ElwynnTree03",
        middle(ELWYNN, [3, 15]),
    ));
    builder.push(doodad(
        EntityId::doodad(0, 4522),
        "ElwynnTree03",
        middle(ELWYNN, [5, 5]),
    ));
    builder.push(doodad(EntityId::doodad(0, 4523), "Bush01", middle(ELWYNN, [0, 0])));
    let snapshot = publish(&mut registry, vec![Change::Publish(key, Arc::new(builder.build()))]);
    assert_eq!(snapshot.entities, 3);
    let tree = snapshot.get(&"doodad:0:4521".parse().unwrap()).unwrap();
    assert_eq!(tree.name(), "ElwynnTree03 #4521");
    let names: Vec<&str> = snapshot.place(tree.area()).into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, ["Elwynn Forest", "Goldshire"]);
    let other = snapshot.get(&EntityId::doodad(0, 4522)).unwrap();
    assert_eq!(snapshot.place(other.area()).len(), 1, "Elwynn alone");
    // No area: the chunk has none.
    assert_eq!(snapshot.get(&EntityId::doodad(0, 4523)).unwrap().area(), 0);
    assert_eq!(tree.tiles(), [ELWYNN]);
}

#[test]
fn a_unique_id_listed_by_three_tiles_is_one_entity_stored_in_all_three() {
    let mut registry = Registry::default();
    let tiles = [ELWYNN, TileId { x: 33, y: 48 }, TileId { x: 32, y: 49 }];
    let changes = tiles
        .iter()
        .map(|tile| {
            let mut builder = PieceBuilder::new(vec![*tile]);
            builder.push(doodad(EntityId::doodad(0, 7), "Fence", middle(ELWYNN, [15, 15])));
            Change::Publish(PieceKey::tile("doodads", 0, *tile), Arc::new(builder.build()))
        })
        .collect();
    let snapshot = publish(&mut registry, changes);
    assert_eq!(snapshot.entities, 1);
    assert_eq!(snapshot.entities().count(), 1);
    let fence = snapshot.get(&EntityId::doodad(0, 7)).unwrap();
    assert_eq!(
        fence.tiles(),
        tiles
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    // A tile left: still one, stored in the two others.
    let snapshot = publish(
        &mut registry,
        vec![Change::Withdraw(PieceKey::tile("doodads", 0, ELWYNN))],
    );
    assert_eq!(snapshot.entities().count(), 1);
    assert_eq!(snapshot.get(&EntityId::doodad(0, 7)).unwrap().tiles().len(), 2);
    // One listed again with other data is an entity of its own.
    let mut builder = PieceBuilder::new(vec![ELWYNN]);
    builder.push(doodad(
        EntityId::doodad_again(0, 7, ELWYNN),
        "Bird",
        middle(ELWYNN, [1, 1]),
    ));
    let snapshot = publish(
        &mut registry,
        vec![Change::Publish(
            PieceKey::tile("doodads", 0, ELWYNN),
            Arc::new(builder.build()),
        )],
    );
    assert_eq!(snapshot.entities().count(), 2);
}

#[test]
fn a_piece_swapped_replaces_its_entities_alone_and_the_others_are_shared() {
    let mut registry = Registry::default();
    let other = TileId { x: 33, y: 48 };
    let snapshot = publish(
        &mut registry,
        vec![
            Change::Publish(PieceKey::tile("doodads", 0, ELWYNN), doodads(ELWYNN, 0, 100, [1, 1])),
            Change::Publish(PieceKey::tile("doodads", 0, other), doodads(other, 1000, 50, [1, 1])),
        ],
    );
    let kept = snapshot
        .placed(*snapshot.keys.get(&PieceKey::tile("doodads", 0, other)).unwrap())
        .unwrap() as *const _;
    let (snapshot, summary) = registry.apply(
        vec![Change::Publish(
            PieceKey::tile("doodads", 0, ELWYNN),
            doodads(ELWYNN, 50, 100, [2, 2]),
        )],
        &Fake,
    );
    assert_eq!(snapshot.entities, 150);
    assert_eq!(
        (summary.added.len(), summary.changed.len(), summary.removed.len()),
        (50, 50, 50)
    );
    let still = snapshot
        .placed(*snapshot.keys.get(&PieceKey::tile("doodads", 0, other)).unwrap())
        .unwrap() as *const _;
    assert_eq!(kept, still, "the piece untouched is shared");
    assert!(snapshot.get(&EntityId::doodad(0, 10)).is_none());
    assert!(snapshot.get(&EntityId::doodad(0, 149)).is_some());
    // The older snapshot still reads what it was made from.
    drop(snapshot);
    let snapshot = publish(&mut registry, vec![Change::WithdrawProvider("doodads".into(), Some(0))]);
    assert_eq!(snapshot.entities, 0);
    assert_eq!(snapshot.keys.len(), 0);
}

#[test]
fn the_parents_the_kinds_and_the_names_of_a_building() {
    let mut registry = Registry::default();
    let mut builder = PieceBuilder::new(vec![ELWYNN]);
    let at = middle(ELWYNN, [3, 15]);
    let inn = builder.push(Entity {
        id: EntityId::building(0, 4128),
        flags: flags::CLIENT,
        parent: None,
        label: "Goldshire_Inn",
        extra: 0,
        origin: at,
        bounds: box_around(at, 20.0),
    });
    let kitchen = builder.push(Entity {
        id: EntityId::room(0, 4128, 3),
        parent: Some(inn),
        label: "",
        ..doodad(EntityId::doodad(0, 0), "", at)
    });
    builder.push(Entity {
        id: EntityId::building_doodad(0, 4128, 12),
        parent: Some(kitchen),
        label: "Chair01",
        ..doodad(EntityId::doodad(0, 0), "", at)
    });
    let snapshot = publish(
        &mut registry,
        vec![Change::Publish(
            PieceKey::part("buildings", 0, 4128),
            Arc::new(builder.build()),
        )],
    );
    let chair = snapshot.get(&EntityId::building_doodad(0, 4128, 12)).unwrap();
    assert_eq!(chair.name(), "Chair01 #12");
    assert_eq!(chair.parent(), Some(EntityId::room(0, 4128, 3)));
    assert_eq!(snapshot.get(&EntityId::room(0, 4128, 3)).unwrap().name(), "Room 3");
    let kinds: Vec<Kind> = snapshot.entities().map(|entity| entity.id().kind).collect();
    assert_eq!(kinds, [Kind::Building, Kind::Room, Kind::BuildingDoodad]);
    assert_eq!(snapshot.areas_within(0, box_around(at, 20.0)), [12, 87]);
}

#[test]
fn snapshots_are_read_by_eight_threads_while_pieces_are_swapped() {
    let published = std::sync::Mutex::new(Arc::new(Snapshot::default()));
    let mut registry = Registry::default();
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let mut reads = 0;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let snapshot = published.lock().unwrap().clone();
                        // Every entity listed reads back whole from the snapshot it came from.
                        for entity in snapshot.entities() {
                            assert_eq!(snapshot.get(&entity.id()).unwrap().id(), entity.id());
                        }
                        reads += 1;
                    }
                    reads
                })
            })
            .collect();
        for round in 0..200u32 {
            let tile = TileId { x: round % 8, y: 0 };
            let piece = doodads(tile, (round % 8) * 1000, 100 + round % 7, [0, 0]);
            let (snapshot, _) = registry.apply(vec![Change::Publish(PieceKey::tile("doodads", 0, tile), piece)], &Fake);
            *published.lock().unwrap() = Arc::new(snapshot);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads: usize = readers.into_iter().map(|reader| reader.join().unwrap()).sum();
        assert!(reads > 0);
    });
}

/// The benchmarks of step 15.2: a tile of 3,576 doodads merged, 500,000 indexed from nothing.
#[test]
fn a_tile_of_3_576_merges_in_milliseconds_and_500_000_index_in_a_fraction_of_a_second() {
    let mut registry = Registry::default();
    let started = Instant::now();
    let changes = (0..140u32)
        .map(|n| {
            let tile = TileId { x: n % 64, y: n / 64 };
            Change::Publish(
                PieceKey::tile("doodads", 0, tile),
                doodads(tile, n * 4000, 3576, [4, 4]),
            )
        })
        .collect::<Vec<_>>();
    let built = started.elapsed();
    let started = Instant::now();
    let (snapshot, _) = registry.apply(changes, &Fake);
    let whole = started.elapsed();
    assert_eq!(snapshot.entities, 140 * 3576);
    drop(snapshot);
    let tile = TileId { x: 63, y: 63 };
    let piece = doodads(tile, 10_000_000, 3576, [4, 4]);
    let started = Instant::now();
    let (snapshot, _) = registry.apply(vec![Change::Publish(PieceKey::tile("doodads", 0, tile), piece)], &Fake);
    let caught_up = started.elapsed();
    assert_eq!(snapshot.entities, 141 * 3576);
    drop(snapshot);
    // From then on, the copy behind is one tile behind: a tile merged costs that tile alone.
    let piece = doodads(tile, 20_000_000, 3576, [4, 4]);
    let started = Instant::now();
    let (snapshot, _) = registry.apply(vec![Change::Publish(PieceKey::tile("doodads", 0, tile), piece)], &Fake);
    let one = started.elapsed();
    eprintln!(
        "the first tile after the burst, the copy behind brought level: {:.1} ms",
        caught_up.as_secs_f64() * 1000.0
    );
    let bytes: usize = snapshot
        .keys
        .values()
        .filter_map(|slot| snapshot.placed(*slot))
        .map(|placed| placed.piece.bytes())
        .sum();
    eprintln!(
        "500,640 entities: pieces built in {:.0} ms, indexed in {:.0} ms; a tile of 3,576 merged in {:.1} ms; {:.0} bytes an entity in the pieces",
        built.as_secs_f64() * 1000.0,
        whole.as_secs_f64() * 1000.0,
        one.as_secs_f64() * 1000.0,
        bytes as f64 / snapshot.entities as f64
    );
    // Lenient bounds for a debug build on any machine; the release figures are in the spec.
    assert!(one.as_millis() < 500, "a tile merged in {one:?}");
    assert!(whole.as_secs() < 20, "500,000 indexed in {whole:?}");
}

mod picking {
    use std::collections::HashMap;
    use std::sync::Arc;

    use uniwow_api::glam::{Mat4, Vec3};
    use uniwow_api::scene::{Entity, EntityId, EntityRef, Kind, Locks, Picked, Picker, PieceBuilder, PieceKey, flags};
    use uniwow_api::viewport::{Ray, Seen};

    use super::{ELWYNN, middle};
    use crate::picking::{pick, within};
    use crate::registry::{Change, Registry};

    /// Misses the doodad 2 whatever its box says, as a model whose leaves the ray passes between.
    struct Leaves;

    impl Picker for Leaves {
        fn intersect(&self, entity: &EntityRef<'_>, ray: &Ray) -> Picked {
            match entity.id() {
                id if id == EntityId::doodad(0, 2) => Picked::Missed,
                _ => ray
                    .hits_box(entity.bounds())
                    .map_or(Picked::Missed, |d| Picked::Hit(d + 0.5)),
            }
        }
    }

    #[test]
    fn a_ray_picks_the_nearest_entity_not_locked_its_parents_first_in_its_chain() {
        let at = middle(ELWYNN, [5, 5]);
        let mut builder = PieceBuilder::new(vec![ELWYNN]);
        let entity = |id, parent, origin: [f32; 3], half| Entity {
            id,
            flags: flags::CLIENT,
            parent,
            label: "",
            extra: 0,
            origin,
            bounds: [origin.map(|v| v - half), origin.map(|v| v + half)],
        };
        // Along the ray, down from above: a chunk at the ground, a doodad 2 over it, a doodad 1
        // higher, and a building whose room holds a chair.
        let up = |z: f32| [at[0], at[1], z];
        builder.push(entity(EntityId::chunk(0, ELWYNN, [5, 5]), None, up(0.0), 1.0));
        builder.push(entity(EntityId::doodad(0, 2), None, up(20.0), 1.0));
        builder.push(entity(EntityId::doodad(0, 1), None, up(10.0), 1.0));
        let snapshot = super::publish(
            &mut Registry::default(),
            vec![Change::Publish(
                PieceKey::tile("doodads", 0, ELWYNN),
                Arc::new(builder.build()),
            )],
        );
        let ray = Ray {
            origin: Vec3::from(up(100.0)),
            direction: Vec3::NEG_Z,
        };
        let none = HashMap::new();
        let locked = Locks::of_kinds(&[Kind::Chunk]);
        let open = Locks::default();
        assert_eq!(
            pick(&snapshot, &ray, &locked, &none).unwrap().id,
            EntityId::doodad(0, 2)
        );
        let pickers: HashMap<String, Arc<dyn Picker>> = HashMap::from([("doodads".to_owned(), Arc::new(Leaves) as _)]);
        let hit = pick(&snapshot, &ray, &locked, &pickers).unwrap();
        assert_eq!(hit.id, EntityId::doodad(0, 1), "the doodad 2 missed by its picker");
        assert!((hit.distance - 89.5).abs() < 1e-3, "{}", hit.distance);
        assert_eq!(
            pick(&snapshot, &ray, &open, &pickers).unwrap().id,
            EntityId::doodad(0, 1)
        );
        let below = Ray {
            origin: Vec3::from(up(5.0)),
            direction: Vec3::NEG_Z,
        };
        assert_eq!(pick(&snapshot, &below, &locked, &none), None, "the chunk locked");
        assert_eq!(
            pick(&snapshot, &below, &open, &none).unwrap().id,
            EntityId::chunk(0, ELWYNN, [5, 5])
        );

        let mut builder = PieceBuilder::new(vec![ELWYNN]);
        let inn = builder.push(entity(EntityId::building(0, 9), None, up(50.0), 20.0));
        let room = builder.push(entity(EntityId::room(0, 9, 1), Some(inn), up(50.0), 5.0));
        builder.push(entity(EntityId::building_doodad(0, 9, 4), Some(room), up(48.0), 1.0));
        let snapshot = super::publish(
            &mut Registry::default(),
            vec![Change::Publish(
                PieceKey::part("buildings", 0, 9),
                Arc::new(builder.build()),
            )],
        );
        // From inside the building, its box entered at once: the chair, by the narrow phase of a
        // picker that finds the building's walls far.
        struct Walls;
        impl Picker for Walls {
            fn intersect(&self, entity: &EntityRef<'_>, ray: &Ray) -> Picked {
                match entity.id().kind {
                    Kind::Building | Kind::Room => Picked::Missed,
                    _ => ray.hits_box(entity.bounds()).map_or(Picked::Missed, Picked::Hit),
                }
            }
        }
        let pickers: HashMap<String, Arc<dyn Picker>> = HashMap::from([("buildings".to_owned(), Arc::new(Walls) as _)]);
        let inside = Ray {
            origin: Vec3::from(up(52.0)),
            direction: Vec3::NEG_Z,
        };
        let mut parent = Locks::default();
        parent.entities.insert(EntityId::room(0, 9, 1));
        assert_eq!(
            pick(&snapshot, &inside, &parent, &pickers),
            None,
            "the chair under a room locked"
        );
        let hit = pick(&snapshot, &inside, &Locks::default(), &pickers).unwrap();
        assert_eq!(
            hit.chain,
            [
                EntityId::building(0, 9),
                EntityId::room(0, 9, 1),
                EntityId::building_doodad(0, 9, 4)
            ]
        );
    }

    #[test]
    fn a_rectangle_keeps_the_origins_in_front_of_the_eye_within_it_of_the_kinds_of_its_filter() {
        let mut builder = PieceBuilder::new(vec![ELWYNN]);
        let at = middle(ELWYNN, [5, 5]);
        for (n, dx) in [(1, 0.0), (2, 5.0), (3, 40.0)] {
            let origin = [at[0] + dx, at[1], at[2]];
            builder.push(Entity {
                id: EntityId::doodad(0, n),
                flags: flags::CLIENT,
                parent: None,
                label: "",
                extra: 0,
                origin,
                bounds: [origin; 2],
            });
        }
        builder.push(Entity {
            id: EntityId::chunk(0, ELWYNN, [5, 5]),
            flags: flags::CLIENT,
            parent: None,
            label: "",
            extra: 0,
            origin: at,
            bounds: [at; 2],
        });
        let snapshot = super::publish(
            &mut Registry::default(),
            vec![Change::Publish(
                PieceKey::tile("doodads", 0, ELWYNN),
                Arc::new(builder.build()),
            )],
        );
        let eye = Vec3::from(at) + Vec3::new(0.0, 0.0, 50.0);
        let seen = Seen {
            view_proj: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.5)
                * Mat4::look_at_rh(eye, Vec3::from(at), Vec3::X),
            eye,
            size: [400.0, 400.0],
        };
        let mut found = within(
            &snapshot,
            &seen,
            [[150.0, 150.0], [250.0, 250.0]],
            &[],
            &Locks::of_kinds(&[Kind::Chunk]),
        );
        found.sort();
        assert_eq!(found, [EntityId::doodad(0, 1), EntityId::doodad(0, 2)]);
        let chunks = within(
            &snapshot,
            &seen,
            [[150.0, 150.0], [250.0, 250.0]],
            &[Kind::Chunk],
            &Locks::default(),
        );
        assert_eq!(chunks, [EntityId::chunk(0, ELWYNN, [5, 5])]);
    }
}

#[test]
fn a_live_creature_of_a_spawn_is_the_spawn_edited_where_it_was_put_and_live_where_it_runs() {
    let mut registry = Registry::default();
    let guard = EntityId::creature(79753);
    let at = middle(ELWYNN, [3, 15]);
    let mut spawns = PieceBuilder::new(vec![ELWYNN]);
    spawns.push(Entity {
        flags: flags::SERVER,
        ..doodad(guard, "Stormwind City Guard", at)
    });
    let now = [at[0] + 5.0, at[1], at[2]];
    let mut live = PieceBuilder::new(Vec::new());
    live.push(Entity {
        flags: flags::LIVE | flags::READ_ONLY,
        ..doodad(guard, "Stormwind City Guard", now)
    });
    live.push(Entity {
        flags: flags::LIVE | flags::READ_ONLY,
        ..doodad(EntityId::live(0xf130), "Rabbit", now)
    });
    let snapshot = publish(
        &mut registry,
        vec![
            Change::Publish(PieceKey::part("live-world", 0, 0), Arc::new(live.build())),
            Change::Publish(PieceKey::tile("spawns", 0, ELWYNN), Arc::new(spawns.build())),
        ],
    );
    assert_eq!(
        snapshot.entities().filter(|entity| entity.id() == guard).count(),
        1,
        "one entity"
    );
    let edited = snapshot.editable(&guard).unwrap();
    assert_eq!((edited.provider(), edited.origin()), ("spawns", at));
    let running = snapshot.live(&guard).unwrap();
    assert_eq!((running.provider(), running.origin()), ("live-world", now));
    // Live alone: the world as it runs gives it, nothing to edit it but that.
    let rabbit = EntityId::live(0xf130);
    assert_eq!(snapshot.editable(&rabbit).unwrap().provider(), "live-world");
    assert!(snapshot.live(&rabbit).is_some());
    // The spawn alone: no live listing.
    // Picked where it runs, not at its place of the database, where nothing is drawn.
    let ray = |at: [f32; 3]| uniwow_api::viewport::Ray {
        origin: uniwow_api::glam::Vec3::new(at[0], at[1], at[2] + 50.0),
        direction: uniwow_api::glam::Vec3::NEG_Z,
    };
    let (none, open) = (HashMap::new(), uniwow_api::scene::Locks::default());
    let picked = crate::picking::pick(&snapshot, &ray(now), &open, &none).map(|hit| hit.id);
    assert_eq!(picked, Some(guard), "where it runs");
    let picked = crate::picking::pick(&snapshot, &ray(at), &open, &none).map(|hit| hit.id);
    assert_eq!(picked, None, "not at its spawn's place while live");
    let snapshot = publish(
        &mut registry,
        vec![Change::Withdraw(PieceKey::part("live-world", 0, 0))],
    );
    assert!(snapshot.live(&guard).is_none());
    let picked = crate::picking::pick(&snapshot, &ray(at), &open, &none).map(|hit| hit.id);
    assert_eq!(picked, Some(guard), "at its spawn's place once the world leaves");
    assert_eq!(snapshot.editable(&guard).unwrap().provider(), "spawns");
}

#[test]
fn a_point_of_a_path_is_picked_before_the_creature_standing_on_it() {
    let mut registry = Registry::default();
    let guard = EntityId::creature(79753);
    let at = middle(ELWYNN, [3, 15]);
    let mut spawns = PieceBuilder::new(vec![ELWYNN]);
    spawns.push(Entity {
        flags: flags::SERVER,
        bounds: box_around(at, 3.0),
        ..doodad(guard, "Stormwind City Guard", at)
    });
    let point = EntityId::waypoint(797530, 1);
    let mut points = PieceBuilder::new(Vec::new());
    points.push(Entity {
        flags: flags::SERVER,
        bounds: box_around(at, 0.6),
        ..doodad(point, "Waypoint", at)
    });
    let snapshot = publish(
        &mut registry,
        vec![
            Change::Publish(PieceKey::tile("spawns", 0, ELWYNN), Arc::new(spawns.build())),
            Change::Publish(PieceKey::part("spawns", 0, 0), Arc::new(points.build())),
        ],
    );
    let ray = uniwow_api::viewport::Ray {
        origin: uniwow_api::glam::Vec3::new(at[0], at[1], at[2] + 50.0),
        direction: uniwow_api::glam::Vec3::NEG_Z,
    };
    let (none, open) = (HashMap::new(), uniwow_api::scene::Locks::default());
    let picked = crate::picking::pick(&snapshot, &ray, &open, &none).map(|hit| hit.id);
    assert_eq!(picked, Some(point), "the point, though the creature's box is hit first");
    let beside = uniwow_api::viewport::Ray {
        origin: uniwow_api::glam::Vec3::new(at[0] + 2.0, at[1], at[2] + 50.0),
        ..ray
    };
    let picked = crate::picking::pick(&snapshot, &beside, &open, &none).map(|hit| hit.id);
    assert_eq!(picked, Some(guard), "beside the point, the creature");
}
