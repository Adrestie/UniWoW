//! Tests of the doodads: the tiles wanted around the camera, the transform of a doodad's file, and
//! the owners of the tiles kept against a fake service `models`.

use std::collections::{BTreeMap, HashSet};
use std::f32::consts::FRAC_PI_2;
use std::sync::Mutex;

use uniwow_api::formats::{Doodad, FileRef, Placements};
use uniwow_api::glam::{Mat3, Mat4, Vec3};
use uniwow_api::models::{Extent, Geosets, Instance, Look, LookId, LookState, Models, Motion};

use crate::placing::{self, Placing, TILE, TileId};

const ORIGIN: f32 = 32.0 * TILE;

/// The sets of instances of each owner, as the service keeps them.
#[derive(Default)]
struct Fake {
    looks: Mutex<Vec<Look>>,
    owners: Mutex<BTreeMap<String, BTreeMap<u64, Instance>>>,
}

impl Fake {
    /// The ids of each owner's instances.
    fn sets(&self) -> BTreeMap<String, Vec<u64>> {
        self.owners
            .lock()
            .unwrap()
            .iter()
            .map(|(owner, set)| (owner.clone(), set.keys().copied().collect()))
            .collect()
    }
}

impl Models for Fake {
    fn look(&self, look: &Look) -> LookId {
        let mut looks = self.looks.lock().unwrap();
        if let Some(at) = looks.iter().position(|known| known == look) {
            return LookId(at as u32);
        }
        looks.push(look.clone());
        LookId(looks.len() as u32 - 1)
    }
    fn display(&self, _display: u32) -> Result<(Look, f32), String> {
        Err("not asked".to_owned())
    }
    fn object(&self, _display: u32) -> Result<Option<Look>, String> {
        Err("not asked".to_owned())
    }
    fn place(&self, owner: &str, instances: &[Instance]) {
        let set = instances.iter().map(|instance| (instance.id, *instance)).collect();
        self.owners.lock().unwrap().insert(owner.to_owned(), set);
    }
    fn change(&self, owner: &str, changed: &[Instance], removed: &[u64]) {
        let mut owners = self.owners.lock().unwrap();
        let set = owners.entry(owner.to_owned()).or_default();
        for instance in changed {
            set.insert(instance.id, *instance);
        }
        for id in removed {
            set.remove(id);
        }
    }
    fn clear(&self, owner: &str) {
        self.owners.lock().unwrap().remove(owner);
    }
    fn state(&self, _look: LookId) -> LookState {
        LookState::Waiting
    }
    fn extent(&self, _look: LookId) -> Option<Extent> {
        None
    }
}

fn doodad(unique_id: u32, file: &str, position: [f32; 3], rotation: [f32; 3], scale: f32) -> Doodad {
    Doodad {
        file: FileRef::Path(file.to_owned()),
        unique_id,
        position,
        rotation,
        scale,
        flags: 0,
    }
}

fn instance(id: u64) -> Instance {
    Instance {
        id,
        look: LookId(0),
        transform: Mat4::from_translation(Vec3::splat(id as f32)),
        alpha: 1.0,
        motion: Motion::Standing,
    }
}

fn tiles(ids: &[TileId]) -> HashSet<TileId> {
    ids.iter().copied().collect()
}

const A: TileId = TileId { x: 1, y: 1 };
const B: TileId = TileId { x: 2, y: 1 };
const C: TileId = TileId { x: 1, y: 2 };

fn owner(tile: TileId) -> String {
    placing::owner("Map", tile)
}

