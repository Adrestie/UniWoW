//! Tests of the buildings: of 3.3.5a and modern, on files the tests write themselves; over every
//! building of the client when `UNIWOW_CLIENT` names it, and over those wow.export exported when
//! `UNIWOW_MODERN` names the folder of its exports.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use uniwow_api::formats::{
    BspNode, DoodadSet, FileRef, FogBand, Portal, PortalRef, Wmo, WmoBatch, WmoDoodad, WmoFace, WmoFog, WmoGroup,
    WmoLight, WmoLiquid, WmoMaterial,
};
use uniwow_api::serde_json;

use crate::chain::Source;
use crate::terrain_tests::client;
use crate::tests::{Stored, file, scratch, write_archive};
use crate::{Client, wmo};

/// Whether `name` is the file of a group: its name ending in `_` and three digits.
fn is_group(name: &str) -> bool {
    let stem = name.to_ascii_lowercase();
    let stem = stem.strip_suffix(".wmo").unwrap_or(&stem);
    stem.len() > 4
        && stem.as_bytes()[stem.len() - 4] == b'_'
        && stem[stem.len() - 3..].bytes().all(|b| b.is_ascii_digit())
}

/// A chunk named as the format writes it, reversed as the file holds it.
fn tagged(name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = vec![name[3], name[2], name[1], name[0]];
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn halves(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

/// A colour red, green, blue and alpha, as the file holds it.
fn colour(rgba: [u8; 4]) -> [u8; 4] {
    [rgba[2], rgba[1], rgba[0], rgba[3]]
}

/// A material of 64 bytes: its flags, shader, blending, three texture fields and three colours.
fn material(shader: u32, blending: u32, textures: [u32; 3], colours: [[u8; 4]; 3]) -> Vec<u8> {
    let mut out = words(&[0x4, shader, blending, textures[0]]);
    out.extend(colour(colours[0]));
    out.extend(words(&[0, textures[1]]));
    out.extend(colour(colours[1]));
    out.extend(words(&[3, textures[2]]));
    out.extend(colour(colours[2]));
    out.extend(words(&[0, 0, 0, 0, 0]));
    out
}

/// The header of a group: its flags, bounds, portals, batches by kind, fogs, liquid and id.
fn group_header(flags: u32, portals: [u16; 2], batch_counts: [u16; 3]) -> Vec<u8> {
    let mut out = words(&[0, 0, flags]);
    out.extend(floats(&[-1.0, -2.0, -3.0, 1.0, 2.0, 3.0]));
    out.extend(halves(&[
        portals[0],
        portals[1],
        batch_counts[0],
        batch_counts[1],
        batch_counts[2],
        0,
    ]));
    out.extend([1, 2, 0, 0]);
    out.extend(words(&[13, 42, 0, 0]));
    assert_eq!(out.len(), 68);
    out
}

/// A batch of 24 bytes: its first index and count, first and last vertex, flags and material; a
/// material past 255 in the last of its bounds when `flags` says so.
fn batch(first: u32, count: u16, vertices: [u16; 2], flags: u8, material: u16) -> Vec<u8> {
    let large = flags & 0x2 != 0;
    let mut out = halves(&[1, 2, 3, 4, 5, if large { material } else { 6 }]);
    out.extend(words(&[first]));
    out.extend(halves(&[count, vertices[0], vertices[1]]));
    out.extend([flags, if large { 0 } else { material as u8 }]);
    out
}

/// A node of a BSP tree as the file holds it.
fn bsp_node(flags: u16, children: [i16; 2], count: u16, first: u32, distance: f32) -> Vec<u8> {
    let mut out = halves(&[flags, children[0] as u16, children[1] as u16, count]);
    out.extend(words(&[first]));
    out.extend(floats(&[distance]));
    out
}

/// The first group of the test buildings: two triangles over four vertices, both sets of
/// coordinates and of colours, a batch, a doodad, a BSP tree and a liquid; its triangles of `MPY2`
/// when `modern`, its batch then of the material 1 given as a large one.
fn group_one(modern: bool) -> Vec<u8> {
    let mut inside = group_header(
        0x1000 | 0x800 | 0x4 | 0x2000 | 0x100_0000 | 0x200_0000,
        [0, 2],
        [0, 1, 0],
    );
    if modern {
        inside.extend(tagged(b"MPY2", &halves(&[0x20, 0, 0, 0xFFFF])));
    } else {
        inside.extend(tagged(b"MOPY", &[0x20, 0, 0, 0xFF]));
    }
    inside.extend(tagged(b"MOVI", &halves(&[0, 1, 2, 1, 2, 3])));
    inside.extend(tagged(
        b"MOVT",
        &floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.5]),
    ));
    inside.extend(tagged(b"MONR", &floats(&[0.0, 0.0, 1.0].repeat(4))));
    inside.extend(tagged(b"MOTV", &floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0])));
    let material = if modern { 1 } else { 0 };
    let flags = if modern { 0x2 } else { 0 };
    inside.extend(tagged(b"MOBA", &batch(0, 6, [0, 3], flags, material)));
    inside.extend(tagged(b"MODR", &halves(&[1])));
    // Its tree: cut across X at 0.5, a triangle on each side.
    inside.extend(tagged(
        b"MOBN",
        &[
            bsp_node(0, [1, 2], 0, 0, 0.5),
            bsp_node(0x4, [-1, -1], 1, 0, 0.0),
            bsp_node(0x4, [-1, -1], 1, 1, 0.0),
        ]
        .concat(),
    ));
    inside.extend(tagged(b"MOBR", &halves(&[0, 1])));
    inside.extend(tagged(b"MOCV", &[10, 20, 30, 40].repeat(4)));
    let mut liquid = words(&[2, 2, 1, 1]);
    liquid.extend(floats(&[5.0, 6.0, 7.0]));
    liquid.extend(halves(&[3]));
    for height in [1.0f32, 2.0, 3.0, 4.0] {
        liquid.extend([9, 8, 7, 6]);
        liquid.extend(height.to_le_bytes());
    }
    liquid.push(0x0F);
    inside.extend(tagged(b"MLIQ", &liquid));
    inside.extend(tagged(b"MOTV", &floats(&[0.5; 8])));
    inside.extend(tagged(b"MOCV", &[1, 2, 3, 4].repeat(4)));
    let mut out = tagged(b"MVER", &words(&[17]));
    out.extend(tagged(b"MOGP", &inside));
    out
}

