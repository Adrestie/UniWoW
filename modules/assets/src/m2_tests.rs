//! Tests of the models on files the tests write themselves, never on files of the client; when
//! `UNIWOW_CLIENT` names the folder of a client, on its own models and tables; when
//! `UNIWOW_MODERN` names a folder of models exported by wow.export, on those.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use uniwow_api::formats::{self, FileRef, Material, Model, ModelTextureSource};
use uniwow_api::serde_json;

use crate::terrain_tests::client;
use crate::{Client, m2};

/// The reason of a refusal without its path nor its numbers, to count them by kind.
fn kind(reason: &str) -> String {
    let reason = reason.split_once(": ").map_or(reason, |(_, rest)| rest);
    reason
        .split(' ')
        .map(|word| {
            if word.chars().any(|c| c.is_ascii_digit()) {
                "N"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Indexes everything `model` gives, as a reader does without a check: panics if one is missing.
fn index_everything(model: &Model) {
    for skin in &model.skins {
        for submesh in &skin.submeshes {
            let _ = &skin.triangles[submesh.start as usize..(submesh.start + submesh.count) as usize];
        }
        for triangle in &skin.triangles {
            let _ = model.vertices[*triangle as usize];
        }
        for batch in &skin.batches {
            let _ = skin.submeshes[usize::from(batch.submesh)];
            let _ = model.materials[usize::from(batch.material)];
            if let Some(colour) = batch.colour {
                let _ = model.colours[usize::from(colour)];
            }
            let _ = model.weights[usize::from(model.weight_combos[usize::from(batch.weight_combo)])];
            let count = usize::from(batch.texture_count);
            for texture in 0..count {
                let _ = &model.textures[usize::from(model.texture_combos[usize::from(batch.texture_combo) + texture])];
            }
            for texture in 0..count.max(1) {
                let _ = model.transform_combos[usize::from(batch.transform_combo) + texture];
                if !model.uv_combos.is_empty() {
                    let _ = model.uv_combos[usize::from(batch.uv_combo) + texture];
                }
            }
            if model.flags & 0x08 != 0 && batch.shader & 0x8000 == 0 {
                for texture in 0..count {
                    let _ = model.combiner_combos[usize::from(batch.shader) + texture];
                }
            }
        }
    }
}

/// Run on demand, a minute or so: `cargo test -p uniwow-module-assets every_model -- --ignored --nocapture`.
#[test]
#[ignore = "reads every model of the client named by UNIWOW_CLIENT"]
fn every_model_of_the_client_is_read_whole() {
    let Some(client) = client() else { return };
    let models: Vec<String> = client
        .chain
        .files_under("")
        .into_iter()
        .filter(|name| name.to_ascii_lowercase().ends_with(".m2"))
        .collect();
    let next = AtomicUsize::new(0);
    let refused = Mutex::new(BTreeMap::<String, Vec<String>>::new());
    let faults = Mutex::new(BTreeMap::<String, Vec<String>>::new());
    let versions = Mutex::new(BTreeMap::<u32, usize>::new());
    let (read, skins, batches, combined) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let (indirect, levelled, unnamed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                while let Some(name) = models.get(next.fetch_add(1, Ordering::Relaxed)) {
                    match client.model(&FileRef::Path(name.clone())) {
                        Ok(model) => {
                            index_everything(&model);
                            read.fetch_add(1, Ordering::Relaxed);
                            *versions.lock().unwrap().entry(model.version).or_default() += 1;
                            skins.fetch_add(model.skins.len(), Ordering::Relaxed);
                            batches.fetch_add(
                                model.skins.iter().map(|skin| skin.batches.len()).sum(),
                                Ordering::Relaxed,
                            );
                            if !model.combiner_combos.is_empty() {
                                combined.fetch_add(1, Ordering::Relaxed);
                            }
                            for fault in &model.faults {
                                faults
                                    .lock()
                                    .unwrap()
                                    .entry(kind(fault))
                                    .or_default()
                                    .push(format!("{name}: {fault}"));
                            }
                            let drawn_unnamed = model.skins[0].batches.iter().any(|batch| {
                                let combos = usize::from(batch.texture_combo)
                                    ..usize::from(batch.texture_combo + batch.texture_count);
                                model.texture_combos[combos].iter().any(|texture| {
                                    model.textures[usize::from(*texture)].source == ModelTextureSource::Unnamed
                                })
                            });
                            if drawn_unnamed {
                                unnamed.fetch_add(1, Ordering::Relaxed);
                            }
                            // Whether the skin's own list of vertices made a difference.
                            let raw = client
                                .chain
                                .read(&format!("{}00.skin", &name[..name.len() - 3]))
                                .unwrap()
                                .unwrap();
                            let (count, offset) = (
                                u32::from_le_bytes(raw[12..16].try_into().unwrap()) as usize,
                                u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize,
                            );
                            let differs = (0..count).any(|index| {
                                let at = offset + index * 2;
                                u32::from(u16::from_le_bytes([raw[at], raw[at + 1]])) != model.skins[0].triangles[index]
                            });
                            if differs {
                                indirect.fetch_add(1, Ordering::Relaxed);
                            }
                            if model.skins[0].submeshes.iter().any(|s| s.start >= 65536) {
                                levelled.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(reason) => refused.lock().unwrap().entry(kind(&reason)).or_default().push(reason),
                    }
                }
            });
        }
    });
    let refused = refused.into_inner().unwrap();
    let faults = faults.into_inner().unwrap();
    eprintln!(
        "{} models: {} read in {:.1} s, versions {:?}, {} skins, {} batches, {} with combiners, {} whose \
         triangles go through the skin's list of vertices, {} past 65,536 indices, {} drawing a texture named by \
         no file; refused {}: {:#?}; faults {}: {:#?}",
        models.len(),
        read.load(Ordering::Relaxed),
        started.elapsed().as_secs_f64(),
        versions.into_inner().unwrap(),
        skins.load(Ordering::Relaxed),
        batches.load(Ordering::Relaxed),
        combined.load(Ordering::Relaxed),
        indirect.load(Ordering::Relaxed),
        levelled.load(Ordering::Relaxed),
        unnamed.load(Ordering::Relaxed),
        refused.values().map(Vec::len).sum::<usize>(),
        refused
            .iter()
            .map(|(kind, all)| (kind.as_str(), all.len(), &all[..all.len().min(3)]))
            .collect::<Vec<_>>(),
        faults.values().map(Vec::len).sum::<usize>(),
        faults
            .iter()
            .map(|(kind, all)| (kind.as_str(), all.len(), &all[..all.len().min(3)]))
            .collect::<Vec<_>>()
    );
    // Refused only for a first skin the client lacks or that is not of its model.
    assert!(
        refused
            .values()
            .flatten()
            .all(|reason| reason.contains(": its skin 0: "))
    );
}

/// The `.m2` under `folder` and its folders.
fn models_under(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            models_under(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("m2"))
        {
            found.push(path);
        }
    }
}

/// The files wow.export wrote beside `model`, by FileDataID, from its manifest.
fn manifest(model: &Path) -> HashMap<u32, String> {
    let path = model.with_extension("manifest.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    ["textures", "skins", "lodSkins"]
        .iter()
        .flat_map(|list| json[list].as_array().cloned().unwrap_or_default())
        .filter_map(|entry| Some((entry["fileDataID"].as_u64()? as u32, entry["file"].as_str()?.to_owned())))
        .collect()
}

#[test]
fn the_modern_models_exported_by_wow_export_are_read_whole() {
    let Ok(folder) = std::env::var("UNIWOW_MODERN") else {
        eprintln!("skipped: UNIWOW_MODERN names no folder of models exported by wow.export");
        return;
    };
    let mut models = Vec::new();
    models_under(Path::new(&folder), &mut models);
    assert!(!models.is_empty(), "no model under {folder}");
    let started = Instant::now();
    for path in &models {
        let named = manifest(path);
        let beside = |file: &str| path.with_file_name(file);
        let bytes = std::fs::read(path).unwrap();
        let model = m2::read(&bytes, &path.to_string_lossy(), |skin| {
            // By the FileDataID the model gives, as wow.export named the file.
            let file = match skin.id {
                Some(id) => beside(
                    named
                        .get(&id)
                        .ok_or_else(|| format!("its skin {id} not in the manifest"))?,
                ),
                None => PathBuf::from(skin.path),
            };
            std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))
        })
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(model.faults.is_empty(), "{}: {:?}", path.display(), model.faults);
        index_everything(&model);
        // A creature keeps the keys of its Stand, held in the model as in 3.3.5a.
        if path.to_string_lossy().to_ascii_lowercase().contains("creature") {
            let stand = model.animation.sequences.iter().find(|sequence| sequence.id == 0);
            assert!(stand.is_some_and(|stand| stand.kept), "{}", path.display());
        }
        assert!(
            model.version > 264 && !model.skins.is_empty(),
            "{}: version {}, {} skins",
            path.display(),
            model.version,
            model.skins.len()
        );
        let skins = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let stem = path.file_stem().unwrap().to_string_lossy().to_ascii_lowercase();
                name.ends_with(".skin")
                    && (name == format!("{stem}00.skin") || name.starts_with(&format!("{stem}_lod")))
            })
            .count();
        assert_eq!(model.skins.len(), skins, "{}: its skins", path.display());
        // Its textures by FileDataID, those wow.export wrote.
        let ids: Vec<u32> = model
            .textures
            .iter()
            .filter_map(|texture| match texture.source {
                ModelTextureSource::File(FileRef::Id(id)) => Some(id),
                _ => None,
            })
            .collect();
        if !named.is_empty() {
            assert!(
                ids.iter().all(|id| named.contains_key(id)),
                "{}: {ids:?}",
                path.display()
            );
        }
        eprintln!(
            "{}: version {}, {} vertices, {} skins, {} batches, {} textures ({} by FileDataID, {} filled)",
            path.strip_prefix(&folder).unwrap_or(path).display(),
            model.version,
            model.vertices.len(),
            model.skins.len(),
            model.skins[0].batches.len(),
            model.textures.len(),
            ids.len(),
            model
                .textures
                .iter()
                .filter(|texture| matches!(texture.source, ModelTextureSource::Filled(_)))
                .count()
        );
    }
    eprintln!(
        "{} modern models read in {:.0} ms",
        models.len(),
        started.elapsed().as_secs_f64() * 1e3
    );
}