#[test]
fn the_tiles_wanted_are_those_around_the_camera_the_nearest_first() {
    let all = vec![true; 4096];
    // The middle of the tile `_30_40`: X falls with its y, Y with its x.
    let eye = [ORIGIN - 40.5 * TILE, ORIGIN - 30.5 * TILE];
    let wanted = placing::wanted(&all, eye, 1, &HashSet::new());
    assert_eq!(wanted[0], TileId { x: 30, y: 40 });
    assert_eq!(
        wanted.len(),
        9,
        "the tiles around it, their centre within 1.5 tiles: {wanted:?}"
    );
    assert!(
        wanted[1..5]
            .iter()
            .all(|tile| tile.x.abs_diff(30) + tile.y.abs_diff(40) == 1)
    );
    // Held, kept within a tile more; not beyond.
    let held = tiles(&[TileId { x: 32, y: 40 }, TileId { x: 33, y: 40 }]);
    let kept = placing::wanted(&all, eye, 1, &held);
    assert_eq!(kept.len(), 10);
    assert_eq!(kept[9], TileId { x: 32, y: 40 });
    // A tile the WDT does not name, never.
    let mut holed = all.clone();
    holed[40 * 64 + 30] = false;
    assert!(!placing::wanted(&holed, eye, 1, &HashSet::new()).contains(&TileId { x: 30, y: 40 }));
}

/// The transform of a doodad as Noggit builds it, in its axes of the file, Y up, the model's
/// vertices turned into them; then brought into the world's.
fn noggit(doodad: &Doodad) -> Mat4 {
    let [x, y, z] = doodad.rotation.map(f32::to_radians);
    let placed = Mat4::from_translation(Vec3::from(doodad.position))
        * Mat4::from_rotation_y(y - FRAC_PI_2)
        * Mat4::from_rotation_z(-x)
        * Mat4::from_rotation_x(z)
        * Mat4::from_scale(Vec3::splat(doodad.scale));
    // A vertex of the model (x, y, z), Z up, as Noggit reads it: (x, z, -y).
    let model = Mat4::from_mat3(Mat3::from_cols(Vec3::X, -Vec3::Z, Vec3::Y));
    // A point of the file's axes in the world's: (ORIGIN - z, ORIGIN - x, y).
    let world = Mat4::from_translation(Vec3::new(ORIGIN, ORIGIN, 0.0))
        * Mat4::from_mat3(Mat3::from_cols(-Vec3::Y, Vec3::Z, -Vec3::X));
    world * placed * model
}

#[test]
fn a_doodad_stands_where_its_file_places_it_turned_as_noggit_turns_it() {
    let still = doodad(1, "a.m2", [100.0, 20.0, 300.0], [0.0; 3], 1.0);
    let at = placing::transform(&still).transform_point3(Vec3::ZERO);
    assert!(
        at.abs_diff_eq(Vec3::new(ORIGIN - 300.0, ORIGIN - 100.0, 20.0), 1e-2),
        "{at}"
    );
    for rotation in [
        [0.0, 0.0, 0.0],
        [0.0, 90.0, 0.0],
        [0.0, 237.5, 0.0],
        [12.0, 0.0, 0.0],
        [0.0, 0.0, -20.0],
        [7.5, 301.0, -14.0],
        [-33.0, 45.0, 81.0],
    ] {
        let doodad = doodad(1, "a.m2", [16_000.0, 35.0, 9_000.0], rotation, 1.75);
        let (made, expected) = (placing::transform(&doodad), noggit(&doodad));
        for point in [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(3.0, -2.0, 5.0)] {
            let (got, want) = (made.transform_point3(point), expected.transform_point3(point));
            assert!(
                got.abs_diff_eq(want, 1e-2),
                "{rotation:?} {point}: {got} against {want}"
            );
        }
    }
    // Facing along X turned about the vertical, its scale kept.
    let turned = doodad(1, "a.m2", [0.0; 3], [0.0, 90.0, 0.0], 2.0);
    let front = placing::transform(&turned).transform_vector3(Vec3::X);
    assert!(front.abs_diff_eq(Vec3::new(0.0, -2.0, 0.0), 1e-4), "{front}");
}

#[test]
fn the_instances_of_a_tile_are_its_doodads_once_each() {
    let models = Fake::default();
    let placements = Placements {
        doodads: vec![
            doodad(7, "world\\a.m2", [1.0, 2.0, 3.0], [0.0; 3], 1.0),
            doodad(8, "world\\b.m2", [4.0, 5.0, 6.0], [0.0; 3], 0.5),
            doodad(7, "world\\c.m2", [7.0, 8.0, 9.0], [0.0; 3], 1.0),
        ],
        buildings: Vec::new(),
    };
    let instances = placing::instances(&placements, &models);
    assert_eq!(
        instances.iter().map(|i| i.id).collect::<Vec<_>>(),
        [7, 8],
        "the first of an id"
    );
    let looks = models.looks.lock().unwrap().clone();
    assert_eq!(
        looks[instances[0].look.0 as usize],
        Look {
            model: FileRef::Path("world\\a.m2".to_owned()),
            textures: Vec::new(),
            geosets: Geosets::All,
        }
    );
    assert_eq!(instances[1].transform, placing::transform(&placements.doodads[1]));
    assert!(instances.iter().all(|i| i.alpha == 1.0 && i.motion == Motion::Standing));
}