/// The second group: a triangle, outside.
fn group_two() -> Vec<u8> {
    let mut inside = group_header(0x8, [2, 0], [0, 0, 1]);
    inside.extend(tagged(b"MOPY", &[0, 0]));
    inside.extend(tagged(b"MOVI", &halves(&[0, 1, 2])));
    inside.extend(tagged(b"MOVT", &floats(&[0.0; 9])));
    inside.extend(tagged(b"MONR", &floats(&[0.0; 9])));
    inside.extend(tagged(b"MOTV", &floats(&[0.0; 6])));
    inside.extend(tagged(b"MOBA", &batch(0, 3, [0, 2], 0, 0)));
    let mut out = tagged(b"MVER", &words(&[17]));
    out.extend(tagged(b"MOGP", &inside));
    out
}

/// A test building's root: of 3.3.5a, its textures and doodads by name; modern, by FileDataID,
/// its groups too, each with a coarser level after the finest.
fn root(modern: bool) -> Vec<u8> {
    let mut out = tagged(b"MVER", &words(&[17]));
    let mut header = words(&[2, 2, 1, 1, 2, 2, 1]);
    header.extend(colour([11, 22, 33, 44]));
    header.extend(words(&[77]));
    header.extend(floats(&[-1.0, -2.0, -3.0, 4.0, 5.0, 6.0]));
    header.extend(halves(&[if modern { 0x19 } else { 0x9 }, if modern { 2 } else { 0 }]));
    out.extend(tagged(b"MOHD", &header));
    let colours = [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]];
    // A third texture field of 0.1, as 3.3.5a holds there.
    let tenth = 0.1f32.to_bits();
    if modern {
        out.extend(tagged(
            b"MOMT",
            &[
                material(0, 1, [5001, 0, tenth], colours),
                material(7, 0, [5001, 5002, 5003], colours),
            ]
            .concat(),
        ));
    } else {
        // `a.blp` at 0, an empty name at 12, `env.blp` at 16.
        out.extend(tagged(b"MOTX", b"tex\\a.blp\0\0\0\0\0\0\0tex\\env.blp\0"));
        out.extend(tagged(
            b"MOMT",
            &[
                material(0, 1, [0, 12, tenth], colours),
                material(5, 0, [0, 16, 0], colours),
            ]
            .concat(),
        ));
    }
    out.extend(tagged(b"MOGN", b"\0\0hall\0"));
    let mut infos = words(&[0x2000]);
    infos.extend(floats(&[-1.0, -2.0, -3.0, 1.0, 2.0, 3.0]));
    infos.extend(words(&[2, 0x8]));
    infos.extend(floats(&[-4.0; 6]));
    infos.extend((-1i32).to_le_bytes());
    out.extend(tagged(b"MOGI", &infos));
    let skybox: &[u8] = if modern { b"\0\0\0\0" } else { b"sky\\dome.mdx\0" };
    out.extend(tagged(b"MOSB", skybox));
    out.extend(tagged(
        b"MOPV",
        &floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
    ));
    let mut portal = halves(&[0, 4]);
    portal.extend(floats(&[0.0, 1.0, 0.0, 5.0]));
    out.extend(tagged(b"MOPT", &portal));
    out.extend(tagged(
        b"MOPR",
        &[halves(&[0, 0, 1, 0]), halves(&[0, 1, 0xFFFF, 0])].concat(),
    ));
    let mut light = vec![1, 1, 0, 0];
    light.extend(colour([200, 100, 50, 255]));
    light.extend(floats(&[1.0, 2.0, 3.0, 2.5, 0.0, 0.0, 0.0, 1.0, 3.0, 9.0]));
    out.extend(tagged(b"MOLT", &light));
    let mut set = b"Set_$DefaultGlobal\0\0".to_vec();
    set.extend(words(&[0, 2, 0]));
    out.extend(tagged(b"MODS", &set));
    if modern {
        out.extend(tagged(b"MODI", &words(&[6001, 6002])));
    } else {
        out.extend(tagged(b"MODN", b"world\\a.mdx\0world\\b.mdx\0"));
    }
    let mut doodads = Vec::new();
    for (name, flags) in [(0u32, 0u32), (if modern { 1 } else { 12 }, 0x1)] {
        doodads.extend(words(&[name | flags << 24]));
        doodads.extend(floats(&[1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0, 1.5]));
        doodads.extend(colour([7, 8, 9, 255]));
    }
    out.extend(tagged(b"MODD", &doodads));
    let mut fog = words(&[0x1]);
    fog.extend(floats(&[1.0, 2.0, 3.0, 10.0, 20.0, 300.0, 0.25]));
    fog.extend(colour([1, 2, 3, 4]));
    fog.extend(floats(&[50.0, 0.5]));
    fog.extend(colour([5, 6, 7, 8]));
    out.extend(tagged(b"MFOG", &fog));
    if modern {
        // The finest level of each group first, then the coarser ones.
        out.extend(tagged(b"GFID", &words(&[7001, 7002, 7101, 7102])));
    }
    out
}

