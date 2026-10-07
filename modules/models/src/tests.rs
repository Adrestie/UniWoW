//! Tests of the models on data the tests make themselves, never on files of the client; drawn on
//! the software adapter of the system when it has one.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, Batch, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair, FileRef,
    Formats, GameObjectDisplay, HairGeoset, MapRecord, Material, Model, ModelTexture, ModelTextureSource, ModelVertex,
    Skin, Submesh, Texture, TextureFormat, Tile, Wdl, Wdt,
};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::models::{Extent, Geosets, Instance, Look, LookId, LookState, Models, Motion};
use uniwow_api::viewport::{Drawing, Layer, Phase, Target, View};
use uniwow_api::{Event, MODULE_FAILED_TOPIC, bytemuck, egui, egui_wgpu, serde_json, wgpu};

use crate::cache::Cache;
use crate::choice::Tables;
use crate::display::{self, CHARACTER_SKIN, CREATURE_SKIN, HAIR, SKIN_EXTRA};
use crate::gpu::{self, ALPHA_KEY, InstanceGpu, Shared, State};
use crate::groups::{self, Group, TILE};
use crate::layer::{self, LIMITS, MARGIN, ModelsLayer, Scene};
use crate::loading::{self, Caches, Ready};
use crate::pool;
use crate::service::Service;
use crate::{forget_failed, lock};

/// The tables, a model and the textures a test gives.
#[derive(Default)]
pub struct Fake {
    pub displays: Vec<CreatureDisplay>,
    pub models: Vec<CreatureModel>,
    pub looks: Vec<CreatureLook>,
    pub hairs: Vec<HairGeoset>,
    pub facials: Vec<FacialHair>,
    pub sections: Vec<CharSection>,
    pub objects: Vec<GameObjectDisplay>,
    pub model: Option<Model>,
    /// The models of the files they name, `model` for the others.
    pub files: HashMap<String, Model>,
    pub textures: HashMap<String, Texture>,
}

impl Formats for Fake {
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String> {
        Err("no maps".to_owned())
    }
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String> {
        Err("no areas".to_owned())
    }
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        Ok(Arc::new(self.displays.clone()))
    }
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String> {
        Ok(Arc::new(self.models.clone()))
    }
    fn creature_looks(&self) -> Result<Arc<Vec<CreatureLook>>, String> {
        Ok(Arc::new(self.looks.clone()))
    }
    fn hair_geosets(&self) -> Result<Arc<Vec<HairGeoset>>, String> {
        Ok(Arc::new(self.hairs.clone()))
    }
    fn facial_hairs(&self) -> Result<Arc<Vec<FacialHair>>, String> {
        Ok(Arc::new(self.facials.clone()))
    }
    fn game_object_displays(&self) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        Ok(Arc::new(self.objects.clone()))
    }
    fn char_sections(&self) -> Result<Arc<Vec<CharSection>>, String> {
        Ok(Arc::new(self.sections.clone()))
    }
    fn animations(&self) -> Result<Arc<Vec<AnimationRecord>>, String> {
        Err("no animations".to_owned())
    }
    fn model(&self, file: &FileRef) -> Result<Model, String> {
        if let FileRef::Path(path) = file
            && let Some(model) = self.files.get(path)
        {
            return Ok(model.clone());
        }
        self.model.clone().ok_or_else(|| "no model".to_owned())
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
    fn texture(&self, file: &FileRef) -> Result<Texture, String> {
        self.texture_rgba(file)
    }
    fn texture_rgba(&self, file: &FileRef) -> Result<Texture, String> {
        let FileRef::Path(path) = file else {
            return Err("by id".to_owned());
        };
        self.textures
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path}: not in the client"))
    }
}

/// A texture of 4 × 4 texels, all `colour`.
pub fn plain(colour: [u8; 4]) -> Texture {
    Texture {
        width: 4,
        height: 4,
        format: TextureFormat::Rgba8,
        levels: vec![colour.repeat(16)],
    }
}

pub fn instance(id: u64, look: u32, at: Vec3, scale: f32) -> Instance {
    Instance {
        id,
        look: LookId(look),
        transform: Mat4::from_scale_rotation_translation(Vec3::splat(scale), Default::default(), at),
        alpha: 1.0,
        motion: Motion::Standing,
    }
}

// The cache shared by the looks.

#[test]
fn a_load_in_flight_is_shared_and_a_value_held_is_read_once() {
    let cache: Cache<u32, u32> = Cache::default();
    let reads = AtomicUsize::new(0);
    let values: Vec<Arc<u32>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    cache
                        .get(&1, || {
                            reads.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(50));
                            Ok(7)
                        })
                        .unwrap()
                })
            })
            .collect();
        threads.into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    assert_eq!(reads.load(Ordering::Relaxed), 1, "read once for eight threads");
    assert!(values.iter().all(|value| Arc::ptr_eq(value, &values[0])));
    assert_eq!(cache.counts(), (1, 0));
    // Read again once no look holds it.
    drop(values);
    cache.purge();
    assert_eq!(cache.counts(), (0, 0));
    assert_eq!(*cache.get(&1, || Ok(8)).unwrap(), 8);
}

