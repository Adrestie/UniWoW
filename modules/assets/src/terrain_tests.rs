//! Tests of the terrain and the textures on files the tests write themselves, never on files of the
//! client; and, when `UNIWOW_CLIENT` names the folder of a client, on its own maps.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use uniwow_api::formats::{FileRef, TextureFormat, Tile};

use crate::chain::{self, Source};
use crate::tests::{Stored, file, scratch, write_archive};
use crate::{Client, blp, terrain};

/// The client named by `UNIWOW_CLIENT`, open, or none.
pub(crate) fn client() -> Option<Client> {
    let Ok(folder) = std::env::var("UNIWOW_CLIENT") else {
        eprintln!("skipped: UNIWOW_CLIENT names no client folder");
        return None;
    };
    let folder = PathBuf::from(folder);
    let locale = chain::locale(&folder).unwrap();
    let sources = chain::order(&folder, &locale)
        .iter()
        .map(|path| Source::open(path).unwrap())
        .collect();
    Some(Client::open(sources, &folder, &locale).0)
}

const TILE: f32 = 1600.0 / 3.0;
const CHUNK: f32 = TILE / 16.0;
const ORIGIN: f32 = 32.0 * TILE;

/// Whether the chunks of `tile`, at `x` and `y`, give the position their tile and index place them
/// at, as the client does.
fn placed(tile: &Tile, x: u32, y: u32) -> bool {
    tile.chunks.iter().all(|chunk| {
        let expected = [
            ORIGIN - TILE * y as f32 - CHUNK * chunk.index[1] as f32,
            ORIGIN - TILE * x as f32 - CHUNK * chunk.index[0] as f32,
        ];
        (chunk.position[0] - expected[0]).abs() < 0.05 && (chunk.position[1] - expected[1]).abs() < 0.05
    })
}

/// How much the normals of `tile` agree with the slope of its heights, in X and in Y: the sums over
/// its outer vertices of each component times the slope down that axis, positive when they agree.
fn slopes(tile: &Tile) -> [f64; 2] {
    let step = f64::from(CHUNK) / 8.0;
    let mut sums = [0.0; 2];
    for chunk in &tile.chunks {
        let height = |row: usize, column: usize| f64::from(chunk.heights[row * 17 + column]);
        for row in 0..8 {
            for column in 0..8 {
                let normal = chunk.normals[row * 17 + column];
                // The next row is lower in X, the next column lower in Y.
                let down_x = -(height(row, column) - height(row + 1, column)) / step;
                let down_y = -(height(row, column) - height(row, column + 1)) / step;
                sums[0] += f64::from(normal[0]) * down_x;
                sums[1] += f64::from(normal[1]) * down_y;
            }
        }
    }
    sums
}

/// Whether `tile` holds together: its chunks in their order, their parts whole, what they name
/// within the tile. A description of what does not, else none.
fn check(tile: &Tile) -> Option<String> {
    if tile.chunks.len() != 256 {
        return Some(format!("{} chunks", tile.chunks.len()));
    }
    for (place, chunk) in tile.chunks.iter().enumerate() {
        let index = [place as u32 % 16, place as u32 / 16];
        let at = format!("chunk {index:?}");
        if chunk.index != index {
            return Some(format!("{at}: index {:?}", chunk.index));
        }
        if chunk.heights.len() != 145 || chunk.heights.iter().any(|h| !h.is_finite()) {
            return Some(format!("{at}: heights"));
        }
        if chunk.normals.len() != 145 || !(chunk.colours.is_empty() || chunk.colours.len() == 145) {
            return Some(format!("{at}: normals or colours"));
        }
        if chunk.layers.len() > 4 || chunk.alphas.len() != chunk.layers.len().saturating_sub(1) {
            return Some(format!(
                "{at}: {} layers, {} alpha maps",
                chunk.layers.len(),
                chunk.alphas.len()
            ));
        }
        if chunk.alphas.iter().any(|map| map.len() != 4096) || !(chunk.shadow.is_empty() || chunk.shadow.len() == 4096)
        {
            return Some(format!("{at}: maps"));
        }
        if let Some(layer) = chunk
            .layers
            .iter()
            .find(|layer| layer.texture as usize >= tile.textures.len())
        {
            return Some(format!("{at}: texture {} of {}", layer.texture, tile.textures.len()));
        }
        if chunk.doodad_refs.iter().any(|r| *r as usize >= tile.doodads.len())
            || chunk.building_refs.iter().any(|r| *r as usize >= tile.buildings.len())
        {
            return Some(format!("{at}: references"));
        }
    }
    None
}