/// A file written by a test: a header of `header` bytes, then the arrays it points to.
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new(magic: &[u8; 4], header: usize) -> Self {
        let mut bytes = vec![0; header];
        bytes[..4].copy_from_slice(magic);
        Self { bytes }
    }

    fn u32(&mut self, at: usize, value: u32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn f32s(&mut self, at: usize, values: &[f32]) {
        for (index, value) in values.iter().enumerate() {
            self.u32(at + index * 4, value.to_bits());
        }
    }

    /// Appends `data`, of `count` items, and points the array at `at` to it.
    fn array(&mut self, at: usize, count: usize, data: &[u8]) {
        let offset = self.append(data);
        self.u32(at, count as u32);
        self.u32(at + 4, offset as u32);
    }

    fn append(&mut self, data: &[u8]) -> usize {
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(data);
        offset
    }

    /// Appends the keys of a track at rest, of its global sequence `global`: its one key `value`
    /// of its first sequence, at its start, unless none; the track.
    fn track(&mut self, global: u16, value: Option<&[u8]>) -> Vec<u8> {
        let mut track = vec![0; 20];
        track[2..4].copy_from_slice(&global.to_le_bytes());
        if let Some(value) = value {
            let time = self.append(&0u32.to_le_bytes()) as u32;
            let times = self.append(&[1u32.to_le_bytes(), time.to_le_bytes()].concat()) as u32;
            let key = self.append(value) as u32;
            let first = self.append(&[1u32.to_le_bytes(), key.to_le_bytes()].concat()) as u32;
            track[4..8].copy_from_slice(&1u32.to_le_bytes());
            track[8..12].copy_from_slice(&times.to_le_bytes());
            track[12..16].copy_from_slice(&1u32.to_le_bytes());
            track[16..20].copy_from_slice(&first.to_le_bytes());
        }
        track
    }
}

