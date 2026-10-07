//! Tests of the entities drawn as models: their looks read once, their instances placed, and the
//! markers kept for what is not seen as a model; against a fake service `models`.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, MapRecord, Model, Texture, Tile, Wdl, Wdt,
};
use uniwow_api::glam::{Mat4, Quat, Vec3};
use uniwow_api::models::{Extent, Geosets, Instance, Look, LookId, LookState, Models, Motion};
use uniwow_api::server_link::protocol::{Entity, Kind, WALKING};

use crate::looks::{self, Looks, Resolved};
use crate::markers::{self, Drawn, Frame, SIZE};
use crate::markers_tests::{entity, path};
use crate::world::{Tracked, World};

/// A creature display of the fake: its model's file is named by its id, its scale is 2; 99 is not
/// in the tables. A game object display: 1 an M2, 2 a WMO, others not in the tables.
#[derive(Default)]
struct Fake {
    looks: Mutex<Vec<Look>>,
    extents: Mutex<HashMap<LookId, Extent>>,
    placed: Mutex<Vec<Instance>>,
    displays: Mutex<Vec<u32>>,
}

fn look_of(file: &str) -> Look {
    Look {
        model: FileRef::Path(file.to_owned()),
        textures: Vec::new(),
        geosets: Geosets::Default,
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
    fn display(&self, display: u32) -> Result<(Look, f32), String> {
        self.displays.lock().unwrap().push(display);
        match display {
            99 => Err("the display 99: not in CreatureDisplayInfo".to_owned()),
            _ => Ok((look_of(&format!("creature {display}.m2")), 2.0)),
        }
    }
    fn object(&self, display: u32) -> Result<Option<Look>, String> {
        match display {
            1 => Ok(Some(look_of("object.m2"))),
            2 => Ok(None),
            _ => Err(format!(
                "the game object display {display}: not in GameObjectDisplayInfo"
            )),
        }
    }
    fn place(&self, _owner: &str, instances: &[Instance]) {
        *self.placed.lock().unwrap() = instances.to_vec();
    }
    fn change(&self, _owner: &str, _changed: &[Instance], _removed: &[u64]) {}
    fn clear(&self, _owner: &str) {}
    fn state(&self, look: LookId) -> LookState {
        match self.extents.lock().unwrap().contains_key(&look) {
            true => LookState::Drawn,
            false => LookState::Waiting,
        }
    }
    fn extent(&self, look: LookId) -> Option<Extent> {
        self.extents.lock().unwrap().get(&look).copied()
    }

    fn shown(&self, _owner: &str) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }
}

/// Tables of the fake: the creature display 7 of alpha 128.
struct Tables;

impl Formats for Tables {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("no maps".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Err("no areas".to_owned())
    }
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        Ok(Arc::new(vec![CreatureDisplay {
            id: 7,
            model: 1,
            scale: 1.0,
            alpha: 128,
            textures: Default::default(),
            extra: 0,
            geosets: 0,
        }]))
    }
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        Err("no models".to_owned())
    }
    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String> {
        Err("no looks".to_owned())
    }
    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String> {
        Err("no hairs".to_owned())
    }
    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String> {
        Err("no facial hairs".to_owned())
    }
    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        Err("no objects".to_owned())
    }
    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String> {
        Err("no sections".to_owned())
    }
    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String> {
        Err("no animations".to_owned())
    }
    fn model(&self, _file: &FileRef) -> Result<Model, String> {
        Err("no model".to_owned())
    }
    fn wmo(&self, _file: &FileRef) -> Result<uniwow_api::formats::Wmo, String> {
        Err("no building".to_owned())
    }
    fn wdt(&self, _directory: &str) -> Result<Arc<Wdt>, String> {
        Err("no WDT".to_owned())
    }
    fn tile(&self, _directory: &str, _x: u32, _y: u32) -> Result<Option<Tile>, String> {
        Ok(None)
    }
    fn wdl(&self, _directory: &str) -> Result<Option<Wdl>, String> {
        Ok(None)
    }
    fn texture(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("no texture".to_owned())
    }
    fn texture_rgba(&self, _file: &FileRef) -> Result<Texture, String> {
        Err("no texture".to_owned())
    }
}

/// A look drawn of `batches`, 2 yards high, of radius 1, drawn up to 100 times its radius.
fn extent(batches: usize) -> Extent {
    Extent {
        low: Vec3::ZERO,
        high: Vec3::new(0.0, 0.0, 2.0),
        radius: 1.0,
        batches,
        reach: 100.0,
    }
}