#[test]
fn the_client_s_tiles_read_whole_and_hold_together() {
    let Some(client) = client() else { return };
    let started = Instant::now();
    let read = AtomicUsize::new(0);
    let textures = Mutex::new(HashSet::new());
    let misplaced = Mutex::new(Vec::new());
    let agreement = Mutex::new([0.0f64; 2]);
    let (normals_up, normals) = (AtomicUsize::new(0), AtomicUsize::new(0));
    // The microseconds spent reading the tiles whole and their placements alone.
    let (whole, alone) = (AtomicUsize::new(0), AtomicUsize::new(0));
    for map in ["Azeroth", "Kalimdor", "Expansion01", "Northrend"] {
        let wdt = client.wdt(map).unwrap();
        let tiles: Vec<(u32, u32)> = (0..4096u32)
            .filter(|index| wdt.tiles[*index as usize])
            .map(|index| (index % 64, index / 64))
            .collect();
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    while let Some((x, y)) = tiles.get(next.fetch_add(1, Ordering::Relaxed)).copied() {
                        let reading = Instant::now();
                        let tile = client
                            .tile(map, x, y)
                            .unwrap_or_else(|e| panic!("{map} {x} {y}: {e}"))
                            .unwrap_or_else(|| panic!("{map} {x} {y}: none"));
                        whole.fetch_add(reading.elapsed().as_micros() as usize, Ordering::Relaxed);
                        let reading = Instant::now();
                        let placements = client.placements(map, x, y).unwrap().unwrap();
                        alone.fetch_add(reading.elapsed().as_micros() as usize, Ordering::Relaxed);
                        assert!(
                            placements.doodads == tile.doodads && placements.buildings == tile.buildings,
                            "{map}_{x}_{y}: the placements read alone"
                        );
                        if let Some(fault) = check(&tile) {
                            panic!("{map}_{x}_{y}: {fault}");
                        }
                        let [x_sum, y_sum] = slopes(&tile);
                        let mut sums = agreement.lock().unwrap();
                        sums[0] += x_sum;
                        sums[1] += y_sum;
                        drop(sums);
                        if !placed(&tile, x, y) {
                            misplaced.lock().unwrap().push(format!("{map}_{x}_{y}"));
                        }
                        let up = tile
                            .chunks
                            .iter()
                            .flat_map(|chunk| &chunk.normals)
                            .filter(|n| n[2] > 0)
                            .count();
                        normals_up.fetch_add(up, Ordering::Relaxed);
                        normals.fetch_add(256 * 145, Ordering::Relaxed);
                        if map == "Azeroth" {
                            textures.lock().unwrap().extend(tile.textures.iter().cloned());
                        }
                        read.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
    }
    let elapsed = started.elapsed();
    let up = normals_up.load(Ordering::Relaxed) as f64 / normals.load(Ordering::Relaxed) as f64;
    assert!(up > 0.99, "{up} of the normals point up");
    let agreement = agreement.into_inner().unwrap();
    assert!(
        agreement[0] > 0.0 && agreement[1] > 0.0,
        "normals against slopes: {agreement:?}"
    );
    let textures = textures.into_inner().unwrap();
    let started = Instant::now();
    let mut decoded = 0;
    let mut refused = Vec::new();
    for file in &textures {
        let raw = match client.texture(file, false) {
            Ok(raw) => raw,
            Err(reason) => {
                assert!(reason.contains("BLP1, which the client"), "{file:?}: {reason}");
                refused.push(reason);
                continue;
            }
        };
        let rgba = client.texture(file, true).unwrap();
        assert_eq!(raw.levels.len(), rgba.levels.len(), "{file:?}");
        assert_eq!(rgba.format, TextureFormat::Rgba8);
        assert_eq!(
            rgba.levels[0].len(),
            (rgba.width * rgba.height * 4) as usize,
            "{file:?}"
        );
        assert!(matches!(file, FileRef::Path(_)));
        decoded += 1;
    }
    let misplaced = misplaced.into_inner().unwrap();
    eprintln!(
        "{} tiles read in {elapsed:?}, {:.1} % of the normals up; {} whose chunks give another position \
         than their place: {misplaced:?}; {decoded} textures of Azeroth decoded in {:?}, refused {refused:?}; \
         a tile read whole in {:.2} ms on average, its placements alone in {:.2} ms",
        read.load(Ordering::Relaxed),
        up * 100.0,
        misplaced.len(),
        started.elapsed(),
        whole.load(Ordering::Relaxed) as f64 / 1e3 / read.load(Ordering::Relaxed) as f64,
        alone.load(Ordering::Relaxed) as f64 / 1e3 / read.load(Ordering::Relaxed) as f64
    );
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

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn put_at(out: &mut [u8], at: usize, bytes: &[u8]) {
    out[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A WDT of `flags` whose tiles are `tiles`, by (x, y), with `tail` bytes after its last chunk.
fn write_wdt(flags: u32, tiles: &[(usize, usize)], tail: usize) -> Vec<u8> {
    let mut main = vec![0u8; 64 * 64 * 8];
    for (x, y) in tiles {
        main[(y * 64 + x) * 8] = 1;
    }
    let mut out = tagged(b"MVER", &words(&[18]));
    out.extend(tagged(b"MPHD", &words(&[flags, 0, 0, 0, 0, 0, 0, 0])));
    out.extend(tagged(b"MAIN", &main));
    out.extend(tagged(b"MWMO", &[]));
    out.extend(vec![0u8; tail]);
    out
}

/// How a test tile stores its alpha maps.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Alphas {
    /// 4 bits a texel, the last row and column copied by the reader.
    Fixed,
    /// 4 bits a texel, kept as they are.
    Unfixed,
    /// Compressed in runs.
    Compressed,
    /// 8 bits a texel, as the WDT says.
    Big,
}

/// What a test chunk holds, as the reader gives it and as the file stores it, made from its place.
struct Made {
    flags: u32,
    heights: Vec<f32>,
    normals: Vec<[i8; 3]>,
    /// Red first; the file stores blue first.
    colours: Vec<[u8; 4]>,
    /// Each layer: its texture, flags, effect, and offset into the stored maps.
    layers: Vec<[u32; 4]>,
    alphas: Vec<Vec<u8>>,
    stored_alphas: Vec<u8>,
    shadow: Vec<u8>,
    stored_shadow: Vec<u8>,
    holes: u64,
    doodad_refs: Vec<u32>,
    building_refs: Vec<u32>,
}

/// Copies the row and the column before the last into the last ones.
fn fixed(mut map: Vec<u8>) -> Vec<u8> {
    for row in 0..64 {
        map[row * 64 + 63] = map[row * 64 + 62];
    }
    for column in 0..64 {
        map[63 * 64 + column] = map[62 * 64 + column];
    }
    map
}

/// The holes of the 4 × 4 bits 1 (row 0, column 1) and 14 (row 3, column 2), as the reader gives
/// them: two squares of 2 × 2 quads.
const LOW_HOLES: u16 = 0b0100_0000_0000_0010;
const HOLES: u64 = 1 << 2 | 1 << 3 | 1 << 10 | 1 << 11 | 1 << 52 | 1 << 53 | 1 << 60 | 1 << 61;

fn made(place: usize, alphas: Alphas) -> Made {
    let fix = alphas == Alphas::Fixed;
    // Every sixth chunk has no shadow; every seventh has holes.
    let shadowed = place % 6 != 5;
    let mut flags = 0x40;
    if shadowed {
        flags |= 0x1;
    }
    if !fix {
        flags |= 0x8000;
    }
    let mut layers = Vec::new();
    let mut stored_alphas = Vec::new();
    let mut maps = Vec::new();
    for index in 0..place % 5 {
        let mut layer_flags = (index as u32) << 8 & 0x100;
        let offset = stored_alphas.len() as u32;
        if index > 0 {
            let seed = place + index;
            let (stored, map) = match alphas {
                Alphas::Fixed | Alphas::Unfixed => {
                    let stored: Vec<u8> = (0..2048).map(|i| ((i * 7 + seed) % 256) as u8).collect();
                    let map: Vec<u8> = stored.iter().flat_map(|b| [(b & 0x0F) * 17, (b >> 4) * 17]).collect();
                    (stored, if fix { fixed(map) } else { map })
                }
                Alphas::Compressed => {
                    layer_flags |= 0x200;
                    let mut map = vec![(seed % 256) as u8; 100];
                    let mut stored = vec![0x80 | 100, (seed % 256) as u8];
                    while map.len() < 4096 {
                        let count = (4096 - map.len()).min(127);
                        stored.push(count as u8);
                        let values: Vec<u8> = (0..count).map(|i| ((map.len() + i + seed) % 251) as u8).collect();
                        stored.extend(&values);
                        map.extend(values);
                    }
                    (stored, map)
                }
                Alphas::Big => {
                    let map: Vec<u8> = (0..4096).map(|i| ((i + seed) % 256) as u8).collect();
                    (map.clone(), map)
                }
            };
            stored_alphas.extend(stored);
            maps.push(map);
        }
        layers.push([(place + index) as u32 % 3, layer_flags, index as u32, offset]);
    }
    let stored_shadow: Vec<u8> = (0..512).map(|i| ((i * 13 + place) % 256) as u8).collect();
    let shadow: Vec<u8> = stored_shadow
        .iter()
        .flat_map(|byte| (0..8).map(move |bit| if byte & (1 << bit) != 0 { 255 } else { 0 }))
        .collect();
    Made {
        flags,
        heights: (0..145).map(|i| (place * 145 + i) as f32 * 0.25).collect(),
        normals: (0..145)
            .map(|i| [(i % 50) as i8 - 25, (place % 50) as i8 - 25, 120])
            .collect(),
        colours: (0..145).map(|i| [i as u8, place as u8, 127, 255]).collect(),
        layers,
        alphas: maps,
        stored_alphas,
        shadow: match (shadowed, fix) {
            (false, _) => Vec::new(),
            (true, true) => fixed(shadow),
            (true, false) => shadow,
        },
        stored_shadow,
        holes: if place.is_multiple_of(7) { HOLES } else { 0 },
        doodad_refs: if place.is_multiple_of(3) {
            vec![0, 1]
        } else {
            Vec::new()
        },
        building_refs: if place.is_multiple_of(4) { vec![0] } else { Vec::new() },
    }
}

const TILE_X: usize = 32;
const TILE_Y: usize = 48;

fn corner(place: usize) -> [f32; 3] {
    [
        ORIGIN - TILE * TILE_Y as f32 - CHUNK * (place / 16) as f32,
        ORIGIN - TILE * TILE_X as f32 - CHUNK * (place % 16) as f32,
        place as f32,
    ]
}

/// The header of a test chunk; its offsets are set by the writer of 3.3.5a.
fn header(place: usize, made: &Made) -> Vec<u8> {
    let mut header = vec![0u8; 128];
    put_at(&mut header, 0x00, &made.flags.to_le_bytes());
    put_at(
        &mut header,
        0x04,
        &words(&[(place % 16) as u32, (place / 16) as u32, made.layers.len() as u32]),
    );
    put_at(&mut header, 0x10, &(made.doodad_refs.len() as u32).to_le_bytes());
    put_at(
        &mut header,
        0x34,
        &words(&[place as u32 + 1000, made.building_refs.len() as u32]),
    );
    put_at(&mut header, 0x68, &floats(&corner(place)));
    header
}

fn layer_bytes(made: &Made) -> Vec<u8> {
    made.layers
        .iter()
        .flat_map(|l| words(&[l[0], l[1], l[3], l[2]]))
        .collect()
}

fn colour_bytes(made: &Made) -> Vec<u8> {
    made.colours.iter().flat_map(|c| [c[2], c[1], c[0], c[3]]).collect()
}

fn normal_bytes(made: &Made) -> Vec<u8> {
    made.normals.iter().flat_map(|n| n.map(|v| v as u8)).collect()
}

/// The names of a block of strings, and the offset of each.
fn string_block(names: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut block = Vec::new();
    let mut offsets = Vec::new();
    for name in names {
        offsets.push(block.len() as u32);
        block.extend(name.as_bytes());
        block.push(0);
    }
    (block, offsets)
}

/// The placements of the test tiles: a doodad by name and one by FileDataID, a building by name.
fn placements() -> Vec<u8> {
    let (models, model_offsets) = string_block(&["world\\a.m2", "world\\b.m2"]);
    let (buildings, building_offsets) = string_block(&["world\\c.wmo"]);
    let mut doodads = Vec::new();
    for (name, unique, flags) in [(1u32, 7u32, 0u16), (424_242, 8, 0x40)] {
        doodads.extend(words(&[name, unique]));
        doodads.extend(floats(&[1.0, 2.0, 3.0, 0.0, 90.0, 0.0]));
        doodads.extend(2048u16.to_le_bytes());
        doodads.extend(flags.to_le_bytes());
    }
    let mut building = words(&[0, 9]);
    building.extend(floats(&[
        4.0, 5.0, 6.0, 0.0, 45.0, 0.0, -1.0, -2.0, -3.0, 1.0, 2.0, 3.0,
    ]));
    for value in [0x4u16, 1, 2, 512] {
        building.extend(value.to_le_bytes());
    }
    let mut out = tagged(b"MMDX", &models);
    out.extend(tagged(b"MMID", &words(&model_offsets)));
    out.extend(tagged(b"MWMO", &buildings));
    out.extend(tagged(b"MWID", &words(&building_offsets)));
    out.extend(tagged(b"MDDF", &doodads));
    out.extend(tagged(b"MODF", &building));
    out
}

/// A tile of 3.3.5a, its parts found by offsets, its alpha maps declared one byte long when
/// `short_alphas`, as some of the client are; and what each chunk holds.
fn write_adt(alphas: Alphas, short_alphas: bool) -> (Vec<u8>, Vec<Made>) {
    let mut out = tagged(b"MVER", &words(&[18]));
    out.extend(tagged(b"MHDR", &[0u8; 64]));
    out.extend(tagged(
        b"MTEX",
        &string_block(&["tileset\\a.blp", "tileset\\b.blp", "tileset\\c.blp"]).0,
    ));
    out.extend(placements());
    let mut chunks = Vec::new();
    for place in 0..256 {
        let made = made(place, alphas);
        let mut data = header(place, &made);
        // The offsets count from the chunk's header, 8 bytes before its data.
        let part = |data: &mut Vec<u8>, offset_at: usize, name: &[u8; 4], bytes: &[u8]| {
            let offset = data.len() as u32 + 8;
            put_at(data, offset_at, &offset.to_le_bytes());
            data.extend(tagged(name, bytes));
        };
        part(&mut data, 0x14, b"MCVT", &floats(&made.heights));
        part(&mut data, 0x18, b"MCNR", &normal_bytes(&made));
        // The normals of 3.3.5a are followed by 13 bytes their size does not count.
        data.extend([0u8; 13]);
        part(&mut data, 0x1C, b"MCLY", &layer_bytes(&made));
        let refs: Vec<u32> = made.doodad_refs.iter().chain(&made.building_refs).copied().collect();
        part(&mut data, 0x20, b"MCRF", &words(&refs));
        part(&mut data, 0x74, b"MCCV", &colour_bytes(&made));
        if made.flags & 0x1 != 0 {
            part(&mut data, 0x2C, b"MCSH", &made.stored_shadow);
        } else {
            // An offset of shadow the flags do not ask for, pointing at the heights.
            let heights = u32::from_le_bytes(data[0x14..0x18].try_into().unwrap());
            put_at(&mut data, 0x2C, &heights.to_le_bytes());
        }
        part(&mut data, 0x24, b"MCAL", &made.stored_alphas);
        if short_alphas && !made.stored_alphas.is_empty() {
            let size_at = data.len() - made.stored_alphas.len() - 4;
            put_at(&mut data, size_at, &1u32.to_le_bytes());
        }
        if made.holes != 0 {
            put_at(&mut data, 0x3C, &LOW_HOLES.to_le_bytes());
        }
        out.extend(tagged(b"MCNK", &data));
        chunks.push(made);
    }
    (out, chunks)
}

/// A split tile: its root, `_tex0` and `_obj0`; the textures by FileDataID when `ids`; holes of
/// high resolution in the chunks that have holes; and what each chunk holds.
fn write_split(alphas: Alphas, ids: bool) -> ([Vec<u8>; 3], Vec<Made>) {
    let mut root = tagged(b"MVER", &words(&[18]));
    root.extend(tagged(b"MHDR", &[0u8; 64]));
    let mut tex = tagged(b"MVER", &words(&[18]));
    if ids {
        tex.extend(tagged(b"MDID", &words(&[1001, 1002, 1003])));
    } else {
        tex.extend(tagged(
            b"MTEX",
            &string_block(&["tileset\\a.blp", "tileset\\b.blp", "tileset\\c.blp"]).0,
        ));
    }
    let mut obj = tagged(b"MVER", &words(&[18]));
    obj.extend(placements());
    let mut chunks = Vec::new();
    for place in 0..256 {
        let mut made = made(place, alphas);
        if made.holes != 0 {
            made.flags |= 0x10000;
            made.holes = 0x0123_4567_89AB_CDEF;
        }
        let mut data = header(place, &made);
        if made.holes != 0 {
            put_at(&mut data, 0x14, &made.holes.to_le_bytes());
        }
        data.extend(tagged(b"MCVT", &floats(&made.heights)));
        data.extend(tagged(b"MCCV", &colour_bytes(&made)));
        data.extend(tagged(b"MCNR", &normal_bytes(&made)));
        root.extend(tagged(b"MCNK", &data));
        let mut texture = tagged(b"MCLY", &layer_bytes(&made));
        if made.flags & 0x1 != 0 {
            texture.extend(tagged(b"MCSH", &made.stored_shadow));
        }
        texture.extend(tagged(b"MCAL", &made.stored_alphas));
        tex.extend(tagged(b"MCNK", &texture));
        let mut object = tagged(b"MCRD", &words(&made.doodad_refs));
        object.extend(tagged(b"MCRW", &words(&made.building_refs)));
        obj.extend(tagged(b"MCNK", &object));
        chunks.push(made);
    }
    ([root, tex, obj], chunks)
}

/// Whether `tile` holds what the test chunks `made` and the placements hold.
fn same(tile: &Tile, made: &[Made], textures: &[FileRef]) {
    assert_eq!(tile.textures, textures);
    assert_eq!(tile.doodads.len(), 2);
    assert_eq!(tile.doodads[0].file, FileRef::Path("world\\b.m2".to_owned()));
    assert_eq!(tile.doodads[1].file, FileRef::Id(424_242), "a doodad by FileDataID");
    assert_eq!(
        (
            tile.doodads[0].unique_id,
            tile.doodads[0].position,
            tile.doodads[0].rotation,
            tile.doodads[0].scale
        ),
        (7, [1.0, 2.0, 3.0], [0.0, 90.0, 0.0], 2.0)
    );
    let building = &tile.buildings[0];
    assert_eq!(building.file, FileRef::Path("world\\c.wmo".to_owned()));
    assert_eq!(
        (building.unique_id, building.position, building.bounds, building.scale),
        (9, [4.0, 5.0, 6.0], [[-1.0, -2.0, -3.0], [1.0, 2.0, 3.0]], 0.5)
    );
    assert_eq!((building.flags, building.doodad_set, building.name_set), (0x4, 1, 2));
    assert_eq!(tile.chunks.len(), 256);
    for (place, (chunk, made)) in tile.chunks.iter().zip(made).enumerate() {
        let at = format!("chunk {place}");
        assert_eq!(chunk.index, [(place % 16) as u32, (place / 16) as u32], "{at}");
        assert_eq!(
            (chunk.flags, chunk.position, chunk.area),
            (made.flags, corner(place), place as u32 + 1000),
            "{at}"
        );
        assert_eq!(chunk.heights, made.heights, "{at}");
        assert_eq!(chunk.normals, made.normals, "{at}");
        assert_eq!(chunk.colours, made.colours, "{at}");
        assert_eq!(chunk.holes, made.holes, "{at}");
        let layers: Vec<[u32; 3]> = chunk.layers.iter().map(|l| [l.texture, l.flags, l.effect]).collect();
        let expected: Vec<[u32; 3]> = made.layers.iter().map(|l| [l[0], l[1], l[2]]).collect();
        assert_eq!(layers, expected, "{at}");
        assert_eq!(chunk.alphas, made.alphas, "{at}");
        assert_eq!(chunk.shadow, made.shadow, "{at}");
        assert_eq!(
            (&chunk.doodad_refs, &chunk.building_refs),
            (&made.doodad_refs, &made.building_refs),
            "{at}"
        );
    }
}

fn paths(names: &[&str]) -> Vec<FileRef> {
    names.iter().map(|name| FileRef::Path(name.to_string())).collect()
}

#[test]
fn a_wdt_names_its_tiles_by_their_place_and_its_flags() {
    let wdt = terrain::wdt(&write_wdt(0x4, &[(32, 48), (0, 63)], 3)).unwrap();
    assert_eq!(wdt.flags, 0x4);
    assert!(wdt.tiles[48 * 64 + 32] && wdt.tiles[63 * 64]);
    assert_eq!(wdt.tiles.iter().filter(|tile| **tile).count(), 2);
    assert!(terrain::wdt(&tagged(b"MVER", &words(&[18]))).is_err(), "no MAIN");
}

#[test]
fn a_tile_of_3_3_5a_reads_its_chunks_by_their_offsets_in_every_way_of_storing_alpha() {
    let textures = paths(&["tileset\\a.blp", "tileset\\b.blp", "tileset\\c.blp"]);
    for (alphas, wdt_flags) in [
        (Alphas::Fixed, 0),
        (Alphas::Unfixed, 0),
        (Alphas::Compressed, 0),
        (Alphas::Big, 0x4),
    ] {
        let (bytes, made) = write_adt(alphas, false);
        let tile = terrain::tile(&bytes, None, None, wdt_flags).unwrap_or_else(|e| panic!("{alphas:?}: {e}"));
        same(&tile, &made, &textures);
    }
    let (bytes, made) = write_adt(Alphas::Compressed, true);
    same(&terrain::tile(&bytes, None, None, 0).unwrap(), &made, &textures);
}

#[test]
fn a_split_tile_reads_its_root_tex0_and_obj0_as_warcraftxl_loads_them() {
    let names = paths(&["tileset\\a.blp", "tileset\\b.blp", "tileset\\c.blp"]);
    for (alphas, wdt_flags, ids) in [
        (Alphas::Big, 0x80, false),
        (Alphas::Compressed, 0, true),
        (Alphas::Unfixed, 0, false),
    ] {
        let ([root, tex, obj], made) = write_split(alphas, ids);
        let tile =
            terrain::tile(&root, Some(&tex), Some(&obj), wdt_flags).unwrap_or_else(|e| panic!("{alphas:?}: {e}"));
        let textures = if ids {
            vec![FileRef::Id(1001), FileRef::Id(1002), FileRef::Id(1003)]
        } else {
            names.clone()
        };
        same(&tile, &made, &textures);
    }
}

#[test]
fn the_placements_of_a_tile_are_its_doodads_and_buildings_read_alone() {
    let (monolithic, _) = write_adt(Alphas::Compressed, false);
    let ([root, tex, obj], _) = write_split(Alphas::Compressed, true);
    for (placements, tile) in [
        (
            terrain::placements(&monolithic, None).unwrap(),
            terrain::tile(&monolithic, None, None, 0).unwrap(),
        ),
        (
            terrain::placements(&[], Some(&obj)).unwrap(),
            terrain::tile(&root, Some(&tex), Some(&obj), 0).unwrap(),
        ),
    ] {
        assert_eq!(placements.doodads.len(), 2);
        assert_eq!(
            (placements.doodads, placements.buildings),
            (tile.doodads, tile.buildings)
        );
    }
}

#[test]
fn a_tile_damaged_is_refused_and_never_panics() {
    let (bytes, _) = write_adt(Alphas::Compressed, false);
    assert!(terrain::tile(&bytes[..bytes.len() / 2], None, None, 0).is_err());
    let ([root, tex, obj], _) = write_split(Alphas::Big, false);
    let mut seed = 0x1234_5678u32;
    for sample in [&bytes, &root, &tex, &obj] {
        for _ in 0..300 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let mut damaged = sample.clone();
            let at = seed as usize % damaged.len();
            damaged[at] ^= 0xFF;
            let cut = &damaged[..damaged.len() - (seed as usize >> 8) % 64];
            let _ = terrain::tile(cut, None, None, 0);
            let _ = terrain::tile(&root, Some(cut), Some(&obj), 0x4);
            let _ = terrain::tile(&root, Some(&tex), Some(cut), 0x4);
        }
    }
}

/// A BLP of version 2: its encoding, depth and encoding of alpha, size, levels, and palette.
fn write_blp(
    encoding: u8,
    alpha_depth: u8,
    alpha_encoding: u8,
    size: [u32; 2],
    levels: &[Vec<u8>],
    palette: &[u8],
) -> Vec<u8> {
    let mut out = b"BLP2".to_vec();
    out.extend(words(&[1]));
    out.extend([encoding, alpha_depth, alpha_encoding, u8::from(levels.len() > 1)]);
    out.extend(words(&size));
    let mut at = 148 + palette.len();
    let mut offsets = [0u32; 16];
    let mut sizes = [0u32; 16];
    for (index, level) in levels.iter().enumerate() {
        offsets[index] = at as u32;
        sizes[index] = level.len() as u32;
        at += level.len();
    }
    out.extend(words(&offsets));
    out.extend(words(&sizes));
    out.extend(palette);
    for level in levels {
        out.extend(level);
    }
    out
}

/// A palette whose entry `i`, stored blue, green, red, is `[i, 2i, 3i]` in red, green, blue.
fn palette() -> Vec<u8> {
    (0..256u32)
        .flat_map(|i| [(3 * i) as u8, (2 * i) as u8, i as u8, 0])
        .collect()
}

#[test]
fn a_blp_of_a_palette_takes_its_alpha_of_each_depth() {
    let indices = [1u8, 2, 3, 4, 5, 6, 7, 8];
    for (depth, alphas, expected) in [
        (0u8, vec![], [255u8; 8]),
        (1, vec![0b1010_0101], [255, 0, 255, 0, 0, 255, 0, 255]),
        (
            4,
            vec![0x21, 0x43, 0x65, 0x87],
            [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        ),
        (8, vec![9, 8, 7, 6, 5, 4, 3, 2], [9, 8, 7, 6, 5, 4, 3, 2]),
    ] {
        let mut level = indices.to_vec();
        level.extend(alphas);
        let texture = blp::texture(&write_blp(1, depth, 0, [4, 2], &[level], &palette()), false).unwrap();
        assert_eq!(
            (texture.width, texture.height, texture.format),
            (4, 2, TextureFormat::Rgba8)
        );
        let rgba: Vec<u8> = indices
            .iter()
            .zip(expected)
            .flat_map(|(i, a)| [*i, i * 2, i * 3, a])
            .collect();
        assert_eq!(texture.levels[0], rgba, "alpha of {depth} bits");
    }
}

/// A block of DXT1 of `c0` and `c1`, each row of texels taking the colours 0 to 3 in turn.
fn dxt1_block(c0: u16, c1: u16) -> Vec<u8> {
    let mut block = c0.to_le_bytes().to_vec();
    block.extend(c1.to_le_bytes());
    block.extend([0b1110_0100; 4]);
    block
}

#[test]
fn dxt_is_kept_as_bc_or_decoded_as_wow_export_decodes_it() {
    // Red then green: four colours, the two between them a third and two thirds of the way.
    let block = dxt1_block(0xF800, 0x07E0);
    let blp = write_blp(2, 0, 0, [4, 4], std::slice::from_ref(&block), &[]);
    let kept = blp::texture(&blp, false).unwrap();
    assert_eq!((kept.format, &kept.levels[0]), (TextureFormat::Bc1, &block));
    let decoded = blp::texture(&blp, true).unwrap();
    let row = [[255, 0, 0, 255], [0, 255, 0, 255], [170, 85, 0, 255], [85, 170, 0, 255]];
    assert_eq!(decoded.levels[0], row.concat().repeat(4));
    // Green then red: three colours and a transparent black.
    let three = blp::texture(&write_blp(2, 1, 0, [4, 4], &[dxt1_block(0x07E0, 0xF800)], &[]), true).unwrap();
    let row = [[0, 255, 0, 255], [255, 0, 0, 255], [127, 127, 0, 255], [0, 0, 0, 0]];
    assert_eq!(three.levels[0], row.concat().repeat(4));

    // DXT3: an alpha of 4 bits a texel before the colours.
    let mut dxt3: Vec<u8> = (0..8u8).map(|j| j | (15 - j) << 4).collect();
    dxt3.extend(dxt1_block(0xF800, 0x07E0));
    let texture = blp::texture(&write_blp(2, 8, 1, [4, 4], &[dxt3.clone()], &[]), true).unwrap();
    let alphas: Vec<u8> = texture.levels[0].iter().skip(3).step_by(4).copied().collect();
    let expected: Vec<u8> = (0..8u8).flat_map(|j| [j * 17, (15 - j) * 17]).collect();
    assert_eq!(alphas, expected);
    assert_eq!(
        blp::texture(&write_blp(2, 8, 1, [4, 4], &[dxt3], &[]), false)
            .unwrap()
            .format,
        TextureFormat::Bc2
    );

    // DXT5: two alphas and the ones between them, texel `i` taking the index `i % 8`.
    for (a0, a1, between) in [
        (200u8, 100u8, vec![185u8, 171, 157, 142, 128, 114]),
        (100, 200, vec![120, 140, 160, 180, 0, 255]),
    ] {
        let mut bits = 0u64;
        for i in 0..16 {
            bits |= (i as u64 % 8) << (3 * i);
        }
        let mut dxt5 = vec![a0, a1];
        dxt5.extend(&bits.to_le_bytes()[..6]);
        dxt5.extend(dxt1_block(0xF800, 0x07E0));
        let texture = blp::texture(&write_blp(2, 8, 7, [4, 4], &[dxt5], &[]), true).unwrap();
        let palette: Vec<u8> = [a0, a1].into_iter().chain(between).collect();
        let alphas: Vec<u8> = texture.levels[0].iter().skip(3).step_by(4).copied().collect();
        let expected: Vec<u8> = (0..16).map(|i| palette[i % 8]).collect();
        assert_eq!(alphas, expected, "alphas {a0} and {a1}");
    }
}

#[test]
fn the_levels_of_a_blp_end_at_the_first_cut_short_and_the_older_versions_are_refused() {
    let levels: Vec<Vec<u8>> = [16usize, 4, 1, 1]
        .iter()
        .map(|blocks| dxt1_block(0xF800, 0x07E0).repeat(*blocks))
        .collect();
    let texture = blp::texture(&write_blp(2, 0, 0, [16, 16], &levels, &[]), false).unwrap();
    assert_eq!(
        texture.levels.len(),
        4,
        "16, 8, 4 and 2 texels wide... down to the blocks given"
    );
    let mut short = levels.clone();
    short[2].truncate(4);
    let texture = blp::texture(&write_blp(2, 0, 0, [16, 16], &short, &[]), false).unwrap();
    assert_eq!(texture.levels.len(), 2, "a level cut short ends the levels");
    let bgra = blp::texture(&write_blp(3, 8, 0, [1, 1], &[vec![1, 2, 3, 4]], &[]), false).unwrap();
    assert_eq!(bgra.levels, vec![vec![3, 2, 1, 4]]);
    let mut old = write_blp(2, 0, 0, [4, 4], &[dxt1_block(0, 0)], &[]);
    old[3] = b'1';
    let refused = blp::texture(&old, false).err().unwrap();
    assert!(refused.contains("BLP1, which the client"), "{refused}");
    assert!(blp::texture(b"BLP2", false).is_err());
    assert!(blp::texture(&[0u8; 200], false).is_err());
}

#[test]
fn the_service_reads_the_tiles_its_wdt_names_split_when_their_tex0_exists() {
    let folder = scratch("terrain");
    let wdt = write_wdt(0, &[(32, 48), (33, 48)], 0);
    let (monolithic, made) = write_adt(Alphas::Unfixed, false);
    let ([root, tex, obj], split_made) = write_split(Alphas::Unfixed, true);
    let texture = write_blp(2, 0, 0, [4, 4], &[dxt1_block(0xF800, 0x07E0)], &[]);
    let archive = folder.join("common.mpq");
    let maps = "World\\Maps\\Test\\Test";
    let names = [
        format!("{maps}.wdt"),
        format!("{maps}_32_48.adt"),
        format!("{maps}_32_48_obj0.adt"),
        format!("{maps}_33_48.adt"),
        format!("{maps}_33_48_tex0.adt"),
        format!("{maps}_33_48_obj0.adt"),
        format!("{maps}_34_48.adt"),
        "tileset\\a.blp".to_owned(),
    ];
    // An `_obj0` without its `_tex0`, unread as the whole tile leaves it.
    let orphan = tagged(b"MVER", &words(&[18]));
    let contents = [&wdt, &monolithic, &orphan, &root, &tex, &obj, &monolithic, &texture];
    let files: Vec<_> = names
        .iter()
        .zip(contents)
        .map(|(name, bytes)| file(name, bytes, Stored::Plain))
        .collect();
    write_archive(&archive, 0, &files);
    let (client, _) = Client::open(vec![Source::open(&archive).unwrap()], &folder, "enUS");

    let first = client.wdt("Test").unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&first, &client.wdt("test").unwrap()),
        "read once"
    );
    let paths = paths(&["tileset\\a.blp", "tileset\\b.blp", "tileset\\c.blp"]);
    same(&client.tile("Test", 32, 48).unwrap().unwrap(), &made, &paths);
    let ids = vec![FileRef::Id(1001), FileRef::Id(1002), FileRef::Id(1003)];
    same(&client.tile("Test", 33, 48).unwrap().unwrap(), &split_made, &ids);
    assert_eq!(
        client.tile("Test", 34, 48).unwrap(),
        None,
        "a tile its WDT does not name"
    );
    assert_eq!(client.tile("Test", 64, 0).unwrap(), None);
    for x in [32, 33] {
        let placements = client.placements("Test", x, 48).unwrap().unwrap();
        let tile = client.tile("Test", x, 48).unwrap().unwrap();
        assert_eq!(placements.doodads.len(), 2, "tile {x}");
        assert_eq!(
            (placements.doodads, placements.buildings),
            (tile.doodads, tile.buildings)
        );
    }
    assert_eq!(client.placements("Test", 34, 48).unwrap(), None);
    assert_eq!(client.placements("Test", 64, 0).unwrap(), None);
    assert!(
        client
            .tile("Missing", 0, 0)
            .err()
            .unwrap()
            .contains("not in the client")
    );

    let kept = client
        .texture(&FileRef::Path("Tileset\\A.blp".to_owned()), false)
        .unwrap();
    assert_eq!(kept.format, TextureFormat::Bc1);
    let unnamed = client.texture(&FileRef::Id(1001), false).err().unwrap();
    assert!(unnamed.contains("named by no table of paths"), "{unnamed}");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_wdl_of_the_client_follows_the_heights_of_its_tiles_row_by_row() {
    let Some(client) = client() else {
        return;
    };
    let wdt = client.wdt("Azeroth").unwrap();
    let wdl = client.wdl("Azeroth").unwrap().expect("Azeroth has a WDL");
    // The mean distance between the heights of the WDL and those of the tiles, the rows of the WDL
    // taken as those of the chunks (going down in X), or as their columns.
    let mut errors = [0.0f64; 2];
    let mut count = 0;
    let tiles = (0..4096usize)
        .filter(|&i| wdt.tiles[i] && wdl.tiles[i].is_some())
        .step_by(40)
        .take(8);
    for index in tiles {
        let (x, y) = (index as u32 % 64, index as u32 / 64);
        let tile = client.tile("Azeroth", x, y).unwrap().unwrap();
        let heights = wdl.tiles[index].as_ref().unwrap();
        let height_at = |row: u32, column: u32| -> Option<f64> {
            // The outer vertex at the row and column of the tile, 128 of them a side.
            let (r, c) = (row.min(127), column.min(127));
            let chunk = tile.chunks.iter().find(|chunk| chunk.index == [c / 8, r / 8])?;
            let vertex = ((r % 8) + (row - r)) as usize * 17 + ((c % 8) + (column - c)) as usize;
            Some(f64::from(chunk.position[2] + chunk.heights[vertex]))
        };
        for row in 0..16u32 {
            for column in 0..16u32 {
                let wdl_height = f64::from(heights[(row * 17 + column) as usize]);
                if let (Some(as_rows), Some(as_columns)) =
                    (height_at(row * 8, column * 8), height_at(column * 8, row * 8))
                {
                    errors[0] += (wdl_height - as_rows).abs();
                    errors[1] += (wdl_height - as_columns).abs();
                    count += 1;
                }
            }
        }
    }
    let [rows, columns] = errors.map(|sum| sum / f64::from(count));
    eprintln!("{count} heights: {rows:.2} yards apart as rows, {columns:.2} as columns");
    assert!(count > 1000 && rows < 2.0 && columns > 3.0 * rows, "{rows} {columns}");
}

/// A WDL with the heights `tiles` give, at `y * 64 + x`, each of its heights its own place.
fn write_wdl(tiles: &[(usize, usize)]) -> Vec<u8> {
    let mut out = tagged(b"MVER", &words(&[18]));
    let maof_at = out.len() + 8;
    out.extend(tagged(b"MAOF", &vec![0u8; 64 * 64 * 4]));
    for (x, y) in tiles {
        let offset = out.len() as u32;
        out[maof_at + (y * 64 + x) * 4..][..4].copy_from_slice(&offset.to_le_bytes());
        let heights: Vec<u8> = (0..545i16).flat_map(|h| (h - 100).to_le_bytes()).collect();
        out.extend(tagged(b"MARE", &heights));
        out.extend(tagged(b"MAHO", &[0u8; 32]));
    }
    out
}

#[test]
fn a_wdl_gives_the_heights_of_its_tiles_and_none_elsewhere() {
    let wdl = terrain::wdl(&write_wdl(&[(32, 48), (5, 1)])).unwrap();
    assert_eq!(wdl.tiles.len(), 4096);
    let heights = wdl.tiles[48 * 64 + 32].as_ref().unwrap();
    assert_eq!(heights.len(), 17 * 17, "the heights between them left");
    assert_eq!((heights[0], heights[288]), (-100, 188));
    assert!(wdl.tiles[64 + 5].is_some());
    assert_eq!(wdl.tiles.iter().filter(|tile| tile.is_some()).count(), 2);

    let mut wrong = write_wdl(&[(1, 1)]);
    let at = u32::from_le_bytes(wrong[20 + (64 + 1) * 4..][..4].try_into().unwrap()) as usize;
    wrong[at] = b'X';
    let refused = terrain::wdl(&wrong).err().unwrap();
    assert!(refused.contains("tile 1 1"), "{refused}");
    assert!(terrain::wdl(&tagged(b"MVER", &words(&[18]))).is_err(), "no MAOF");
}