#[test]
fn a_value_refused_is_not_read_again_and_a_load_that_fails_lets_its_waiters_go() {
    let cache: Cache<u32, u32> = Cache::default();
    assert_eq!(
        cache.get(&1, || Err("unreadable".to_owned())).unwrap_err(),
        "unreadable"
    );
    assert_eq!(cache.get(&1, || Ok(1)).unwrap_err(), "unreadable", "not read again");
    assert_eq!(cache.counts(), (0, 1));
    let waited = std::thread::scope(|scope| {
        let loading = scope.spawn(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache.get(&2, || -> Result<u32, String> {
                    std::thread::sleep(Duration::from_millis(100));
                    panic!("the load fails")
                })
            }))
        });
        std::thread::sleep(Duration::from_millis(20));
        let waiting = scope.spawn(|| cache.get(&2, || Ok(5)));
        assert!(loading.join().unwrap().is_err());
        waiting.join().unwrap()
    });
    assert_eq!(waited.unwrap_err(), "its load failed", "the waiter told");
    assert_eq!(*cache.get(&2, || Ok(6)).unwrap(), 6, "asked again after the failure");
}

// The groups of the owners.

#[test]
fn instances_are_grouped_by_look_and_tile_their_box_and_largest_scale_kept() {
    let mut instances = vec![
        instance(4, 1, Vec3::new(10.0, 10.0, 0.0), 1.0),
        instance(1, 0, Vec3::new(TILE + 5.0, 0.0, 0.0), 1.0),
        instance(2, 0, Vec3::new(1.0, 2.0, 3.0), 2.0),
        instance(3, 0, Vec3::new(5.0, -4.0, 1.0), 1.0),
    ];
    let groups = groups::group(&mut instances);
    let ids: Vec<u64> = instances.iter().map(|instance| instance.id).collect();
    assert_eq!(ids, [3, 2, 1, 4], "by look, tile, then id");
    assert_eq!(
        groups
            .iter()
            .map(|group| (group.look.0, group.tile, group.first, group.count))
            .collect::<Vec<_>>(),
        [
            (0, [0, -1], 0, 1),
            (0, [0, 0], 1, 1),
            (0, [1, 0], 2, 1),
            (1, [0, 0], 3, 1)
        ]
    );
    let mut same_tile = vec![
        instance(1, 0, Vec3::new(1.0, 2.0, 3.0), 2.0),
        instance(2, 0, Vec3::new(5.0, 4.0, 1.0), 1.0),
    ];
    let group = &groups::group(&mut same_tile)[0];
    assert_eq!(
        (group.low, group.high, group.scale),
        (Vec3::new(1.0, 2.0, 1.0), Vec3::new(5.0, 4.0, 3.0), 2.0)
    );
}

#[test]
fn a_partial_update_replaces_adds_and_removes_by_id() {
    let kept = vec![instance(1, 0, Vec3::ZERO, 1.0), instance(2, 0, Vec3::X, 1.0)];
    let merged = groups::merge(
        &kept,
        &[instance(2, 1, Vec3::Y, 1.0), instance(3, 0, Vec3::Z, 1.0)],
        &[1],
    );
    let seen: Vec<(u64, u32)> = merged.iter().map(|instance| (instance.id, instance.look.0)).collect();
    assert_eq!(seen, [(2, 1), (3, 0)]);
}

#[test]
fn groups_keep_their_places_only_with_the_same_looks_tiles_and_counts() {
    let group = |look: u32, first, count| Group {
        look: LookId(look),
        tile: [0, 0],
        first,
        count,
        low: Vec3::ZERO,
        high: Vec3::ONE,
        scale: 1.0,
    };
    let old = [group(0, 0, 2), group(1, 2, 1)];
    let mut moved = old.clone();
    moved[0].high = Vec3::splat(9.0);
    assert!(groups::same_places(&old, &moved), "moving within keeps the places");
    assert!(!groups::same_places(&old, &[group(0, 0, 3), group(1, 3, 1)]));
    assert!(!groups::same_places(&old, &[group(0, 0, 2)]));
}

// The service.