/// Reads `root` at `path`, its groups from `files` by FileDataID or by path; those asked for
/// counted in `asked`.
fn read_with(
    root: &[u8],
    path: &str,
    files: &HashMap<String, Vec<u8>>,
    asked: &Mutex<Vec<String>>,
) -> Result<Wmo, String> {
    wmo::read(root, path, |group| {
        let key = group.id.map_or_else(|| group.path.clone(), |id| id.to_string());
        asked.lock().unwrap().push(key.clone());
        files.get(&key).cloned().ok_or_else(|| format!("{key}: not written"))
    })
}

fn files(modern: bool) -> HashMap<String, Vec<u8>> {
    let keys = if modern {
        ["7001", "7002"]
    } else {
        [r"world\wmo\test_000.wmo", r"world\wmo\test_001.wmo"]
    };
    HashMap::from([
        (keys[0].to_owned(), group_one(modern)),
        (keys[1].to_owned(), group_two()),
    ])
}

#[test]
fn a_building_of_3_3_5a_reads_its_root_and_its_groups_beside_it() {
    let asked = Mutex::new(Vec::new());
    let wmo = read_with(&root(false), r"world\wmo\test.wmo", &files(false), &asked).unwrap();
    assert!(wmo.faults.is_empty(), "{:?}", wmo.faults);
    let mut asked = asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(asked, [r"world\wmo\test_000.wmo", r"world\wmo\test_001.wmo"]);
    assert_eq!(
        (wmo.flags, wmo.ambient, wmo.id, wmo.bounds),
        (0x9, [11, 22, 33, 44], 77, [[-1.0, -2.0, -3.0], [4.0, 5.0, 6.0]])
    );
    assert_eq!(wmo.skybox, Some(FileRef::Path(r"sky\dome.mdx".to_owned())));
    let path = |name: &str| Some(FileRef::Path(name.to_owned()));
    assert_eq!(
        wmo.materials[0],
        WmoMaterial {
            flags: 0x4,
            shader: 0,
            blending: 1,
            // The second names an empty name; the third field holds no texture in 3.3.5a.
            textures: [path(r"tex\a.blp"), None, None],
            emissive: [1, 2, 3, 4],
            diffuse: [5, 6, 7, 8],
            colour: [9, 10, 11, 12],
            ground: 3,
        }
    );
    assert_eq!(
        wmo.materials[1].textures,
        [path(r"tex\a.blp"), path(r"tex\env.blp"), None]
    );
    assert_eq!(
        wmo.portals,
        [Portal {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            plane: [0.0, 1.0, 0.0, 5.0],
        }]
    );
    assert_eq!(
        wmo.portal_refs,
        [
            PortalRef {
                portal: 0,
                group: 0,
                side: 1
            },
            PortalRef {
                portal: 0,
                group: 1,
                side: -1
            },
        ]
    );
    assert_eq!(
        wmo.lights,
        [WmoLight {
            kind: 1,
            attenuated: true,
            colour: [200, 100, 50, 255],
            position: [1.0, 2.0, 3.0],
            intensity: 2.5,
            attenuation: [3.0, 9.0],
        }]
    );
    assert_eq!(
        wmo.doodad_sets,
        [DoodadSet {
            name: "Set_$DefaultGlobal".to_owned(),
            first: 0,
            count: 2,
        }]
    );
    assert_eq!(
        wmo.doodads[1],
        WmoDoodad {
            file: FileRef::Path(r"world\b.mdx".to_owned()),
            flags: 0x1,
            position: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: 1.5,
            colour: [7, 8, 9, 255],
        }
    );
    assert_eq!(
        wmo.fogs,
        [WmoFog {
            flags: 0x1,
            position: [1.0, 2.0, 3.0],
            radii: [10.0, 20.0],
            fog: FogBand {
                end: 300.0,
                start: 0.25,
                colour: [1, 2, 3, 4]
            },
            underwater: FogBand {
                end: 50.0,
                start: 0.5,
                colour: [5, 6, 7, 8]
            },
        }]
    );

    let [one, two] = &wmo.groups[..] else {
        panic!("{} groups", wmo.groups.len())
    };
    assert_eq!((one.name.as_str(), two.name.as_str()), ("hall", ""));
    assert_eq!(
        (
            one.flags & 0x1000,
            one.bounds,
            one.portals,
            one.batch_counts,
            one.fogs,
            one.liquid_type,
            one.id
        ),
        (
            0x1000,
            [[-1.0, -2.0, -3.0], [1.0, 2.0, 3.0]],
            [0, 2],
            [0, 1, 0],
            [1, 2, 0, 0],
            13,
            42
        )
    );
    assert_eq!(one.vertices[3], [1.0, 1.0, 0.5]);
    assert_eq!(one.normals, vec![[0.0, 0.0, 1.0]; 4]);
    assert_eq!(one.coordinates.len(), 2, "both sets of coordinates");
    assert_eq!((one.coordinates[0][1], one.coordinates[1][3]), ([1.0, 0.0], [0.5, 0.5]));
    assert_eq!(one.colours, [vec![[30, 20, 10, 40]; 4], vec![[3, 2, 1, 4]; 4]]);
    assert_eq!(one.triangles, [0, 1, 2, 1, 2, 3]);
    assert_eq!(
        one.faces,
        [
            WmoFace {
                flags: 0x20,
                material: Some(0)
            },
            WmoFace {
                flags: 0,
                material: None
            },
        ]
    );
    assert_eq!(
        one.batches,
        [WmoBatch {
            first: 0,
            count: 6,
            vertices: [0, 3],
            flags: 0,
            material: 0
        }]
    );
    assert_eq!(one.doodad_refs, [1]);
    let leaf = |first| BspNode {
        flags: 0x4,
        children: [-1, -1],
        first,
        count: 1,
        distance: 0.0,
    };
    assert_eq!(
        one.bsp,
        [
            BspNode {
                flags: 0,
                children: [1, 2],
                first: 0,
                count: 0,
                distance: 0.5
            },
            leaf(0),
            leaf(1)
        ]
    );
    assert_eq!(one.bsp_faces, [0, 1]);
    assert_eq!(
        one.liquid,
        Some(WmoLiquid {
            size: [2, 2],
            tiles: [1, 1],
            corner: [5.0, 6.0, 7.0],
            material: 3,
            heights: vec![1.0, 2.0, 3.0, 4.0],
            data: vec![[9, 8, 7, 6]; 4],
            tile_flags: vec![0x0F],
        })
    );
    assert_eq!(
        (two.flags, two.portals, two.batch_counts, two.triangles.len()),
        (0x8, [2, 0], [0, 0, 1], 3)
    );
    assert!(two.colours.is_empty() && two.liquid.is_none());
}

