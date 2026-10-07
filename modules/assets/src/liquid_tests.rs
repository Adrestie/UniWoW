//! Tests of the liquids of the tiles on files the tests write themselves; and, when `UNIWOW_CLIENT`
//! names the folder of a client, on its own maps.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use uniwow_api::formats::{CHUNK, LIQUID_SIDE, ORIGIN, TILE};

use crate::liquid;
use crate::terrain_tests::client;

/// A chunk named as the format writes it, reversed as the file holds it.
fn tagged(name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = vec![name[3], name[2], name[1], name[0]];
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn halves(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

/// A layer of `MH2O`: its type, format, heights, rectangle, and the offsets of its bitmap and of its
/// vertices.
fn instance(liquid: u16, format: u16, heights: [f32; 2], rectangle: [u8; 4], bitmap: u32, vertices: u32) -> Vec<u8> {
    let mut out = halves(&[liquid, format]);
    out.extend(floats(&heights));
    out.extend(rectangle);
    out.extend(bitmap.to_le_bytes());
    out.extend(vertices.to_le_bytes());
    out
}

/// An `MH2O`: in the chunk 0, a slow water of format 0 over the tiles (1, 2) and (2, 2) of its
/// rectangle, its bitmap leaving the second out; in the chunk 17, an ocean of format 2 without
/// vertices over the whole chunk, then a slow magma of format 1 over one tile.
fn mh2o() -> Vec<u8> {
    let mut data = vec![0u8; 256 * 12];
    let put = |data: &mut Vec<u8>, bytes: &[u8]| -> u32 {
        let at = data.len() as u32;
        data.extend(bytes);
        at
    };
    let water_heights = put(&mut data, &floats(&[10.0, 10.5, 11.0, 11.5, 12.0, 99.0]));
    put(&mut data, &[10, 20, 30, 40, 50, 60]);
    let bitmap = put(&mut data, &[0b01]);
    let water = instance(5, 0, [10.0, 12.0], [1, 2, 2, 1], bitmap, water_heights);
    let magma_heights = put(&mut data, &floats(&[20.0, 20.25, 20.5, 20.75]));
    put(&mut data, &halves(&[0, 0, 255, 0, 0, 510, 255, 510]));
    let ocean = instance(2, 2, [-1.0, 4.0], [0, 0, 8, 8], 0, 0);
    let magma = instance(7, 1, [20.0, 21.0], [0, 0, 1, 1], 0, magma_heights);
    let first = put(&mut data, &water);
    let second = put(&mut data, &[ocean, magma].concat());
    for (chunk, offset, count) in [(0usize, first, 1u32), (17, second, 2)] {
        data[chunk * 12..chunk * 12 + 4].copy_from_slice(&offset.to_le_bytes());
        data[chunk * 12 + 4..chunk * 12 + 8].copy_from_slice(&count.to_le_bytes());
    }
    tagged(b"MH2O", &data)
}

/// A chunk of terrain of `column` and `row`, its flags `flags`, with an older liquid of magma when
/// `magma`: its tile 5 dry, its vertices at 30 and their coordinates by their place.
fn mcnk(column: u32, row: u32, flags: u32, magma: bool) -> Vec<u8> {
    let mut header = vec![0u8; 128];
    header[0..4].copy_from_slice(&flags.to_le_bytes());
    header[4..8].copy_from_slice(&column.to_le_bytes());
    header[8..12].copy_from_slice(&row.to_le_bytes());
    if !magma {
        return tagged(b"MCNK", &header);
    }
    // The part after the header, at its own header, counted from the chunk's: 8 + 128.
    header[0x60..0x64].copy_from_slice(&(8u32 + 128).to_le_bytes());
    let mut liquid = floats(&[30.0, 31.0]);
    for index in 0..81u16 {
        liquid.extend(halves(&[index, 2 * index]));
        liquid.extend(floats(&[30.0 + f32::from(index) / 100.0]));
    }
    liquid.extend((0..64).map(|tile| if tile == 5 { 0x0F } else { 0x02 }));
    liquid.extend([0u8; 4 + 80]);
    let mut data = header;
    data.extend(tagged(b"MCLQ", &liquid));
    tagged(b"MCNK", &data)
}

/// A root of a tile: `MH2O`, then its chunks 0 (an older liquid it holds too, left out), 1 (an older
/// magma) and 2 (none).
fn root() -> Vec<u8> {
    let mut out = tagged(b"MVER", &18u32.to_le_bytes());
    out.extend(mh2o());
    out.extend(mcnk(0, 0, 0x10, true));
    out.extend(mcnk(1, 0, 0x10, true));
    out.extend(mcnk(2, 0, 0, false));
    out
}

#[test]
fn the_liquids_of_a_tile_are_its_layers_of_mh2o_and_the_older_ones_of_its_other_chunks() {
    let layers = liquid::liquids(&root(), [31, 49]).unwrap();
    let kinds: Vec<u16> = layers.iter().map(|layer| layer.liquid).collect();
    assert_eq!(
        kinds,
        [5, 2, 7, 3],
        "the older liquid of the chunk 0, which MH2O covers, left out"
    );
    let at = |row: usize, column: usize| row * LIQUID_SIDE + column;
    // The slow water: its corner by its tile and chunk, its one tile, its vertices over its
    // rectangle, the others at its least height.
    let water = &layers[0];
    assert_eq!(water.corner, [ORIGIN - 49.0 * TILE, ORIGIN - 31.0 * TILE]);
    assert_eq!(water.tiles, 1 << (2 * 8 + 1));
    assert_eq!(
        [
            water.heights[at(2, 1)],
            water.heights[at(2, 3)],
            water.heights[at(3, 1)],
            water.heights[at(3, 3)]
        ],
        [10.0, 11.0, 11.5, 12.0],
        "held within its heights"
    );
    assert_eq!(water.heights[at(0, 0)], 10.0);
    assert_eq!(
        (water.depths[at(2, 2)], water.depths[at(3, 3)], water.depths[0]),
        (20, 60, 255)
    );
    assert!(water.coordinates.is_empty());
    // The ocean: every tile, flat at its least height, full depth.
    let ocean = &layers[1];
    assert_eq!(
        ocean.corner,
        [ORIGIN - 49.0 * TILE - CHUNK, ORIGIN - 31.0 * TILE - CHUNK]
    );
    assert_eq!(ocean.tiles, u64::MAX);
    assert!(ocean.heights.iter().all(|height| *height == -1.0) && ocean.depths.iter().all(|depth| *depth == 255));
    // The magma: heights and coordinates in 255ths.
    let magma = &layers[2];
    assert_eq!(magma.tiles, 1);
    assert_eq!(magma.heights[at(1, 1)], 20.75);
    assert_eq!(
        [magma.coordinates[at(0, 1)], magma.coordinates[at(1, 0)]],
        [[1.0, 0.0], [0.0, 2.0]]
    );
    // The older magma of the chunk 1: its corner by its own column and row, its dry tile left out.
    let old = &layers[3];
    assert_eq!(old.corner, [ORIGIN - 49.0 * TILE, ORIGIN - 31.0 * TILE - CHUNK]);
    assert_eq!(old.tiles, !(1u64 << 5));
    assert_eq!(old.heights[10], 30.1);
    assert_eq!(old.coordinates[10], [10.0 / 255.0, 20.0 / 255.0]);
}

#[test]
fn a_liquid_damaged_is_refused_and_never_panics() {
    // A rectangle past its chunk.
    let mut bad = root();
    let at = bad.windows(4).position(|window| window == b"O2HM").unwrap() + 8;
    let first = u32::from_le_bytes(bad[at..at + 4].try_into().unwrap()) as usize;
    bad[at + first + 12] = 7;
    let said = liquid::liquids(&bad, [0, 0]).unwrap_err();
    assert!(
        said.contains("its chunk 0") && said.contains("past the chunk"),
        "{said}"
    );
    // Every cut of the root is refused or read, never panics.
    let whole = root();
    for cut in (0..whole.len()).step_by(7) {
        let _ = liquid::liquids(&whole[..cut], [0, 0]);
    }
}

#[test]
fn the_client_s_liquids_read_whole_and_lie_over_the_ground_they_cover() {
    let Some(client) = client() else {
        return;
    };
    let types = client.tables.liquid_types(&client.chain).unwrap();
    assert_eq!(types.len(), 26);
    let water = types.iter().find(|liquid| liquid.id == 5).unwrap();
    assert_eq!((water.kind, water.vertex_format), (1, Some(0)));
    assert_eq!(water.textures[5], r"XTextures\ocean\ocean_h.%d.blp");
    for map in ["Azeroth", "Kalimdor", "Expansion01", "Northrend"] {
        let wdt = client.wdt(map).unwrap();
        let tiles: Vec<u32> = (0..4096).filter(|index| wdt.tiles[*index as usize]).collect();
        let next = AtomicUsize::new(0);
        let counts = Mutex::new(BTreeMap::<u16, usize>::new());
        let faults = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    while let Some(index) = tiles.get(next.fetch_add(1, Ordering::Relaxed)) {
                        match client.liquids(map, index % 64, index / 64) {
                            Ok(layers) => {
                                let mut counts = counts.lock().unwrap();
                                for layer in layers.unwrap_or_default() {
                                    *counts.entry(layer.liquid).or_default() += 1;
                                }
                            }
                            Err(reason) => faults.lock().unwrap().push(reason),
                        }
                    }
                });
            }
        });
        let faults = faults.into_inner().unwrap();
        eprintln!(
            "{map}: layers by type {:?}; {} faults",
            counts.lock().unwrap(),
            faults.len()
        );
        assert!(faults.is_empty(), "{:?}", &faults[..faults.len().min(5)]);
    }
    // The waters by Goldshire: under the tiles they cover, the ground lies lower than their surface,
    // in the axes of the terrain and not across them.
    let tile = client.tile("Azeroth", 31, 49).unwrap().unwrap();
    let layers = client.liquids("Azeroth", 31, 49).unwrap().unwrap();
    let under = |across: bool| {
        let (mut under, mut covered) = (0, 0);
        for pond in layers.iter().filter(|layer| layer.liquid == 5) {
            let chunk = tile
                .chunks
                .iter()
                .find(|chunk| {
                    (chunk.position[0] - pond.corner[0]).abs() < 0.1 && (chunk.position[1] - pond.corner[1]).abs() < 0.1
                })
                .unwrap();
            for row in 0..8 {
                for column in 0..8 {
                    let [r, c] = if across { [column, row] } else { [row, column] };
                    // The inner vertex of the terrain at the middle of the tile.
                    let ground = chunk.position[2] + chunk.heights[r * 17 + 9 + c];
                    if pond.tiles >> (row * 8 + column) & 1 != 0 {
                        covered += 1;
                        under += usize::from(ground < pond.heights[row * LIQUID_SIDE + column]);
                    }
                }
            }
        }
        (under, covered)
    };
    let ((under, covered), (across, _)) = (under(false), under(true));
    // The water runs over the ground at its shores: three quarters under it at the least.
    assert!(
        covered > 20 && under * 4 >= covered * 3 && across + covered / 10 < under,
        "{under}, {across} of {covered}"
    );
}