#[test]
fn the_same_look_has_the_same_id_and_an_owner_keeps_its_set_until_it_gives_another() {
    let service = Service::default();
    let look = Look {
        model: FileRef::Path("a.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let other = Look {
        geosets: Geosets::Default,
        ..look.clone()
    };
    let (a, b) = (service.look(&look), service.look(&other));
    assert_ne!(a, b);
    assert_eq!(service.look(&look), a);
    assert_eq!(service.state(a), LookState::Waiting);
    assert!(matches!(service.state(LookId(9)), LookState::Refused(_)));
    service.set_state(a, LookState::Drawn);
    assert_eq!(service.state(a), LookState::Drawn);
    // Without the device of the view: kept, written by the next change.
    service.place("one", &[instance(1, a.0, Vec3::ZERO, 1.0)]);
    service.change("one", &[instance(2, a.0, Vec3::X, 1.0)], &[]);
    assert_eq!(service.owners().len(), 1);
    assert!(service.owners()[0].published().buffer.is_none());
    service.clear("one");
    assert!(service.owners().is_empty());
}

#[test]
fn the_instances_of_a_module_that_fails_are_cleared() {
    let service = Service::default();
    service.place("live-world", &[]);
    service.place("live-world/tile", &[]);
    service.place("live-worlds", &[]);
    service.place("other", &[]);
    let event = |topic: &str| Event {
        topic: topic.to_owned(),
        source: "kernel".to_owned(),
        payload: serde_json::json!({ "id": "live-world" }),
    };
    forget_failed(&service, &event("another.topic"));
    assert_eq!(service.owners().len(), 4);
    forget_failed(&service, &event(MODULE_FAILED_TOPIC));
    assert_eq!(service.owners().len(), 2, "its own owners only");
}

// The layer's rules.

#[test]
fn a_level_changes_only_past_its_limit_by_the_margin() {
    assert_eq!(layer::level(10.0, None, 4), 0);
    assert_eq!(layer::level(LIMITS[0] + 1.0, None, 4), 1);
    // Going away: kept until the margin past the limit.
    assert_eq!(layer::level(LIMITS[0] * (1.0 + MARGIN / 2.0), Some(0), 4), 0);
    assert_eq!(layer::level(LIMITS[0] * (1.0 + MARGIN * 2.0), Some(0), 4), 1);
    // Coming nearer: kept until the margin before the limit.
    assert_eq!(layer::level(LIMITS[0] * (1.0 - MARGIN / 2.0), Some(1), 4), 1);
    assert_eq!(layer::level(LIMITS[0] * (1.0 - MARGIN * 2.0), Some(1), 4), 0);
    // Far: the last level, never past the skins a model has.
    assert_eq!(layer::level(1e6, None, 4), 3);
    assert_eq!(layer::level(1e6, None, 2), 1);
    assert_eq!(layer::level(1e6, Some(0), 1), 0);
}

#[test]
fn the_blended_order_is_kept_until_two_cross_by_the_margin() {
    assert!(layer::still_ordered(&[100.0, 50.0, 10.0]));
    // Swapped by less than the margin: kept.
    assert!(layer::still_ordered(&[100.0, 102.0]));
    assert!(layer::still_ordered(&[10.0, 11.5]));
    assert!(!layer::still_ordered(&[100.0, 110.0]));
    assert!(!layer::still_ordered(&[10.0, 13.0]));
}

#[test]
fn the_nearest_point_of_a_box_and_whether_it_is_in_sight() {
    let bounds = [Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 2.0)];
    assert_eq!(layer::nearest(Vec3::new(4.0, 0.0, 1.0), bounds), 3.0);
    assert_eq!(layer::nearest(Vec3::ZERO, bounds), 0.0);
    let projection = Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1);
    let eye = Vec3::new(10.0, 0.0, 1.0);
    assert!(layer::in_sight(
        projection * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z),
        bounds
    ));
    let away = Vec3::new(20.0, 0.0, 1.0);
    assert!(!layer::in_sight(
        projection * Mat4::look_at_rh(eye, away, Vec3::Z),
        bounds
    ));
}

// The materials.

#[test]
fn a_material_gives_its_pipeline_state_and_the_flags_of_its_shader() {
    let material = |flags, blending| Material { flags, blending };
    let opaque = State::of(&material(0, 0));
    assert_eq!(
        opaque,
        State {
            blending: 0,
            two_sided: false,
            depth_test: true,
            depth_write: true
        }
    );
    assert!(!opaque.blended());
    let alpha = State::of(&material(0x04 | 0x08, 2));
    assert!(alpha.blended() && alpha.two_sided && !alpha.depth_test && !alpha.depth_write);
    assert!(!State::of(&material(0x10, 1)).depth_write, "the flag 0x10");
    assert_eq!(State::of(&material(0, 9)).blending, 0, "an unknown blending is opaque");
    assert_eq!(gpu::flags(&material(0, 1)), [ALPHA_KEY, 0.0, 0.0, 0.0]);
    assert_eq!(
        gpu::flags(&material(0x01 | 0x02, 4)),
        [0.0, 1.0, 1.0, 1.0],
        "add: unlit, unfogged, black fog"
    );
    assert_eq!(
        gpu::flags(&material(0, 5)),
        [0.0, 1.0, 0.0, 2.0],
        "mod: unlit, white fog"
    );
    assert_eq!(
        gpu::flags(&material(0, 6)),
        [0.0, 1.0, 0.0, 3.0],
        "mod2x: unlit, grey fog"
    );
    assert_eq!(gpu::flags(&material(0, 2)), [0.0; 4]);
}

// The looks of displays.

/// The tables of a wolf of two skins, an iron dwarf of variants, and a human of a look.
fn tables() -> Fake {
    let display = |id, model, extra, scale, textures: [&str; 3], geosets| CreatureDisplay {
        id,
        model,
        scale,
        alpha: 255,
        textures: textures.map(str::to_owned),
        extra,
        geosets,
    };
    let model = |id, path: &str, scale| CreatureModel {
        id,
        flags: 0,
        path: path.to_owned(),
        scale,
    };
    // A row of `CharSections` for the human male.
    let section = |section, variation, colour, textures: [&str; 2]| CharSection {
        race: 1,
        sex: 0,
        section,
        variation,
        colour,
        textures: [textures[0].to_owned(), textures[1].to_owned(), String::new()],
    };
    Fake {
        displays: vec![
            display(1, 10, 0, 1.5, ["WolfGrey", "", "WolfEyes"], 0),
            display(2, 11, 0, 0.0, ["", "", ""], 0x21),
            display(3, 12, 7, 1.0, ["", "", ""], 0),
            display(4, 99, 0, 1.0, ["", "", ""], 0),
        ],
        models: vec![
            model(10, "Creature\\Wolf\\Wolf.mdx", 2.0),
            model(11, "Creature\\IronDwarf\\IronDwarf.mdx", 1.0),
            model(12, "Character\\Human\\Male\\HumanMale.mdx", 1.0),
        ],
        looks: vec![CreatureLook {
            id: 7,
            race: 1,
            sex: 0,
            skin: 5,
            face: 0,
            hair_style: 2,
            hair_colour: 4,
            facial_hair: 1,
            items: [0; 11],
            flags: 0,
            baked: "abc.blp".to_owned(),
        }],
        hairs: vec![HairGeoset {
            race: 1,
            sex: 0,
            variation: 2,
            geoset: 3,
            scalp: false,
        }],
        facials: vec![FacialHair {
            race: 1,
            sex: 0,
            variation: 1,
            geosets: [1, 2, 1, 0, 0],
        }],
        sections: vec![
            section(3, 2, 4, ["Character\\Human\\Hair02_04.blp", ""]),
            // Its skin, of its colour 5: its extra second; another colour, and a face of its colour.
            section(0, 0, 5, ["HumanMaleSkin00_05.blp", "HumanMaleSkin00_05_Extra.blp"]),
            section(0, 0, 4, ["HumanMaleSkin00_04.blp", "HumanMaleSkin00_04_Extra.blp"]),
            section(1, 0, 5, ["HumanMaleFaceLower00_05.blp", "HumanMaleFaceUpper00_05.blp"]),
        ],
        ..Fake::default()
    }
}