#[test]
fn a_modern_building_names_its_groups_doodads_and_textures_by_file_data_id() {
    let asked = Mutex::new(Vec::new());
    let wmo = read_with(&root(true), r"world\wmo\test.wmo", &files(true), &asked).unwrap();
    assert!(wmo.faults.is_empty(), "{:?}", wmo.faults);
    let mut asked = asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(asked, ["7001", "7002"], "the groups at their finest level");
    assert_eq!(wmo.skybox, None, "an empty name");
    let id = |id: u32| Some(FileRef::Id(id));
    assert_eq!(wmo.materials[0].textures, [id(5001), None, None]);
    assert_eq!(
        wmo.materials[1].textures,
        [id(5001), id(5002), id(5003)],
        "a shader of a later client"
    );
    assert_eq!(
        wmo.doodads.iter().map(|doodad| doodad.file.clone()).collect::<Vec<_>>(),
        [FileRef::Id(6001), FileRef::Id(6002)]
    );
    let one = &wmo.groups[0];
    assert_eq!(
        one.faces,
        [
            WmoFace {
                flags: 0x20,
                material: Some(0)
            },
            WmoFace {
                flags: 0,
                material: None
            },
        ]
    );
    assert_eq!(
        one.batches[0].material, 1,
        "a large material, in the last of the bounds"
    );
}