fn u16_bytes(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A model of `version` and `flags`: two views, three vertices, three textures (a file, a skin of
/// its creature, one named by no file), two materials, a colour and two weights, its first
/// sequence holding its keys when `held`.
fn written_model(version: u32, flags: u32, held: bool) -> Vec<u8> {
    let mut w = Writer::new(b"MD20", 0x138);
    w.u32(4, version);
    w.u32(0x10, flags);
    let mut sequence = vec![0; 64];
    sequence[12] = if held { 0x20 } else { 0 };
    w.array(0x1C, 1, &sequence);
    let vertices: Vec<u8> = (0..3u8)
        .flat_map(|v| {
            let f = f32::from(v);
            [
                f32_bytes(&[f, f + 0.5, -f]),
                vec![255, 0, 0, 0, v, 0, 0, 0],
                f32_bytes(&[0.0, 0.0, 1.0, f / 4.0, 1.0 - f / 4.0, 0.5, 0.5]),
            ]
            .concat()
        })
        .collect();
    w.array(0x3C, 3, &vertices);
    w.u32(0x44, 2);
    let colour = [
        w.track(0xFFFF, Some(&f32_bytes(&[0.5, 0.25, 1.0]))),
        w.track(0xFFFF, Some(&16384i16.to_le_bytes())),
    ]
    .concat();
    w.array(0x48, 1, &colour);
    let name = w.append(b"a.blp\0") as u32;
    let textures: Vec<u8> = [[0, 1, 6, name], [11, 0, 0, 0], [0, 0, 1, name + 5]]
        .iter()
        .flat_map(|entry| entry.iter().flat_map(|v: &u32| v.to_le_bytes()))
        .collect();
    w.array(0x50, 3, &textures);
    let weights = [w.track(0, Some(&16384i16.to_le_bytes())), w.track(0xFFFF, None)].concat();
    w.array(0x58, 2, &weights);
    w.array(0x70, 2, &u16_bytes(&[0x04, 1, 0x10, 4]));
    w.array(0x80, 3, &u16_bytes(&[0, 1, 2]));
    w.array(0x88, 2, &u16_bytes(&[0, 1]));
    w.array(0x90, 2, &u16_bytes(&[0, 1]));
    w.array(0x98, 2, &u16_bytes(&[0xFFFF, 0xFFFF]));
    w.f32s(0xA0, &[-1.0, -2.0, -3.0, 1.0, 2.0, 3.0, 4.0]);
    if flags & 0x08 != 0 {
        w.array(0x130, 2, &u16_bytes(&[5, 6]));
    }
    w.bytes
}

/// A batch: its submesh, colour, material, texture count and combo, combo of coordinates, weight
/// combo, transform combo and shader.
type Written = [u16; 9];

/// A batch drawing the submesh 0 with what the written model has.
const GOOD: Written = [0, 0xFFFF, 1, 2, 1, 0, 1, 0, 0];

/// A skin: its list of vertices `lookup`, its `indices`, its submeshes (id, level, start, count)
/// and its batches.
fn written_skin(lookup: &[u16], indices: &[u16], submeshes: &[[u16; 4]], batches: &[Written]) -> Vec<u8> {
    let mut w = Writer::new(b"SKIN", 48);
    w.array(4, lookup.len(), &u16_bytes(lookup));
    w.array(12, indices.len(), &u16_bytes(indices));
    let sections: Vec<u8> = submeshes
        .iter()
        .flat_map(|[id, level, start, count]| {
            [
                u16_bytes(&[*id, *level, 0, 3, *start, *count, 0, 0, 0, 0]),
                f32_bytes(&[0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 9.0]),
            ]
            .concat()
        })
        .collect();
    w.array(28, submeshes.len(), &sections);
    let units: Vec<u8> = batches
        .iter()
        .flat_map(
            |[submesh, colour, material, count, combo, uv, weight, transform, shader]| {
                u16_bytes(&[
                    0x10, *shader, *submesh, 0, *colour, *material, 0, *count, *combo, *uv, *weight, *transform,
                ])
            },
        )
        .collect();
    w.array(36, batches.len(), &units);
    w.bytes
}

#[test]
fn a_model_of_3_3_5a_is_read_at_rest() {
    let parsed = m2::model(&written_model(264, 0, true)).unwrap();
    assert_eq!((parsed.views, parsed.skin_ids.len()), (2, 0));
    let model = parsed.model;
    assert_eq!(model.version, 264);
    assert_eq!(model.vertices.len(), 3);
    let vertex = model.vertices[2];
    assert_eq!(vertex.position, [2.0, 2.5, -2.0]);
    assert_eq!(
        (vertex.bone_weights, vertex.bone_indices),
        ([255, 0, 0, 0], [2, 0, 0, 0])
    );
    assert_eq!((vertex.normal, vertex.uv), ([0.0, 0.0, 1.0], [[0.5, 0.5], [0.5, 0.5]]));
    let sources: Vec<_> = model.textures.iter().map(|t| t.source.clone()).collect();
    assert_eq!(
        sources,
        [
            ModelTextureSource::File(FileRef::Path("a.blp".to_owned())),
            ModelTextureSource::Filled(11),
            ModelTextureSource::Unnamed,
        ]
    );
    assert_eq!(model.textures[0].flags, 1);
    let material = |flags, blending| Material { flags, blending };
    assert_eq!(model.materials, [material(0x04, 1), material(0x10, 4)]);
    assert_eq!(
        (
            model.texture_combos,
            model.uv_combos,
            model.weight_combos,
            model.transform_combos
        ),
        (vec![0, 1, 2], vec![0, 1], vec![0, 1], vec![0xFFFF, 0xFFFF])
    );
    let [r, g, b, a] = model.colours[0];
    assert_eq!([r, g, b], [0.5, 0.25, 1.0]);
    assert!((a - 0.5).abs() < 1e-4 && (model.weights[0] - 0.5).abs() < 1e-4 && model.weights[1] == 1.0);
    assert_eq!(
        (model.bounds, model.radius),
        ([[-1.0, -2.0, -3.0], [1.0, 2.0, 3.0]], 4.0)
    );
    assert!(model.combiner_combos.is_empty());
    let combined = m2::model(&written_model(264, 0x08, true)).unwrap().model;
    assert_eq!(combined.combiner_combos, [5, 6]);
}

#[test]
fn the_keys_of_a_first_sequence_kept_in_an_anim_file_are_not_read() {
    let model = m2::model(&written_model(264, 0, false)).unwrap().model;
    assert_eq!(model.colours, [[1.0; 4]]);
    // Those of a global sequence are.
    assert!((model.weights[0] - 0.5).abs() < 1e-4);
}

#[test]
fn the_triangles_of_a_skin_go_through_its_list_of_vertices() {
    let model = m2::model(&written_model(264, 0, true)).unwrap().model;
    let skin = written_skin(&[2, 0, 1], &[0, 1, 2, 2, 1, 0], &[[0, 0, 0, 6]], &[GOOD]);
    let read = m2::skin(&skin, &model, &mut Vec::new()).unwrap();
    assert_eq!(read.triangles, [2, 0, 1, 1, 0, 2]);
    assert_eq!(read.batches.len(), 1);
    assert_eq!(read.batches[0].colour, None);
    assert_eq!(
        (read.submeshes[0].centre, read.submeshes[0].radius),
        ([1.0, 2.0, 3.0], 9.0)
    );
}

#[test]
fn a_submesh_starts_at_its_level_times_65536() {
    let model = m2::model(&written_model(264, 0, true)).unwrap().model;
    let indices: Vec<u16> = (0..65_536 + 8).map(|i| (i % 3) as u16).collect();
    let skin = written_skin(&[0, 1, 2], &indices, &[[0, 0, 0, 3], [1, 1, 3, 3]], &[]);
    let read = m2::skin(&skin, &model, &mut Vec::new()).unwrap();
    assert_eq!((read.submeshes[1].start, read.submeshes[1].count), (65_539, 3));
    let past = written_skin(&[0, 1, 2], &indices, &[[1, 1, 6, 3]], &[]);
    let refused = m2::skin(&past, &model, &mut Vec::new()).unwrap_err();
    assert!(refused.contains("past its"), "{refused}");
}

#[test]
fn a_batch_referring_to_what_its_model_lacks_is_left_out_and_said() {
    let model = m2::model(&written_model(264, 0, true)).unwrap().model;
    let mut batches = vec![GOOD];
    for (field, value) in [(0, 1), (1, 1), (2, 2), (4, 2), (5, 2), (6, 2), (7, 1)] {
        let mut batch = GOOD;
        batch[field] = value;
        batches.push(batch);
    }
    let skin = written_skin(&[0, 1, 2], &[0, 1, 2], &[[0, 0, 0, 3]], &batches);
    let mut faults = Vec::new();
    let read = m2::skin(&skin, &model, &mut faults).unwrap();
    assert_eq!(read.batches.len(), 1);
    assert_eq!(
        faults,
        [
            "its batch 1 left out, to its submesh 1 of 1",
            "its batch 2 left out, to its colour 1 of 1",
            "its batch 3 left out, to its material 2 of 2",
            "its batch 4 left out, to its texture combos 2 and on, 2 of 3",
            "its batch 5 left out, to its combo of coordinates 2 of 2",
            "its batch 6 left out, to its weight combo 2 of 2",
            "its batch 7 left out, to its transform combos 1 and on, 2 of 2",
        ]
    );
}

#[test]
fn with_combiners_a_shader_not_yet_chosen_is_where_the_combiners_of_its_textures_are() {
    let model = m2::model(&written_model(264, 0x08, true)).unwrap().model;
    let at = |shader| [0, 0xFFFF, 1, 2, 1, 0, 1, 0, shader];
    let skin = written_skin(&[0, 1, 2], &[0, 1, 2], &[[0, 0, 0, 3]], &[at(0), at(1), at(0x8005)]);
    let mut faults = Vec::new();
    let read = m2::skin(&skin, &model, &mut faults).unwrap();
    let shaders: Vec<u16> = read.batches.iter().map(|batch| batch.shader).collect();
    assert_eq!(shaders, [0, 0x8005], "a shader already chosen is no place");
    assert_eq!(
        faults,
        ["its batch 1 left out, to its combiner combos 1 and on, 2 of 2"]
    );
}

#[test]
fn the_coordinates_past_the_combos_are_the_first_and_any_value_is_kept() {
    let mut bytes = written_model(264, 0, true);
    // Its combos of coordinates 0 and 2: 2 is taken as the first set by the client, and kept.
    let at = u32::from_le_bytes(bytes[0x8C..0x90].try_into().unwrap()) as usize;
    bytes[at + 2..at + 4].copy_from_slice(&2u16.to_le_bytes());
    // Two textures from the second combo, the last.
    let skin = written_skin(
        &[0, 1, 2],
        &[0, 1, 2],
        &[[0, 0, 0, 3]],
        &[[0, 0xFFFF, 1, 2, 1, 1, 1, 0, 0]],
    );
    let model = m2::read(&bytes, "World\\x.m2", |_| Ok(skin.clone())).unwrap();
    assert_eq!((model.skins[0].batches.len(), model.skins[1].batches.len()), (1, 1));
    assert_eq!(model.uv_combos, [0, 2, 0]);
    assert_eq!(
        model.faults,
        ["its skin 0: the coordinates of the next textures of 1 of its batches, past its combos, taken as the first"]
    );
}

#[test]
fn a_model_without_its_first_skin_is_refused_and_a_later_one_left_out_with_the_next() {
    let bytes = written_model(264, 0, true);
    let skin = written_skin(&[0, 1, 2], &[0, 1, 2], &[[0, 0, 0, 3]], &[GOOD]);
    let model = m2::read(&bytes, "World\\x.m2", |wanted| match wanted.path.as_str() {
        "World\\x00.skin" => Ok(skin.clone()),
        _ => Err("not in the client".to_owned()),
    })
    .unwrap();
    assert_eq!(model.skins.len(), 1);
    assert_eq!(
        model.faults,
        ["its skin 1 and the next ones left out: not in the client"]
    );
    let refused = m2::read(&bytes, "World\\x.m2", |_| Err("not in the client".to_owned())).unwrap_err();
    assert_eq!(refused, "its skin 0: not in the client");
}

#[test]
fn a_modern_model_names_its_skins_and_textures_by_file_data_id() {
    let mut inner = written_model(274, 0, true);
    // Without combos of coordinates, as since Cataclysm.
    inner[0x88..0x90].fill(0);
    let chunk = |name: &[u8; 4], data: &[u8]| [&name[..], &(data.len() as u32).to_le_bytes(), data].concat();
    let ids = |values: &[u32]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
    let bytes = [
        chunk(b"MD21", &inner),
        chunk(b"SFID", &ids(&[100, 0, 300])),
        chunk(b"TXID", &ids(&[7, 0, 9])),
    ]
    .concat();
    let skin = written_skin(
        &[0, 1, 2],
        &[0, 1, 2],
        &[[0, 0, 0, 3]],
        &[[0, 0xFFFF, 1, 2, 1, 5, 1, 0, 0]],
    );
    let asked = Mutex::new(Vec::new());
    let model = m2::read(&bytes, "spells\\x.m2", |wanted| {
        asked.lock().unwrap().push((wanted.id, wanted.path));
        Ok(skin.clone())
    })
    .unwrap();
    assert_eq!(
        asked.into_inner().unwrap(),
        [
            (Some(100), "spells\\x00.skin".to_owned()),
            (None, "spells\\x01.skin".to_owned()),
            (Some(300), "spells\\x_lod01.skin".to_owned()),
        ]
    );
    assert!(model.faults.is_empty(), "{:?}", model.faults);
    assert_eq!((model.version, model.skins.len()), (274, 3));
    let sources: Vec<_> = model.textures.iter().map(|t| t.source.clone()).collect();
    assert_eq!(
        sources,
        [
            ModelTextureSource::File(FileRef::Id(7)),
            ModelTextureSource::Filled(11),
            ModelTextureSource::File(FileRef::Id(9)),
        ]
    );
}

#[test]
fn what_is_not_a_model_or_a_skin_of_it_is_refused() {
    let refused = |bytes: &[u8]| m2::model(bytes).err().unwrap();
    assert_eq!(refused(b"MDX0"), "not an M2");
    assert_eq!(
        refused(&written_model(263, 0, true)),
        "version 263, where 264 to 274 are read"
    );
    assert_eq!(
        refused(&written_model(275, 0, true)),
        "version 275, where 264 to 274 are read"
    );
    assert_eq!(refused(b"MD21"), "its chunk MD21 missing");
    let mut cut = written_model(264, 0, true);
    cut[0x3C..0x40].copy_from_slice(&1000u32.to_le_bytes());
    assert!(refused(&cut).starts_with("its vertices, 1000 at byte"));
    let model = m2::model(&written_model(264, 0, true)).unwrap().model;
    let skin = |bytes: &[u8]| m2::skin(bytes, &model, &mut Vec::new()).err().unwrap();
    assert_eq!(skin(b"MD20"), "not a skin");
    assert_eq!(
        skin(&written_skin(&[0, 1], &[0, 1, 0, 1], &[], &[])),
        "4 indices, not whole triangles"
    );
    assert_eq!(
        skin(&written_skin(&[0, 1], &[0, 1, 2], &[], &[])),
        "a triangle to its vertex 2 of 2"
    );
    assert_eq!(
        skin(&written_skin(&[0, 1, 3], &[0, 1, 2], &[], &[])),
        "its vertex 3 of 3 of the model"
    );
    let far = written_skin(&[0, 1, 2], &[0, 1, 2], &[[0, 0xFFFF, 0xFFFF, 0xFFFF]], &[]);
    assert_eq!(skin(&far), "its submesh 0 past its 3 indices");
}

#[test]
fn a_model_or_a_skin_cut_anywhere_is_refused_and_never_panics() {
    let bytes = written_model(264, 0x08, true);
    for length in 0..bytes.len() {
        let _ = m2::model(&bytes[..length]);
    }
    let model = m2::model(&bytes).unwrap().model;
    let skin = written_skin(&[0, 1, 2], &[0, 1, 2], &[[0, 0, 0, 3]], &[GOOD]);
    for length in 0..skin.len() {
        assert!(
            m2::skin(&skin[..length], &model, &mut Vec::new()).is_err(),
            "cut at {length}"
        );
    }
}

#[test]
fn a_path_of_a_table_is_read_as_an_m2() {
    assert_eq!(m2::path("Creature\\Wolf\\Wolf.mdx"), "Creature\\Wolf\\Wolf.m2");
    assert_eq!(m2::path("World\\x.MDL"), "World\\x.m2");
    assert_eq!(m2::path("World\\x.m2"), "World\\x.m2");
}

/// The ids of the submeshes of the first skin of `path` in `client`, once each.
fn submesh_ids(client: &Client, path: &str) -> Vec<u16> {
    let model = client.model(&FileRef::Path(path.to_owned())).unwrap();
    let mut ids: Vec<u16> = model.skins[0].submeshes.iter().map(|submesh| submesh.id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Whether `drawn` shows at most one variant of each group from 100 to 2,999 of `ids`, sorted.
fn one_a_group(ids: &[u16], drawn: &[bool]) -> bool {
    let mut groups: Vec<u16> = ids
        .iter()
        .zip(drawn)
        .filter(|(id, drawn)| **drawn && (100..3000).contains(*id))
        .map(|(id, _)| id / 100)
        .collect();
    let all = groups.len();
    groups.dedup();
    groups.len() == all
}

#[test]
fn the_iron_dwarves_show_the_variants_their_displays_choose() {
    let Some(client) = client() else { return };
    let displays = client.tables.creature_displays(&client.chain).unwrap();
    let models = client.tables.creature_models(&client.chain).unwrap();
    let iron: Vec<_> = displays
        .iter()
        .filter(|display| {
            models.iter().any(|model| {
                model.id == display.model && model.path.to_ascii_lowercase().ends_with("irondwarf\\irondwarf.mdx")
            })
        })
        .collect();
    let ids = submesh_ids(&client, "Creature\\IronDwarf\\IronDwarf.m2");
    let mut looks = std::collections::BTreeSet::new();
    for display in iron.iter().filter(|display| display.geosets != 0) {
        let drawn = formats::creature_geosets(&ids, display.geosets);
        let chosen: Vec<u16> = (0..8)
            .map(|nibble| ((nibble + 1) * 100 + ((display.geosets >> (4 * nibble)) & 0xF)) as u16)
            .collect();
        for (id, drawn) in ids.iter().zip(&drawn) {
            let expected = *id == 0 || *id >= 900 || chosen.contains(id);
            assert_eq!(*drawn, expected, "display {}: submesh {id}", display.id);
        }
        assert!(one_a_group(&ids, &drawn), "display {}", display.id);
        looks.insert(drawn);
    }
    eprintln!(
        "{} displays of IronDwarf, {} with variants, {} distinct looks, of the submeshes {ids:?}",
        iron.len(),
        iron.iter().filter(|display| display.geosets != 0).count(),
        looks.len()
    );
    assert!(looks.len() >= 2);
}

#[test]
fn marshal_dughan_shows_his_hair_and_facial_hair() {
    let Some(client) = client() else { return };
    let displays = client.tables.creature_displays(&client.chain).unwrap();
    let looks = client.tables.creature_looks(&client.chain).unwrap();
    // Marshal Dughan, of Goldshire: his display in creature_template_model of AzerothCore.
    let display = displays.iter().find(|display| display.id == 1985).unwrap();
    let look = looks.iter().find(|look| look.id == display.extra).unwrap();
    assert_eq!((look.race, look.sex), (1, 0), "a human man");
    let hairs = client.tables.hair_geosets(&client.chain).unwrap();
    let facials = client.tables.facial_hairs(&client.chain).unwrap();
    let hair = hairs
        .iter()
        .find(|hair| (hair.race, hair.sex, hair.variation) == (1, 0, look.hair_style));
    let facial = facials
        .iter()
        .find(|facial| (facial.race, facial.sex, facial.variation) == (1, 0, look.facial_hair));
    let ids = submesh_ids(&client, "Character\\Human\\Male\\HumanMale.m2");
    let drawn = formats::look_geosets(&ids, hair, facial);
    let shown: Vec<u16> = ids
        .iter()
        .zip(&drawn)
        .filter(|(_, drawn)| **drawn)
        .map(|(id, _)| *id)
        .collect();
    eprintln!(
        "display 1985, look {}: hair style {} ({hair:?}), facial hair {} ({facial:?}), shows {shown:?}",
        look.id, look.hair_style, look.facial_hair
    );
    let hair = hair.unwrap().geoset.max(1) as u16;
    assert!(shown.contains(&hair) && shown.iter().filter(|id| **id < 100).count() == 2);
    assert!([0, 401, 501, 702, 1301].iter().all(|id| shown.contains(id)));
    assert!(one_a_group(&ids, &drawn));
}

// What moves.

impl Writer {
    /// Appends a track of `interpolation` on the global sequence `global` (0xFFFF none), in each
    /// sequence its times, the bytes of its values and how many values they are; the track.
    fn keyed(&mut self, interpolation: u16, global: u16, lists: &[(Vec<u32>, Vec<u8>, u32)]) -> Vec<u8> {
        let mut times = Vec::new();
        let mut values = Vec::new();
        for (at, bytes, count) in lists {
            let written: Vec<u8> = at.iter().flat_map(|time| time.to_le_bytes()).collect();
            let time_offset = self.append(&written) as u32;
            let value_offset = self.append(bytes) as u32;
            times.extend([at.len() as u32, time_offset]);
            values.extend([*count, value_offset]);
        }
        let as_bytes = |words: &[u32]| -> Vec<u8> { words.iter().flat_map(|word| word.to_le_bytes()).collect() };
        let time_lists = self.append(&as_bytes(&times)) as u32;
        let value_lists = self.append(&as_bytes(&values)) as u32;
        [
            u16_bytes(&[interpolation, global]),
            as_bytes(&[lists.len() as u32, time_lists, lists.len() as u32, value_lists]),
        ]
        .concat()
    }
}

/// A sequence of `id` and `variation`, of `flags`, an alias of `alias`.
fn written_sequence(id: u16, variation: u16, flags: u32, alias: u16) -> Vec<u8> {
    [
        u16_bytes(&[id, variation]),
        1000u32.to_le_bytes().to_vec(),
        2.5f32.to_le_bytes().to_vec(),
        flags.to_le_bytes().to_vec(),
        u16_bytes(&[32767, 0]),
        [0u32, 0, 150].iter().flat_map(|word| word.to_le_bytes()).collect(),
        f32_bytes(&[-1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 2.0]),
        u16_bytes(&[0xFFFF, alias]),
    ]
    .concat()
}

/// A bone of `parent` and its three tracks.
fn written_bone(parent: i16, tracks: [Vec<u8>; 3]) -> Vec<u8> {
    [
        (-1i32).to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
        parent.to_le_bytes().to_vec(),
        0u16.to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
        tracks.concat(),
        f32_bytes(&[0.5, 0.0, 1.0]),
    ]
    .concat()
}

/// The model of `written_model` with five sequences (an emote held; *Stand* held; *Walk* an alias
/// of the fifth; *Run* in an `.anim` file; another emote held), a global sequence, three bones
/// given in an order where a child comes before its parent, and a texture transform.
fn animated_model() -> Vec<u8> {
    let mut w = Writer {
        bytes: written_model(264, 0, true),
    };
    let sequences = [
        written_sequence(60, 0, 0x20, 0),
        written_sequence(0, 0, 0x20, 0),
        written_sequence(4, 0, 0x40, 4),
        written_sequence(5, 0, 0, 0),
        written_sequence(61, 0, 0x20, 0),
    ]
    .concat();
    w.array(0x1C, 5, &sequences);
    w.array(0x14, 1, &1000u32.to_le_bytes());
    let none = |w: &mut Writer| w.keyed(1, 0xFFFF, &[]);
    let empty = || (Vec::new(), Vec::new(), 0);
    let translation = w.keyed(
        1,
        0xFFFF,
        &[
            (vec![0], f32_bytes(&[9.0, 9.0, 9.0]), 1),
            (vec![0, 500], f32_bytes(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 2),
            empty(),
            empty(),
            (vec![0], f32_bytes(&[7.0, 8.0, 9.0]), 1),
        ],
    );
    let (rotation, scale) = (none(&mut w), none(&mut w));
    let child = written_bone(2, [translation, rotation, scale]);
    // The identity, as stored, in Stand.
    let identity: Vec<u8> = [32767i16, 32767, 32767, -1]
        .iter()
        .flat_map(|part| part.to_le_bytes())
        .collect();
    let (translation, scale) = (none(&mut w), none(&mut w));
    let rotation = w.keyed(1, 0xFFFF, &[empty(), (vec![0], identity, 1)]);
    let root = written_bone(-1, [translation, rotation, scale]);
    // A Bezier scale on the global sequence: each key its value, then its tangents in and out.
    let curve = f32_bytes(&[
        1.0, 1.0, 1.0, 0.5, 0.5, 0.5, 2.0, 2.0, 2.0, 4.0, 4.0, 4.0, 3.0, 3.0, 3.0, 5.0, 5.0, 5.0,
    ]);
    let (translation, rotation) = (none(&mut w), none(&mut w));
    let scale = w.keyed(2, 0, &[(vec![0, 1000], curve, 2)]);
    let middle = written_bone(1, [translation, rotation, scale]);
    w.array(0x2C, 3, &[child, root, middle].concat());
    let transform = [
        w.keyed(
            1,
            0xFFFF,
            &[
                empty(),
                empty(),
                empty(),
                empty(),
                (vec![0, 2000], f32_bytes(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]), 2),
            ],
        ),
        w.keyed(1, 0xFFFF, &[]),
        w.keyed(1, 0xFFFF, &[]),
    ]
    .concat();
    w.array(0x60, 1, &transform);
    w.bytes
}

#[test]
fn the_sequences_and_bones_of_a_model_are_read_their_keys_kept_for_those_played() {
    let model = m2::model(&animated_model()).unwrap().model;
    assert!(model.faults.is_empty(), "{:?}", model.faults);
    let animation = &model.animation;
    let kept: Vec<(u16, bool)> = animation
        .sequences
        .iter()
        .map(|sequence| (sequence.id, sequence.kept))
        .collect();
    assert_eq!(
        kept,
        [(60, false), (0, true), (4, false), (5, false), (61, true)],
        "Stand, and what Walk is an alias of; not the first emote, nor Run in its .anim file"
    );
    let walk = &animation.sequences[2];
    assert_eq!(
        (walk.alias, walk.duration, walk.speed, walk.blend),
        (Some(4), 1000, 2.5, [150, 150])
    );
    assert_eq!(
        (walk.bounds, walk.radius, walk.next),
        ([[-1.0; 3], [1.0; 3]], 2.0, None)
    );
    assert_eq!(animation.globals, [1000]);
    // The first bone, a child of the third whose parent is the second: computed after both.
    let parents: Vec<Option<u16>> = animation.bones.iter().map(|bone| bone.parent).collect();
    assert_eq!(parents, [Some(2), None, Some(1)]);
    assert_eq!(animation.order, [1, 2, 0]);
    let child = &animation.bones[0];
    assert_eq!(child.pivot, [0.5, 0.0, 1.0]);
    let keys = &child.translation.keys;
    assert_eq!(
        (keys[1].times.clone(), keys[1].values.clone()),
        (vec![0, 500], vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])
    );
    assert!(
        keys[0].times.is_empty() && keys[2].times.is_empty() && keys[3].times.is_empty(),
        "not kept"
    );
    assert_eq!(keys[4].values, [[7.0, 8.0, 9.0]]);
    let rotation = animation.bones[1].rotation.keys[1].values[0];
    assert!(
        rotation
            .iter()
            .zip([0.0, 0.0, 0.0, 1.0])
            .all(|(read, wanted)| (read - wanted).abs() < 1e-4),
        "{rotation:?}"
    );
    // On the global sequence, read whatever the sequence of its list.
    let scale = &animation.bones[2].scale;
    assert_eq!(
        (scale.interpolation, scale.global),
        (formats::Interpolation::Bezier, Some(0))
    );
    assert_eq!(scale.keys[0].values, [[1.0; 3], [4.0; 3]]);
    assert_eq!(scale.keys[0].tangents, [[[0.5; 3], [2.0; 3]], [[3.0; 3], [5.0; 3]]]);
    // The colour of `written_model` in the first emote, not kept; its weight on a global sequence.
    assert!(animation.colours[0].0.keys[0].times.is_empty());
    assert_eq!(animation.weights[0].global, Some(0));
    assert!((animation.weights[0].keys[0].values[0] - 0.5).abs() < 1e-4);
    assert!(animation.weights[1].keys.is_empty());
    assert_eq!(
        animation.transforms[0].translation.keys[4].values,
        [[0.0; 3], [1.0, 0.0, 0.0]]
    );
}

#[test]
fn a_track_or_a_bone_that_does_not_hold_together_is_left_at_rest_and_said() {
    let mut w = Writer {
        bytes: written_model(264, 0, true),
    };
    w.array(0x1C, 1, &written_sequence(0, 0, 0x20, 0));
    let none = |w: &mut Writer| w.keyed(1, 0xFFFF, &[]);
    // Two times for one value; a parent past the bones; two bones each the parent of the other.
    let short = w.keyed(1, 0xFFFF, &[(vec![0, 500], f32_bytes(&[1.0, 2.0, 3.0]), 1)]);
    let (rotation, scale) = (none(&mut w), none(&mut w));
    let first = written_bone(9, [short, rotation, scale]);
    let tracks = |w: &mut Writer| [none(w), none(w), none(w)];
    let second = written_bone(2, tracks(&mut w));
    let third = written_bone(1, tracks(&mut w));
    w.array(0x2C, 3, &[first, second, third].concat());
    let model = m2::model(&w.bytes).unwrap().model;
    let animation = &model.animation;
    assert!(animation.bones[0].translation.keys[0].times.is_empty(), "left at rest");
    let parents: Vec<Option<u16>> = animation.bones.iter().map(|bone| bone.parent).collect();
    assert_eq!(parents, [None, Some(2), None], "past the bones; the loop cut");
    assert_eq!(animation.order, [0, 2, 1]);
    assert_eq!(
        model.faults,
        [
            "its animation: its bone 0: its parent 9 of 3 bones left out",
            "its animation: a translation: 2 times and 1 values in its sequence 0, left at rest there",
            "its animation: its bone 2: its parent closes a loop, left without one",
        ]
    );
}

#[test]
fn an_animated_model_cut_anywhere_is_refused_and_never_panics() {
    let bytes = animated_model();
    // Past the header, a model cut short of what it points to is refused.
    for length in 0x138..bytes.len() {
        assert!(m2::model(&bytes[..length]).is_err(), "cut at {length}");
    }
}

/// Run on demand: `cargo test -p uniwow-module-assets every_animation -- --ignored --nocapture`.
#[test]
#[ignore = "reads the animations of every model of the client named by UNIWOW_CLIENT"]
fn every_animation_of_the_client_is_read_its_played_keys_kept() {
    let Some(client) = client() else { return };
    let models: Vec<String> = client
        .chain
        .files_under("")
        .into_iter()
        .filter(|name| name.to_ascii_lowercase().ends_with(".m2"))
        .collect();
    let next = AtomicUsize::new(0);
    let (animated, kept, keys, most) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let interpolations = Mutex::new(BTreeMap::<String, usize>::new());
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                while let Some(name) = models.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let Ok(model) = client.model(&FileRef::Path(name.clone())) else {
                        continue;
                    };
                    let animation = &model.animation;
                    if animation.bones.len() > 1 {
                        animated.fetch_add(1, Ordering::Relaxed);
                    }
                    most.fetch_max(animation.bones.len(), Ordering::Relaxed);
                    assert_eq!(animation.order.len(), animation.bones.len(), "{name}");
                    kept.fetch_add(
                        animation.sequences.iter().filter(|sequence| sequence.kept).count(),
                        Ordering::Relaxed,
                    );
                    let mut found = interpolations.lock().unwrap();
                    for bone in &animation.bones {
                        for (kind, count) in [
                            (
                                bone.translation.interpolation,
                                bone.translation.keys.iter().map(|k| k.times.len()).sum::<usize>(),
                            ),
                            (
                                bone.rotation.interpolation,
                                bone.rotation.keys.iter().map(|k| k.times.len()).sum(),
                            ),
                            (
                                bone.scale.interpolation,
                                bone.scale.keys.iter().map(|k| k.times.len()).sum(),
                            ),
                        ] {
                            keys.fetch_add(count, Ordering::Relaxed);
                            if count > 0 {
                                *found.entry(format!("{kind:?}")).or_default() += 1;
                            }
                        }
                    }
                }
            });
        }
    });
    let animations = client.tables.animations(&client.chain).unwrap();
    let fallback = |id: u32| {
        animations
            .iter()
            .find(|animation| animation.id == id)
            .map(|animation| (animation.name.clone(), animation.fallback))
    };
    eprintln!(
        "{} models read in {:.1} s: {} with bones that move, {} bones at most; {} sequences kept, {} keys of bones kept; tracks of bones with keys by interpolation {:?}",
        models.len(),
        started.elapsed().as_secs_f64(),
        animated.load(Ordering::Relaxed),
        most.load(Ordering::Relaxed),
        kept.load(Ordering::Relaxed),
        keys.load(Ordering::Relaxed),
        interpolations.into_inner().unwrap()
    );
    eprintln!(
        "{} animations; Stand {:?}, Walk {:?}, Run {:?}",
        animations.len(),
        fallback(0),
        fallback(4),
        fallback(5)
    );
    assert_eq!(fallback(0), Some(("Stand".to_owned(), 147)));
    assert_eq!(fallback(4), Some(("Walk".to_owned(), 0)));
    assert_eq!(fallback(5), Some(("Run".to_owned(), 0)));
}