#[test]
fn a_creature_display_gives_its_skins_in_the_folder_of_its_model_and_its_variants() {
    let tables = tables();
    let (wolf, scale) = display::display(&tables, 1).unwrap();
    assert_eq!(scale, 3.0, "of the display and of its model");
    assert_eq!(wolf.model, FileRef::Path("Creature\\Wolf\\Wolf.mdx".to_owned()));
    assert_eq!(
        wolf.textures,
        [
            (CREATURE_SKIN, FileRef::Path("Creature\\Wolf\\WolfGrey.blp".to_owned())),
            (
                CREATURE_SKIN + 2,
                FileRef::Path("Creature\\Wolf\\WolfEyes.blp".to_owned())
            ),
        ]
    );
    assert_eq!(wolf.geosets, Geosets::All, "no variant: every submesh");
    let (dwarf, scale) = display::display(&tables, 2).unwrap();
    assert_eq!(
        (dwarf.geosets, scale),
        (Geosets::Creature(0x21), 1.0),
        "a scale of 0 is 1"
    );
    assert!(display::display(&tables, 4).unwrap_err().contains("its model 99"));
    assert!(
        display::display(&tables, 5)
            .unwrap_err()
            .contains("not in CreatureDisplayInfo")
    );
}

#[test]
fn a_character_display_gives_the_model_of_its_race_its_baked_skin_its_extra_and_its_hair() {
    let (human, _) = display::display(&tables(), 3).unwrap();
    assert_eq!(
        human.model,
        FileRef::Path("Character\\Human\\Male\\HumanMale.mdx".to_owned())
    );
    assert_eq!(
        human.textures,
        [
            (
                CHARACTER_SKIN,
                FileRef::Path("Textures\\BakedNpcTextures\\abc.blp".to_owned())
            ),
            (SKIN_EXTRA, FileRef::Path("HumanMaleSkin00_05_Extra.blp".to_owned())),
            (HAIR, FileRef::Path("Character\\Human\\Hair02_04.blp".to_owned())),
        ]
    );
    assert_eq!(
        human.geosets,
        Geosets::Character {
            hair: 3,
            facial: [1, 2, 1, 0, 0]
        }
    );
}

#[test]
fn a_look_shows_the_submeshes_its_rule_chooses() {
    let ids = [0, 1, 3, 101, 102, 302, 401, 501, 702, 1301];
    let shown = |geosets| {
        let flags = loading::shown(&ids, geosets);
        ids.iter()
            .zip(flags)
            .filter(|(_, shown)| *shown)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(Geosets::All), ids);
    assert_eq!(shown(Geosets::Default), [0, 101, 401, 501, 1301]);
    assert_eq!(shown(Geosets::Creature(0x2)), [0, 102, 1301], "from 900 on, shown");
    assert_eq!(
        shown(Geosets::Character {
            hair: 3,
            facial: [1, 2, 0, 0, 0]
        }),
        [0, 3, 101, 302, 401, 501, 702, 1301]
    );
    assert_eq!(
        loading::key(&FileRef::Path("Creature/Wolf/WOLF.MDX".to_owned())),
        "creature\\wolf\\wolf.m2"
    );
    assert_eq!(loading::key(&FileRef::Id(42)), "#42");
}

#[test]
fn a_game_object_display_gives_its_m2_with_its_default_submeshes_and_none_for_a_wmo() {
    let object = |id: u32, path: &str| GameObjectDisplay {
        id,
        path: path.to_owned(),
    };
    let fake = Arc::new(Fake {
        objects: vec![
            object(1, "World/Generic/Chair.mdx"),
            object(2, "World/wmo/Hut.WMO"),
            object(4, ""),
        ],
        ..Fake::default()
    });
    let service = Service::default();
    *lock(&service.formats) = Some(fake);
    assert_eq!(
        service.object(1),
        Ok(Some(Look {
            model: FileRef::Path("World/Generic/Chair.mdx".to_owned()),
            textures: Vec::new(),
            geosets: Geosets::Default,
        }))
    );
    assert_eq!(service.object(2), Ok(None));
    assert!(service.object(3).unwrap_err().contains("not in GameObjectDisplayInfo"));
    assert!(service.object(4).unwrap_err().contains("no model"));
}