#[test]
fn what_a_building_lacks_is_said_and_left_out() {
    // A batch past its triangles, a triangle and a batch of a material it does not have, a doodad
    // it does not have, a BSP tree whose root has a child past its nodes, a set past its doodads, a
    // portal reference to a group it does not have, and a group missing.
    let mut inside = group_header(0x8, [1, 4], [0, 0, 3]);
    inside.extend(tagged(b"MOPY", &[0, 9]));
    inside.extend(tagged(b"MOVI", &halves(&[0, 1, 2])));
    inside.extend(tagged(b"MOVT", &floats(&[0.0; 9])));
    inside.extend(tagged(b"MONR", &floats(&[0.0; 9])));
    inside.extend(tagged(b"MOTV", &floats(&[0.0; 6])));
    inside.extend(tagged(
        b"MOBA",
        &[
            batch(0, 3, [0, 2], 0, 0),
            batch(3, 3, [0, 2], 0, 0),
            batch(0, 3, [0, 2], 0, 9),
        ]
        .concat(),
    ));
    inside.extend(tagged(b"MODR", &halves(&[1, 5])));
    inside.extend(tagged(b"MOBN", &bsp_node(0, [-1, 5], 0, 0, 0.0)));
    inside.extend(tagged(b"MOBR", &halves(&[0])));
    let mut group = tagged(b"MVER", &words(&[17]));
    group.extend(tagged(b"MOGP", &inside));
    let mut root = root(false);
    let refs = halves(&[0, 0, 1, 0, 0, 7, 1, 0]);
    let at = root.windows(4).position(|window| window == b"RPOM").unwrap();
    root.splice(at + 8..at + 8 + refs.len(), refs);
    let set_count = root
        .windows(18)
        .position(|window| window == b"Set_$DefaultGlobal")
        .unwrap()
        + 24;
    root[set_count..set_count + 4].copy_from_slice(&5u32.to_le_bytes());
    let files = HashMap::from([(r"world\wmo\test_000.wmo".to_owned(), group)]);
    let wmo = read_with(&root, r"world\wmo\test.wmo", &files, &Mutex::new(Vec::new())).unwrap();

    let one = &wmo.groups[0];
    assert_eq!(
        one.batches.iter().map(|batch| batch.count).collect::<Vec<_>>(),
        [3, 0, 0]
    );
    assert_eq!(one.faces[0].material, None);
    assert_eq!(one.doodad_refs, [1]);
    assert!(one.bsp.is_empty() && one.bsp_faces.is_empty());
    assert_eq!(wmo.doodad_sets[0].count, 2);
    let two = &wmo.groups[1];
    assert_eq!(
        (two.flags, two.bounds, two.vertices.len()),
        (0x8, [[-4.0; 3]; 2], 0),
        "missing, as its root says"
    );
    let said = wmo.faults.join("\n");
    for fault in [
        "group 0: 1 triangles of a material past its 2",
        "group 0: its batch 1",
        "group 0: its batch 2",
        "group 0: 1 of its doodads past the 2 of the building",
        "group 0: its portals 1 to 5 past the 2 references",
        "group 0: its BSP tree left out: its node 0 of 1 has the child 5",
        r"group 1 (world\wmo\test_001.wmo): world\wmo\test_001.wmo: not written",
        "the portal reference 1: the portal 0 of 1, the group 7 of 2",
        "the doodad set \"Set_$DefaultGlobal\": 5 doodads from 0, of 2",
    ] {
        assert!(said.contains(fault), "{fault} not in {said}");
    }
    assert_eq!(wmo.faults.len(), 9, "{said}");
}