fn world(entities: Vec<Entity>, now: Instant) -> World {
    let mut world = World::default();
    for entity in entities {
        world
            .entities
            .insert(entity.guid, Arc::new(Tracked { entity, received: now }));
    }
    world
}

fn frame(world: &World, now: Instant, eye: Vec3, looks: &Looks, fake: &Fake) -> Frame {
    let resolved = looks.resolved();
    markers::build(
        world,
        now,
        Some(eye),
        Some(Drawn {
            looks: &resolved,
            service: fake,
        }),
    )
}

#[test]
fn a_display_is_read_once_its_look_and_scale_and_alpha_a_failure_kept() {
    let (fake, looks) = (Fake::default(), Looks::default());
    let displays = [
        (Kind::Creature, 7),
        (Kind::Creature, 99),
        (Kind::GameObject, 1),
        (Kind::GameObject, 2),
        (Kind::GameObject, 3),
    ];
    // Wanted twice, read once; wanted again once read, not given again.
    looks.want(displays);
    looks.want([(Kind::Creature, 7)]);
    let wanted = looks.take_wanted();
    assert_eq!(wanted.len(), 5);
    looks.insert(looks::read(&fake, &Tables, &wanted));
    looks.want(displays);
    assert!(looks.take_wanted().is_empty());
    let mut read = fake.displays.lock().unwrap().clone();
    read.sort();
    assert_eq!(read, [7, 99], "each creature display read once");
    let resolved = looks.resolved();
    let creature = fake.look(&look_of("creature 7.m2"));
    assert_eq!(
        resolved[&(Kind::Creature, 7)],
        Resolved::Look {
            look: creature,
            scale: 2.0,
            alpha: 128.0 / 255.0
        }
    );
    assert!(matches!(&resolved[&(Kind::Creature, 99)], Resolved::Marker(why) if why.contains("99")));
    let object = fake.look(&look_of("object.m2"));
    assert_eq!(
        resolved[&(Kind::GameObject, 1)],
        Resolved::Look {
            look: object,
            scale: 1.0,
            alpha: 1.0
        }
    );
    assert!(matches!(&resolved[&(Kind::GameObject, 2)], Resolved::Marker(why) if why.contains("WMO")));
    assert!(matches!(resolved[&(Kind::GameObject, 3)], Resolved::Marker(_)));
}

#[test]
fn an_entity_seen_as_its_model_has_no_marker_and_its_name_over_its_model() {
    let now = Instant::now();
    let (fake, looks) = (Fake::default(), Looks::default());
    looks.insert(looks::read(
        &fake,
        &Tables,
        &[(Kind::Creature, 7), (Kind::GameObject, 2)],
    ));
    let look = fake.look(&look_of("creature 7.m2"));
    let mut big = entity(1, Kind::Creature, [10.0, 0.0, 5.0]);
    (big.display, big.scale, big.orientation) = (7, 1.5, 1.0);
    let mut player = entity(2, Kind::Player, [12.0, 0.0, 5.0]);
    player.display = 7;
    let mut unread = entity(3, Kind::Creature, [14.0, 0.0, 5.0]);
    unread.display = 8;
    let mut wmo = entity(4, Kind::GameObject, [16.0, 0.0, 5.0]);
    wmo.display = 2;
    let world = world(vec![big, player, unread, wmo], now);

    // Its look not drawn yet: placed all the same, which makes it load, and a marker.
    let first = frame(&world, now, Vec3::ZERO, &looks, &fake);
    assert_eq!(first.placed.len(), 1);
    assert_eq!(first.markers.len(), 4);
    assert_eq!(first.wanted, [(Kind::Creature, 8)], "the unread display asked for");
    let placed = first.placed[0];
    assert_eq!((placed.id, placed.look, placed.alpha), (1, look, 128.0 / 255.0));
    assert!(placed.transform.abs_diff_eq(
        Mat4::from_scale_rotation_translation(Vec3::splat(3.0), Quat::from_rotation_z(1.0), Vec3::new(10.0, 0.0, 5.0)),
        1e-5
    ));

    // Drawn: no marker; its name at the top of its model, 2 yards times 3.
    fake.extents.lock().unwrap().insert(look, extent(4));
    let drawn = frame(&world, now, Vec3::ZERO, &looks, &fake);
    assert_eq!(drawn.markers.len(), 3);
    assert!(drawn.markers.iter().all(|marker| marker.centre[0] != 10.0));
    let label = drawn.labels.iter().find(|label| label.text == "entity 1").unwrap();
    assert_eq!(label.position, Vec3::new(10.0, 0.0, 5.0 + 6.0));
    let marked = drawn.labels.iter().find(|label| label.text == "entity 2").unwrap();
    assert_eq!(marked.position.z, 5.0 + 2.0 * SIZE, "a player over its marker");

    // Beyond its reach, 100 times its radius times its scale: a marker again.
    let far = frame(&world, now, Vec3::new(10.0 + 301.0, 0.0, 5.0), &looks, &fake);
    assert_eq!(far.markers.len(), 4);
    let near = frame(&world, now, Vec3::new(10.0 + 299.0, 0.0, 5.0), &looks, &fake);
    assert_eq!(near.markers.len(), 3);

    // A model drawing nothing at rest, as the triggers': a marker.
    fake.extents.lock().unwrap().insert(look, extent(0));
    assert_eq!(frame(&world, now, Vec3::ZERO, &looks, &fake).markers.len(), 4);
}