#[test]
fn a_look_drawn_tells_its_extent_until_it_is_released() {
    let service = Service::default();
    let plain_look = Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let look = service.look(&plain_look);
    assert_eq!(service.extent(look), None, "not drawn");
    service.set_reach(100.0);
    let extent = Extent {
        low: Vec3::new(0.0, -1.0, 0.0),
        high: Vec3::new(0.0, 1.0, 2.0),
        radius: 1.5,
        batches: 1,
        reach: 0.0,
    };
    service.set_drawn(look, extent);
    assert_eq!(service.state(look), LookState::Drawn);
    let told = service.extent(look).unwrap();
    assert_eq!(told, Extent { reach: 100.0, ..extent }, "with the setting");
    assert_eq!(told.distance(2.0), 300.0);
    assert_eq!(told.distance(0.1), 100.0, "at least the reach");
    service.set_state(look, LookState::Waiting);
    assert_eq!(service.extent(look), None, "released");
    // On the GPU: the bounds of its vertices at rest, its batches seen, its radius.
    let Some(gpu) = device() else {
        return;
    };
    let fake = Fake {
        model: Some(square(0, 0)),
        textures: HashMap::from([("red.blp".to_owned(), plain([255, 0, 0, 255]))]),
        ..Fake::default()
    };
    for slots in [None, Some(pool::SLOTS)] {
        let shared = Shared::new(&gpu, &TARGET, slots);
        let ready = crate::load(&shared, &fake, &Caches::default(), &plain_look, &mut Vec::new()).unwrap();
        assert_eq!(ready.extent(100.0), Extent { reach: 100.0, ..extent }, "{slots:?}");
    }
}

// Drawn on the software adapter.

fn resolved<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A device of the software adapter of the system, or none.
pub fn device() -> Option<egui_wgpu::RenderState> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: true,
        ..Default::default()
    }))?
    .ok()?;
    // What the pool needs, as the editor asks for it.
    let mut limits = wgpu::Limits::default();
    limits.max_sampled_textures_per_shader_stage = adapter.limits().max_sampled_textures_per_shader_stage.min(128);
    let (device, queue) = resolved(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features()
            & (wgpu::Features::INDIRECT_FIRST_INSTANCE | wgpu::Features::MULTI_DRAW_INDIRECT_COUNT),
        required_limits: limits,
        ..Default::default()
    }))?
    .ok()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    Some(egui_wgpu::RenderState {
        adapter,
        available_adapters: Vec::new(),
        instance,
        device,
        queue,
        target_format: format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
    })
}

pub const TARGET: Target = Target {
    color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    depth_format: wgpu::TextureFormat::Depth32Float,
    sample_count: 1,
    depth_compare: wgpu::CompareFunction::Greater,
};

/// A model of a square facing +X, 2 yards high, its two triangles counterclockwise seen from
/// there, a batch of `flags` and `blending` with the texture `red.blp`.
pub fn square(flags: u16, blending: u16) -> Model {
    let vertex = |y: f32, z: f32, u: f32, v: f32| ModelVertex {
        position: [0.0, y, z],
        normal: [1.0, 0.0, 0.0],
        uv: [[u, v], [0.0, 0.0]],
        bone_weights: [255, 0, 0, 0],
        bone_indices: [0; 4],
    };
    Model {
        version: 264,
        flags: 0,
        vertices: vec![
            vertex(-1.0, 0.0, 0.0, 1.0),
            vertex(1.0, 0.0, 1.0, 1.0),
            vertex(1.0, 2.0, 1.0, 0.0),
            vertex(-1.0, 2.0, 0.0, 0.0),
        ],
        textures: vec![ModelTexture {
            source: ModelTextureSource::File(FileRef::Path("red.blp".to_owned())),
            flags: 0,
        }],
        materials: vec![Material { flags, blending }],
        texture_combos: vec![0],
        uv_combos: vec![0],
        weight_combos: vec![0],
        transform_combos: vec![0xFFFF],
        combiner_combos: Vec::new(),
        colours: Vec::new(),
        weights: vec![1.0],
        bounds: [[0.0, -1.0, 0.0], [0.0, 1.0, 2.0]],
        radius: 1.5,
        animation: Default::default(),
        skins: vec![Skin {
            triangles: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                id: 0,
                start: 0,
                count: 6,
                centre: [0.0, 0.0, 1.0],
                radius: 1.5,
            }],
            batches: vec![Batch {
                flags: 0,
                priority: 0,
                shader: 0,
                submesh: 0,
                colour: None,
                material: 0,
                layer: 0,
                texture_count: 1,
                texture_combo: 0,
                uv_combo: 0,
                weight_combo: 0,
                transform_combo: 0,
            }],
        }],
        faults: Vec::new(),
    }
}

/// What the module would have made: the service on the device, a look of `model` with its
/// texture `red`, held by the scene of a layer.
pub struct Bench {
    pub gpu: egui_wgpu::RenderState,
    pub service: Arc<Service>,
    pub layer: ModelsLayer,
    pub scene: Arc<Mutex<Scene>>,
}

fn bench(model: Model, red: [u8; 4]) -> Option<Bench> {
    bench_on(model, red, true)
}