#[test]
fn a_bsp_tree_that_does_not_hold_together_is_said() {
    let node = |flags, children, first, count| BspNode {
        flags,
        children,
        first,
        count,
        distance: 0.0,
    };
    let tree = |nodes: Vec<BspNode>, faces: Vec<u16>| WmoGroup {
        triangles: vec![0; 6],
        bsp: nodes,
        bsp_faces: faces,
        ..WmoGroup::default()
    };
    let leaf = |first, count| node(0x4, [-1, -1], first, count);
    assert_eq!(
        wmo::bsp_fault(&tree(vec![node(0, [1, 2], 0, 0), leaf(0, 1), leaf(1, 1)], vec![0, 1])),
        None
    );
    for (nodes, faces, said) in [
        (vec![node(0, [-1, 0], 0, 0)], vec![], "its node 0 of 1 has the child 0"),
        (
            vec![node(0, [1, -1], 0, 0), node(0, [0, -1], 0, 0)],
            vec![],
            "its node 1 of 2 has the child 0",
        ),
        (
            vec![node(0, [1, 2], 0, 0), leaf(0, 1)],
            vec![0],
            "its node 0 of 2 has the child 2",
        ),
        (vec![leaf(1, 1)], vec![0], "its leaf 0 holds the triangles 1 to 2 of 1"),
        (vec![leaf(0, 1)], vec![2], "its leaves hold the triangle 2 of 2"),
    ] {
        assert_eq!(wmo::bsp_fault(&tree(nodes, faces)).as_deref(), Some(said));
    }
}

#[test]
fn a_group_longer_in_its_header_than_its_file_is_read_to_its_end() {
    let mut group = group_two();
    let size = u32::from_le_bytes(group[16..20].try_into().unwrap());
    group[16..20].copy_from_slice(&(size + 44).to_le_bytes());
    let files = HashMap::from([
        (r"world\wmo\test_000.wmo".to_owned(), group_one(false)),
        (r"world\wmo\test_001.wmo".to_owned(), group),
    ]);
    let wmo = read_with(&root(false), r"world\wmo\test.wmo", &files, &Mutex::new(Vec::new())).unwrap();
    assert!(wmo.faults.is_empty(), "{:?}", wmo.faults);
    assert_eq!(wmo.groups[1].triangles.len(), 3);
}

#[test]
fn a_building_damaged_is_refused_or_said_and_never_panics() {
    let mut other = root(false);
    other[8..12].copy_from_slice(&16u32.to_le_bytes());
    assert!(
        read_with(&other, "a.wmo", &files(false), &Mutex::new(Vec::new())).is_err(),
        "version 16"
    );
    let empty = HashMap::new();
    let asked = Mutex::new(Vec::new());
    let mut seed = 0x1234_5678u32;
    for sample in [root(false), root(true), group_one(false), group_one(true), group_two()] {
        for _ in 0..400 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let mut damaged = sample.clone();
            let at = seed as usize % damaged.len();
            damaged[at] ^= 0xFF;
            let cut = &damaged[..damaged.len() - (seed as usize >> 8) % 64];
            let _ = read_with(cut, r"world\wmo\test.wmo", &files(false), &asked);
            let as_group = HashMap::from([
                (r"world\wmo\test_000.wmo".to_owned(), cut.to_vec()),
                (r"world\wmo\test_001.wmo".to_owned(), cut.to_vec()),
            ]);
            let _ = read_with(&root(false), r"world\wmo\test.wmo", &as_group, &asked);
            let _ = read_with(cut, r"world\wmo\test.wmo", &empty, &asked);
        }
    }
}