#[test]
fn a_doodad_listed_by_several_tiles_is_placed_once_and_handed_on_when_its_tile_is_left() {
    let models = Fake::default();
    let mut placing = Placing::default();
    let listed = |ids: &[u64]| ids.iter().map(|id| instance(*id)).collect::<Vec<_>>();
    assert!(!placing.want(&models, "Map", tiles(&[A, B, C])));
    assert!(placing.place(&models, "Map", A, listed(&[1, 2, 3])));
    assert!(placing.place(&models, "Map", B, listed(&[3, 4, 5])));
    assert!(placing.place(&models, "Map", C, listed(&[5, 6, 3])));
    assert_eq!(
        models.sets(),
        BTreeMap::from([(owner(A), vec![1, 2, 3]), (owner(B), vec![4, 5]), (owner(C), vec![6])])
    );
    assert_eq!(placing.counts(), [3, 6, 9]);
    assert!(placing.place(&models, "Map", A, listed(&[1])), "placed already");
    assert_eq!(models.sets()[&owner(A)], [1, 2, 3]);

    // Left: its doodads that the others list go to the first of them by its place, C before B.
    assert!(placing.want(&models, "Map", tiles(&[B, C])));
    placing.settle(&models);
    assert_eq!(
        models.sets(),
        BTreeMap::from([(owner(B), vec![4, 5]), (owner(C), vec![3, 6])])
    );
    assert_eq!(models.owners.lock().unwrap()[&owner(C)][&3], instance(3));
    assert_eq!(placing.counts(), [2, 4, 6]);

    // Back: those the others place now are theirs.
    assert!(!placing.want(&models, "Map", tiles(&[A, B, C])));
    assert!(placing.place(&models, "Map", A, listed(&[1, 2, 3])));
    assert_eq!(models.sets()[&owner(A)], [1, 2]);
    assert!(placing.want(&models, "Map", tiles(&[A])));
    placing.settle(&models);
    assert_eq!(models.sets(), BTreeMap::from([(owner(A), vec![1, 2, 3])]));
    assert_eq!(placing.counts(), [1, 3, 3]);
}

#[test]
fn a_tile_is_placed_only_while_wanted_and_another_map_takes_all_away() {
    let models = Fake::default();
    let mut placing = Placing::default();
    placing.want(&models, "Map", tiles(&[A]));
    assert!(!placing.place(&models, "Map", B, vec![instance(1)]), "not wanted");
    assert!(!placing.place(&models, "Other", A, vec![instance(1)]), "of another map");
    assert!(models.sets().is_empty());
    assert!(placing.place(&models, "Map", A, vec![instance(1)]));
    // Left, then taken away; placed again only once wanted again.
    assert!(placing.want(&models, "Map", HashSet::new()));
    assert!(!placing.place(&models, "Map", A, vec![instance(1)]));
    placing.settle(&models);
    assert!(models.sets().is_empty());
    placing.want(&models, "Map", tiles(&[A]));
    assert!(placing.place(&models, "Map", A, vec![instance(1)]));
    // Another map: the tiles of the one before taken away at once.
    assert!(!placing.want(&models, "Other", tiles(&[A])));
    assert!(models.sets().is_empty());
    assert_eq!(placing.counts(), [0, 0, 0]);
    assert!(placing.place(&models, "Other", A, vec![instance(1)]));
    assert_eq!(models.sets(), BTreeMap::from([(placing::owner("Other", A), vec![1])]));
}

#[test]
fn the_owners_of_the_tiles_are_named_after_the_module() {
    assert_eq!(
        placing::owner("Azeroth", TileId { x: 32, y: 48 }),
        "doodads/Azeroth/32_48"
    );
}