/// The bench of `bench`, with the pool when `pooled` says, on the path of step 9.4c otherwise.
pub fn bench_on(model: Model, red: [u8; 4], pooled: bool) -> Option<Bench> {
    let gpu = device()?;
    let shared = Arc::new(Shared::new(&gpu, &TARGET, pooled.then_some(pool::SLOTS)));
    assert_eq!(shared.pool.is_some(), pooled, "the device of the tests offers the pool");
    let service = Arc::new(Service::default());
    let _ = service.gpu.set((gpu.device.clone(), gpu.queue.clone()));
    let fake = Fake {
        model: Some(model),
        textures: HashMap::from([("red.blp".to_owned(), plain(red))]),
        ..Fake::default()
    };
    let look = Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let id = service.look(&look);
    let mut refused = Vec::new();
    let ready = crate::load(&shared, &fake, &Caches::default(), &look, &mut refused).unwrap();
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(matches!(ready, Ready::Pooled(_)), pooled);
    let scene = Arc::new(Mutex::new(scene(&gpu.device, HashMap::from([(id, Arc::new(ready))]))));
    let layer = ModelsLayer::new(service.clone(), scene.clone(), Arc::new(Mutex::new(Some(shared))));
    Some(Bench {
        gpu,
        service,
        layer,
        scene,
    })
}

/// The first `size` bytes of `buffer`, copied back from the GPU.
pub fn read_back(gpu: &egui_wgpu::RenderState, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    gpu.queue.submit([encoder.finish()]);
    staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    staging.slice(..).get_mapped_range().expect("mapped").to_vec()
}

/// What the layer draws seen from `eye` towards `look`, 32 × 32 pixels of RGBA cleared to black.
pub fn render(bench: &mut Bench, eye: Vec3, look: Vec3) -> Vec<u8> {
    let gpu = bench.gpu.clone();
    let size = 32u32;
    let view = View {
        view_proj: Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1)
            * Mat4::look_at_rh(eye, look, Vec3::Z),
        view: Mat4::look_at_rh(eye, look, Vec3::Z),
        eye,
        size: [size, size],
        time: 0.0,
        fog: Default::default(),
        sun: Default::default(),
    };
    bench.layer.prepare(&gpu, &view);
    let in_pass = bench.layer.drawing() == Drawing::Pass;
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    bench.layer.compute(&gpu, &view, &mut encoder);
    // A bundle for each phase, run in their order with what the layer draws in the pass.
    let mut bundles = Vec::new();
    for phase in Phase::ALL {
        let mut bundle = gpu
            .device
            .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                label: None,
                color_formats: &[Some(TARGET.color_format)],
                depth_stencil: Some(wgpu::RenderBundleDepthStencil {
                    format: TARGET.depth_format,
                    depth_read_only: false,
                    stencil_read_only: true,
                }),
                sample_count: 1,
                multiview: None,
            });
        if !in_pass {
            bench.layer.draw(&gpu, &TARGET, &view, phase, &mut bundle);
        }
        bundles.push(bundle.finish(&wgpu::RenderBundleDescriptor { label: None }));
    }
    let texture = |format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let colour = texture(
        TARGET.color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = texture(TARGET.depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(256 * size),
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &colour.create_view(&Default::default()),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.create_view(&Default::default()),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for (phase, bundle) in Phase::ALL.into_iter().zip(&bundles) {
            pass.execute_bundles([bundle]);
            if in_pass {
                bench.layer.draw_pass(&gpu, &TARGET, &view, phase, &mut pass);
            }
        }
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &colour,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read_back(&gpu, &pixels, u64::from(256 * size))
}

/// The scene of `looks`, of the generation 1, with the tables the job of the module makes of them.
pub fn scene(device: &wgpu::Device, looks: HashMap<LookId, Arc<Ready>>) -> Scene {
    let looks = Arc::new(looks);
    Scene {
        tables: Some(Arc::new(Tables::new(device, 1, &looks))),
        looks,
        generation: 1,
        reach: 100.0,
        ..Scene::default()
    }
}

/// The image of `render` once what the GPU drew is read back: the frames it takes rendered first.
pub fn settled(bench: &mut Bench, eye: Vec3, look: Vec3) -> Vec<u8> {
    render(bench, eye, look);
    render(bench, eye, look);
    render(bench, eye, look)
}

/// The pixel at the middle of an image of `render`.
pub fn middle(pixels: &[u8]) -> [u8; 4] {
    let at = 16 * 256 + 16 * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

fn red(pixel: [u8; 4]) -> bool {
    pixel[0] > 100 && pixel[1] < 30 && pixel[2] < 30
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
pub const FRONT: Vec3 = Vec3::new(5.0, 0.0, 1.0);
const BEHIND: Vec3 = Vec3::new(-5.0, 0.0, 1.0);
pub const AIM: Vec3 = Vec3::new(0.0, 0.0, 1.0);

#[test]
fn a_model_is_drawn_where_its_instance_stands_lit_and_one_sided() {
    let Some(mut bench) = bench(square(0, 0), [255, 0, 0, 255]) else {
        return;
    };
    assert_eq!(middle(&render(&mut bench, FRONT, AIM)), BLACK, "nothing placed");
    bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0)]);
    let seen = middle(&render(&mut bench, FRONT, AIM));
    assert!(red(seen), "{seen:?}");
    assert!(seen[0] < 255, "lit by the sun and the ambient: {seen:?}");
    assert_eq!(middle(&render(&mut bench, BEHIND, AIM)), BLACK, "its back culled");
    // Moved out of sight: nothing drawn, nor counted.
    bench
        .service
        .place("test", &[instance(1, 0, Vec3::new(0.0, 0.0, 30.0), 1.0)]);
    assert_eq!(middle(&settled(&mut bench, FRONT, AIM)), BLACK);
    let stats = bench.layer.stats();
    assert_eq!((stats.draws, stats.triangles), (0, 0), "out of sight");
}