#[test]
fn the_service_reads_a_building_and_its_groups_from_the_archives() {
    let folder = scratch("wmo");
    let archive = folder.join("common.mpq");
    let (one, two) = (group_one(false), group_two());
    let root = root(false);
    let files = [
        file(r"World\wmo\Test\Test.wmo", &root, Stored::Plain),
        file(r"World\wmo\Test\Test_000.wmo", &one, Stored::Plain),
        file(r"World\wmo\Test\Test_001.wmo", &two, Stored::Plain),
    ];
    write_archive(&archive, 0, &files);
    let (client, _) = Client::open(vec![Source::open(&archive).unwrap()], &folder, "enUS");
    let read = client
        .wmo(&FileRef::Path(r"world\wmo\test\test.wmo".to_owned()))
        .unwrap();
    let written = HashMap::from([
        (r"world\wmo\test\test_000.wmo".to_owned(), one.clone()),
        (r"world\wmo\test\test_001.wmo".to_owned(), two.clone()),
    ]);
    let expected = read_with(&root, r"world\wmo\test\test.wmo", &written, &Mutex::new(Vec::new())).unwrap();
    assert_eq!(read, expected);
    assert!(read.faults.is_empty(), "{:?}", read.faults);
    let missing = client.wmo(&FileRef::Path("missing.wmo".to_owned())).err().unwrap();
    assert!(missing.contains("not in the client"), "{missing}");
    let _ = std::fs::remove_dir_all(folder);
}

/// The files wow.export wrote for `root`, by FileDataID, from its manifest: their paths, which it
/// gives from the manifest as if it were a folder, made relative to the folder of `root`.
fn manifest(root: &Path) -> HashMap<u32, String> {
    let path = root.with_extension("manifest.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    ["textures", "groups"]
        .iter()
        .flat_map(|list| json[list].as_array().cloned().unwrap_or_default())
        .filter_map(|entry| {
            let file = entry["file"].as_str()?;
            let file = file
                .strip_prefix(r"..\")
                .or_else(|| file.strip_prefix("../"))
                .unwrap_or(file);
            Some((entry["fileDataID"].as_u64()? as u32, file.to_owned()))
        })
        .collect()
}

/// The roots of the buildings under `folder`: neither a group nor a level of detail.
fn roots_under(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_ascii_lowercase();
        if path.is_dir() {
            roots_under(&path, found);
        } else if name.ends_with(".wmo") && !is_group(&name) && !name.contains("_lod") {
            found.push(path);
        }
    }
}

#[test]
fn the_modern_buildings_exported_by_wow_export_are_read_whole() {
    let Ok(folder) = std::env::var("UNIWOW_MODERN") else {
        eprintln!("skipped: UNIWOW_MODERN names no folder of exports of wow.export");
        return;
    };
    let mut roots = Vec::new();
    roots_under(Path::new(&folder), &mut roots);
    assert!(!roots.is_empty(), "no building under {folder}");
    for path in &roots {
        let named = manifest(path);
        let bytes = std::fs::read(path).unwrap();
        let wmo = wmo::read(&bytes, &path.to_string_lossy(), |group| {
            // By the FileDataID the root gives, as wow.export named the file.
            let file = match group.id {
                Some(id) => path.with_file_name(
                    named
                        .get(&id)
                        .ok_or_else(|| format!("its group {id} not in the manifest"))?,
                ),
                None => PathBuf::from(&group.path),
            };
            std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))
        })
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(wmo.faults.is_empty(), "{}: {:?}", path.display(), wmo.faults);
        // Its groups at their finest level: as many as the root says, each with its geometry.
        assert!(
            wmo.groups
                .iter()
                .all(|group| !group.vertices.is_empty() && group.faces.len() * 3 == group.triangles.len()),
            "{}",
            path.display()
        );
        // Its textures by FileDataID, those wow.export wrote.
        let ids: Vec<u32> = wmo
            .materials
            .iter()
            .flat_map(|material| material.textures.iter().flatten())
            .filter_map(|texture| match texture {
                FileRef::Id(id) => Some(*id),
                FileRef::Path(_) => None,
            })
            .collect();
        assert!(
            ids.iter().all(|id| named.contains_key(id)),
            "{}: {ids:?}",
            path.display()
        );
        eprintln!(
            "{}: {} groups, {} triangles, {} batches, {} materials, {} doodads ({} by FileDataID), textures {:?}",
            path.strip_prefix(&folder).unwrap_or(path).display(),
            wmo.groups.len(),
            wmo.groups.iter().map(|group| group.triangles.len() / 3).sum::<usize>(),
            wmo.groups.iter().map(|group| group.batches.len()).sum::<usize>(),
            wmo.materials.len(),
            wmo.doodads.len(),
            wmo.doodads
                .iter()
                .filter(|doodad| matches!(doodad.file, FileRef::Id(_)))
                .count(),
            ids,
        );
    }
}

