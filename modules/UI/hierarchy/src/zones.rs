//! The extent of each zone of a map, for the camera to be framed on one chosen in the Hierarchy: the
//! chunks of every tile of the map by their area, read once by a job from the headers of the
//! tiles, a zone holding its subzones.

use std::collections::HashMap;

use uniwow_api::formats::Formats;
use uniwow_api::scene::Area;

const TILE: f32 = 1600.0 / 3.0;
const CHUNK: f32 = TILE / 16.0;
const ORIGIN: f32 = 32.0 * TILE;

/// The extent of each area, low and high corners in the world, without height.
pub type Extents = HashMap<u32, [[f32; 2]; 2]>;

/// The extents of the areas of the chunks of `tiles`, each tile its place and the area of each of
/// its 256 chunks (`y * 16 + x`); each area's extent held by its parents of `areas` too.
pub fn extents(tiles: &[((u32, u32), Vec<u32>)], areas: &HashMap<u32, Area>) -> Extents {
    let mut out: Extents = HashMap::new();
    let mut grow = |area: u32, low: [f32; 2], high: [f32; 2]| {
        let extent = out.entry(area).or_insert([low, high]);
        for axis in 0..2 {
            extent[0][axis] = extent[0][axis].min(low[axis]);
            extent[1][axis] = extent[1][axis].max(high[axis]);
        }
    };
    for ((x, y), chunks) in tiles {
        for (at, area) in chunks.iter().enumerate() {
            if *area == 0 {
                continue;
            }
            let (cx, cy) = ((at % 16) as f32, (at / 16) as f32);
            // A chunk spans from its corner of highest X and Y down a chunk on each axis.
            let high = [
                ORIGIN - TILE * *y as f32 - CHUNK * cy,
                ORIGIN - TILE * *x as f32 - CHUNK * cx,
            ];
            let low = [high[0] - CHUNK, high[1] - CHUNK];
            // The area and its parents, a zone holding its subzones.
            let mut place = *area;
            for _ in 0..8 {
                grow(place, low, high);
                match areas.get(&place) {
                    Some(record) if record.parent != 0 => place = record.parent,
                    _ => break,
                }
            }
        }
    }
    out
}

/// The tiles of the map `directory` and the areas of their chunks, read from their headers.
pub fn read(formats: &dyn Formats, directory: &str, cancelled: impl Fn() -> bool) -> Vec<((u32, u32), Vec<u32>)> {
    let Ok(wdt) = formats.wdt(directory) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (at, present) in wdt.tiles.iter().enumerate() {
        if !present {
            continue;
        }
        if cancelled() {
            break;
        }
        let (x, y) = ((at % 64) as u32, (at / 64) as u32);
        if let Ok(Some(chunks)) = formats.tile_areas(directory, x, y) {
            out.push(((x, y), chunks));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_spans_its_chunks_and_those_of_its_subzones() {
        let areas = HashMap::from([
            (
                12,
                Area {
                    parent: 0,
                    map: 0,
                    name: "Elwynn Forest".to_owned(),
                },
            ),
            (
                87,
                Area {
                    parent: 12,
                    map: 0,
                    name: "Goldshire".to_owned(),
                },
            ),
        ]);
        let mut first = vec![12; 256];
        first[0] = 87;
        first[255] = 0;
        let mut second = vec![0; 256];
        second[16] = 87;
        let extents = extents(&[((32, 48), first), ((33, 48), second)], &areas);
        let elwynn = extents[&12];
        let goldshire = extents[&87];
        // Goldshire: the first chunk of the tile 32_48 and one of the tile 33_48, next to it in Y.
        assert!((goldshire[1][0] - (ORIGIN - TILE * 48.0)).abs() < 1e-2);
        assert!((goldshire[0][1] - (ORIGIN - TILE * 33.0 - CHUNK)).abs() < 1e-2);
        // Elwynn holds Goldshire.
        for axis in 0..2 {
            assert!(elwynn[0][axis] <= goldshire[0][axis] && elwynn[1][axis] >= goldshire[1][axis]);
        }
        assert!(!extents.contains_key(&0), "no area, no extent");
    }
}