#[test]
fn an_entity_moving_along_its_spline_is_placed_walking_as_its_flags_say() {
    let now = Instant::now();
    let (fake, looks) = (Fake::default(), Looks::default());
    looks.insert(looks::read(&fake, &Tables, &[(Kind::Creature, 7)]));
    let mut walker = entity(1, Kind::Creature, [0.0; 3]);
    (walker.display, walker.flags) = (7, WALKING);
    walker.spline = Some(path(&[([0.0, 0.0], 0), ([10.0, 0.0], 1000)], 0, 0));
    let mut standing = entity(2, Kind::Creature, [5.0, 0.0, 0.0]);
    standing.display = 7;
    let world = world(vec![walker, standing], now);
    let placed = frame(&world, now, Vec3::ZERO, &looks, &fake).placed;
    let motions: HashMap<u64, Motion> = placed.iter().map(|placed| (placed.id, placed.motion)).collect();
    assert_eq!(
        motions,
        HashMap::from([(1, Motion::Walking(10.0)), (2, Motion::Standing)])
    );
}

#[test]
fn a_creature_turns_the_way_it_moves_and_a_game_object_by_its_quaternion() {
    let now = Instant::now();
    let mut walker = entity(1, Kind::Creature, [0.0, 0.0, 0.0]);
    walker.spline = Some(path(&[([0.0, 0.0], 0), ([10.0, 10.0], 10_000)], 0, 0));
    let walker = Tracked {
        entity: walker,
        received: now,
    };
    let way = |at: Instant| walker.rotation_at(at).to_euler(uniwow_api::glam::EulerRot::ZYX).0;
    let quarter = std::f32::consts::FRAC_PI_4;
    assert!(
        (way(now + Duration::from_secs(5)) - quarter).abs() < 1e-3,
        "along its way"
    );
    assert!(
        (way(now + Duration::from_secs(20)) - quarter).abs() < 1e-3,
        "at its end, the way it came"
    );
    let mut standing = entity(2, Kind::Creature, [0.0, 0.0, 0.0]);
    standing.orientation = 2.0;
    let standing = Tracked {
        entity: standing,
        received: now,
    };
    assert!(standing.rotation_at(now).abs_diff_eq(Quat::from_rotation_z(2.0), 1e-6));
    let mut object = entity(3, Kind::GameObject, [0.0, 0.0, 0.0]);
    let tilted = Quat::from_rotation_x(0.5) * Quat::from_rotation_z(1.0);
    object.rotation = Some(tilted.to_array());
    object.orientation = 1.0;
    let object = Tracked {
        entity: object,
        received: now,
    };
    assert!(object.rotation_at(now).abs_diff_eq(tilted, 1e-6));
}

#[test]
fn a_morph_takes_the_look_of_its_new_display() {
    let now = Instant::now();
    let (fake, looks) = (Fake::default(), Looks::default());
    looks.insert(looks::read(&fake, &Tables, &[(Kind::Creature, 7), (Kind::Creature, 8)]));
    let mut creature = entity(1, Kind::Creature, [0.0, 0.0, 0.0]);
    creature.display = 7;
    let before = frame(&world(vec![creature.clone()], now), now, Vec3::ZERO, &looks, &fake);
    creature.display = 8;
    let after = frame(&world(vec![creature], now), now, Vec3::ZERO, &looks, &fake);
    assert_eq!(before.placed[0].look, fake.look(&look_of("creature 7.m2")));
    assert_eq!(after.placed[0].look, fake.look(&look_of("creature 8.m2")));
}