/// Run on demand, a few seconds: `cargo test -p uniwow-module-assets every_building -- --ignored --nocapture`.
#[test]
#[ignore = "reads every building of the client named by UNIWOW_CLIENT"]
fn every_building_of_the_client_is_read_whole() {
    let Some(client) = client() else { return };
    let roots: Vec<String> = client
        .chain
        .files_under("")
        .into_iter()
        .filter(|name| name.to_ascii_lowercase().ends_with(".wmo") && !is_group(name))
        .collect();
    let next = AtomicUsize::new(0);
    let refused = Mutex::new(Vec::new());
    let faults = Mutex::new(BTreeMap::<String, Vec<String>>::new());
    let read = Mutex::new(Vec::<(String, usize, usize, usize, usize, usize)>::new());
    let stormwind = Mutex::new(None);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                while let Some(name) = roots.get(next.fetch_add(1, Ordering::Relaxed)) {
                    match client.wmo(&FileRef::Path(name.clone())) {
                        Ok(wmo) => {
                            if name.eq_ignore_ascii_case(r"World\wmo\Azeroth\Buildings\Stormwind\Stormwind.wmo") {
                                *stormwind.lock().unwrap() = Some(wmo.clone());
                            }
                            let Wmo {
                                groups,
                                doodads,
                                portals,
                                ..
                            } = &wmo;
                            read.lock().unwrap().push((
                                name.clone(),
                                groups.len(),
                                groups.iter().map(|g| g.triangles.len() / 3).sum(),
                                portals.len(),
                                doodads.len(),
                                groups.iter().filter(|g| g.liquid.is_some()).count(),
                            ));
                            if !wmo.faults.is_empty() {
                                faults.lock().unwrap().insert(name.clone(), wmo.faults);
                            }
                        }
                        Err(reason) => refused.lock().unwrap().push(reason),
                    }
                }
            });
        }
    });
    let read = read.into_inner().unwrap();
    let faults = faults.into_inner().unwrap();
    let refused = refused.into_inner().unwrap();
    let groups: usize = read.iter().map(|r| r.1).sum();
    let triangles: usize = read.iter().map(|r| r.2).sum();
    let liquids: usize = read.iter().map(|r| r.5).sum();
    eprintln!(
        "{} buildings read of {} in {:?}: {groups} groups, {triangles} triangles, {liquids} liquids; refused {refused:?}; {} with faults:",
        read.len(),
        roots.len(),
        started.elapsed(),
        faults.len()
    );
    for (name, list) in faults.iter().take(40) {
        eprintln!("  {name}: {} {:?}", list.len(), &list[..list.len().min(4)]);
    }
    assert!(refused.is_empty() && faults.is_empty());
    // As the probes of the proposal of 9.6 counted them; the doodads as their chunks hold them,
    // fewer than the 259,338 their headers count.
    let portals: usize = read.iter().map(|r| r.3).sum();
    let doodads: usize = read.iter().map(|r| r.4).sum();
    assert_eq!((read.len(), groups, portals, doodads), (1_986, 9_347, 7_548, 250_296));
    let stormwind = stormwind.into_inner().unwrap().expect("Stormwind read");
    let triangles: usize = stormwind.groups.iter().map(|g| g.triangles.len() / 3).sum();
    let batches: usize = stormwind.groups.iter().map(|g| g.batches.len()).sum();
    let coloured = stormwind.groups.iter().filter(|g| !g.colours.is_empty()).count();
    let liquids = stormwind.groups.iter().filter(|g| g.liquid.is_some()).count();
    let inside = stormwind.groups.iter().filter(|g| g.flags & 0x2000 != 0).count();
    assert_eq!(
        (
            stormwind.groups.len(),
            inside,
            stormwind.portals.len(),
            stormwind.lights.len(),
            stormwind.doodads.len(),
            stormwind.doodad_sets.len(),
        ),
        (286, 278, 319, 606, 6_157, 1)
    );
    assert_eq!((triangles, batches, coloured, liquids), (727_741, 2_754, 192, 5));
    let reading = Instant::now();
    let path = FileRef::Path(r"World\wmo\Azeroth\Buildings\Stormwind\Stormwind.wmo".to_owned());
    client.wmo(&path).unwrap();
    eprintln!(
        "Stormwind read again in {:?}, its groups on one thread",
        reading.elapsed()
    );
}