#[test]
fn a_two_sided_batch_is_drawn_from_behind() {
    let Some(mut bench) = bench(square(0x04, 0), [255, 0, 0, 255]) else {
        return;
    };
    bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0)]);
    assert!(red(middle(&render(&mut bench, BEHIND, AIM))));
}

#[test]
fn an_instance_beyond_the_reach_of_its_size_is_not_drawn() {
    let Some(mut bench) = bench(square(0, 0), [255, 0, 0, 255]) else {
        return;
    };
    bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0)]);
    lock(&bench.scene).reach = 2.0;
    assert_eq!(middle(&render(&mut bench, FRONT, AIM)), BLACK, "5 yards for 3");
    lock(&bench.scene).reach = 4.0;
    assert!(red(middle(&render(&mut bench, FRONT, AIM))), "5 yards for 6");
    // Twice as large, it reaches twice as far.
    lock(&bench.scene).reach = 2.0;
    bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 2.0)]);
    let pixels = render(&mut bench, FRONT, AIM);
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|pixel| red(*pixel)),
        "5 yards for 6"
    );
}

#[test]
fn an_alpha_keyed_texel_under_224_is_not_drawn() {
    for (alpha, drawn) in [(200, false), (230, true)] {
        let Some(mut bench) = bench(square(0, 1), [255, 0, 0, alpha]) else {
            return;
        };
        bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0)]);
        assert_eq!(red(middle(&render(&mut bench, FRONT, AIM))), drawn, "alpha {alpha}");
    }
}

#[test]
fn a_blended_instance_lets_what_is_behind_through_and_one_of_alpha_0_is_not_drawn() {
    let Some(mut bench) = bench(square(0, 2), [255, 0, 0, 255]) else {
        return;
    };
    let mut half = instance(1, 0, Vec3::ZERO, 1.0);
    half.alpha = 0.5;
    bench.service.place("test", &[half]);
    let seen = middle(&render(&mut bench, FRONT, AIM));
    assert!(
        seen[0] > 40 && seen[0] < 220 && seen[1] < 30,
        "half red over black: {seen:?}"
    );
    half.alpha = 0.0;
    bench.service.place("test", &[half]);
    assert_eq!(middle(&render(&mut bench, FRONT, AIM)), BLACK);
}

#[test]
fn moving_instances_are_written_in_place_and_new_groups_into_a_new_buffer_made_filled() {
    let Some(bench) = bench(square(0, 0), [255, 0, 0, 255]) else {
        return;
    };
    let service = &bench.service;
    let stride = size_of::<InstanceGpu>();
    service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0), instance(2, 0, Vec3::X, 1.0)]);
    let slot = service.owners()[0].clone();
    let first = slot.published();
    let buffer = first.buffer.clone().unwrap();
    // Moved within their tile: the same buffer, written in place.
    service.change("test", &[instance(2, 0, Vec3::new(3.0, 0.0, 0.0), 1.0)], &[]);
    let moved = slot.published();
    assert!(Arc::ptr_eq(&buffer, moved.buffer.as_ref().unwrap()));
    assert_eq!(moved.layout, first.layout);
    let bytes = read_back(&bench.gpu, &buffer, buffer.size());
    let second: &[f32] = bytemuck::cast_slice(&bytes[stride..2 * stride]);
    assert_eq!(second[3], 3.0, "the second instance's x, written");
    // In another tile: a new group, in a new buffer made with them.
    service.change("test", &[instance(3, 0, Vec3::new(TILE * 2.0, 0.0, 0.0), 1.0)], &[]);
    let grown = slot.published();
    let new = grown.buffer.clone().unwrap();
    assert!(!Arc::ptr_eq(&buffer, &new));
    assert_eq!((grown.layout, grown.groups.len()), (first.layout + 1, 2));
    let bytes = read_back(&bench.gpu, &new, 3 * stride as u64);
    let third: &[f32] = bytemuck::cast_slice(&bytes[2 * stride..]);
    assert_eq!(third[3], TILE * 2.0, "the third, in the buffer from its making");
}

#[test]
fn an_instance_beyond_its_own_reach_is_dropped_where_its_group_is_drawn() {
    let Some(mut bench) = bench(square(0x04, 0), [255, 0, 0, 255]) else {
        return;
    };
    // Both in one tile: the near one within its reach, the large far one beyond its own.
    bench.service.place(
        "test",
        &[
            instance(1, 0, Vec3::new(6.0, 2.0, 0.0), 1.0),
            instance(2, 0, Vec3::new(100.0, 0.0, -9.0), 10.0),
        ],
    );
    lock(&bench.scene).reach = 4.0;
    let eye = Vec3::new(1.0, 0.0, 1.0);
    let pixels = render(&mut bench, eye, Vec3::new(100.0, 0.0, 1.0));
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|pixel| red(*pixel)),
        "the near one drawn"
    );
    assert_eq!(middle(&pixels), BLACK, "the far one, 99 yards for 60, not");
    lock(&bench.scene).reach = 8.0;
    assert!(
        red(middle(&render(&mut bench, eye, Vec3::new(100.0, 0.0, 1.0)))),
        "99 for 120"
    );
}

#[test]
fn a_look_keeps_the_batches_of_the_submeshes_it_shows_and_seen_at_rest() {
    let Some(gpu) = device() else {
        return;
    };
    let shared = Shared::new(&gpu, &TARGET, None);
    let mut model = square(0, 0);
    let skin = &mut model.skins[0];
    skin.submeshes.push(Submesh {
        id: 102,
        ..skin.submeshes[0]
    });
    skin.submeshes.push(Submesh {
        id: 101,
        ..skin.submeshes[0]
    });
    let batch = skin.batches[0];
    skin.batches.push(Batch { submesh: 1, ..batch });
    skin.batches.push(Batch {
        submesh: 2,
        colour: Some(0),
        ..batch
    });
    // Its third batch unseen at rest.
    model.colours = vec![[1.0, 1.0, 1.0, 0.0]];
    let fake = Fake {
        model: Some(model),
        ..Fake::default()
    };
    let look = |geosets| Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets,
    };
    let batches = |geosets| {
        let mut refused = Vec::new();
        let ready = loading::look(&shared, &fake, &Caches::default(), &look(geosets), &mut refused).unwrap();
        (ready.skins[0].len(), refused.len())
    };
    // The texture red.blp is not given: drawn white, and said.
    assert_eq!(batches(Geosets::All), (2, 1), "the one at alpha 0 left out");
    assert_eq!(batches(Geosets::Creature(0x1)), (1, 1), "102 hidden, 101 unseen");
    assert_eq!(batches(Geosets::Creature(0x2)), (2, 1));
}

#[test]
fn the_alpha_is_tested_as_wotlk_does() {
    // Opaque: the alpha of the texel does not count.
    let cases = [
        (0, 0, true),
        (3, 0, false),
        (3, 255, true),
        (1, 230, true),
        (1, 200, false),
    ];
    for (blending, alpha, drawn) in cases {
        let Some(mut bench) = bench(square(0, blending), [255, 0, 0, alpha]) else {
            return;
        };
        bench.service.place("test", &[instance(1, 0, Vec3::ZERO, 1.0)]);
        let seen = middle(&render(&mut bench, FRONT, AIM));
        assert_eq!(seen[0] > 100, drawn, "blending {blending}, alpha {alpha}: {seen:?}");
    }
}

#[test]
fn what_a_model_keeps_on_the_cpu_counts_its_skins_and_its_animation() {
    let mut model = square(0, 0);
    let [skins, moving] = loading::cpu_bytes(&model);
    assert_eq!(moving, 0, "no animation");
    model.skins.push(model.skins[0].clone());
    let [more, _] = loading::cpu_bytes(&model);
    assert_eq!(
        more - skins,
        (6 * 4 + size_of::<Submesh>() + size_of::<Batch>()) as u64,
        "a second skin"
    );
    // A bone of three keys of translation in one sequence: their times and values, the keys and the bone.
    let mut bone = uniwow_api::formats::Bone {
        key_bone: -1,
        flags: 0,
        parent: None,
        pivot: [0.0; 3],
        translation: Default::default(),
        rotation: Default::default(),
        scale: Default::default(),
    };
    bone.translation.keys.push(uniwow_api::formats::Keys {
        times: vec![0, 1, 2],
        values: vec![[0.0; 3]; 3],
        tangents: Vec::new(),
    });
    model.animation.bones.push(bone);
    let [_, animation] = loading::cpu_bytes(&model);
    let keys = size_of::<uniwow_api::formats::Keys<[f32; 3]>>() + 3 * 4 + 3 * 12;
    assert_eq!(animation, (size_of::<uniwow_api::formats::Bone>() + keys) as u64);
}

#[test]
fn the_bounds_of_an_owner_hold_its_groups_drawn_grown_by_their_largest_radius() {
    let Some(gpu) = device() else {
        return;
    };
    let fake = Fake {
        model: Some(square(0, 0)),
        textures: HashMap::from([("red.blp".to_owned(), plain([255, 0, 0, 255]))]),
        ..Fake::default()
    };
    let shared = Shared::new(&gpu, &TARGET, None);
    let look = Look {
        model: FileRef::Path("square.m2".to_owned()),
        textures: Vec::new(),
        geosets: Geosets::All,
    };
    let ready = Arc::new(crate::load(&shared, &fake, &Caches::default(), &look, &mut Vec::new()).unwrap());
    let radius = ready.radius();
    let group = |look: u32, low: Vec3, high: Vec3, scale: f32| Group {
        look: LookId(look),
        tile: [0, 0],
        first: 0,
        count: 1,
        low,
        high,
        scale,
    };
    // Two groups of a look drawn, one of a look not drawn far away.
    let published = groups::Published {
        groups: vec![
            group(0, Vec3::ZERO, Vec3::X, 1.0),
            group(0, Vec3::new(10.0, 0.0, 0.0), Vec3::new(10.0, 5.0, 0.0), 2.0),
            group(1, Vec3::splat(-100.0), Vec3::splat(-100.0), 9.0),
        ],
        ..groups::Published::default()
    };
    let looks = HashMap::from([(LookId(0), ready)]);
    let (bounds, largest) = layer::owner_bounds(&published, &looks).unwrap();
    assert_eq!(largest, radius * 2.0);
    assert_eq!(
        bounds,
        [Vec3::splat(-largest), Vec3::new(10.0, 5.0, 0.0) + Vec3::splat(largest)]
    );
    let undrawn = groups::Published {
        groups: vec![group(1, Vec3::ZERO, Vec3::ZERO, 1.0)],
        ..groups::Published::default()
    };
    assert_eq!(layer::owner_bounds(&undrawn, &looks), None, "no look of it drawn");
}
