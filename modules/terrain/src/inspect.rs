//! The component *Chunk* of the Inspector (milestone 15, step 15.13a, decision 22): a chunk's
//! layers of texture with their alpha maps, its area, flags and holes, its water, its vertex
//! colours, its shadow baked in and its ambient sounds, as the files store them. Read only.

use uniwow_api::formats::{Chunk, FileRef, LiquidLayer, LiquidTypeRecord, Tile};
use uniwow_api::serde_json::{Value, json};

/// The flags of a chunk, by their bit.
const FLAGS: [(u32, &str); 9] = [
    (0x1, "shadow"),
    (0x2, "impassable"),
    (0x4, "river"),
    (0x8, "ocean"),
    (0x10, "magma"),
    (0x20, "slime"),
    (0x40, "vertex colours"),
    (0x8000, "do not fix alpha"),
    (0x10000, "high resolution holes"),
];

/// The flags of a layer, by their bit.
const LAYER_FLAGS: [(u32, &str); 5] = [
    (0x40, "animated"),
    (0x100, "alpha map"),
    (0x200, "compressed"),
    (0x400, "reflective"),
    (0x800, "overbright"),
];

fn named(flags: u32, names: &'static [(u32, &'static str)]) -> Vec<&'static str> {
    names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// The share of a map of 64 × 64 bytes over 0, from 0 to 100, to a tenth.
fn share(map: &[u8]) -> f64 {
    if map.is_empty() {
        return 0.0;
    }
    let sum: u64 = map.iter().map(|value| u64::from(*value)).sum();
    (sum as f64 * 1000.0 / (map.len() as f64 * 255.0)).round() / 10.0
}

/// The water of `chunk` among `liquids`: the layers whose corner is the chunk's.
fn water(chunk: &Chunk, liquids: &[LiquidLayer], types: &[LiquidTypeRecord]) -> Vec<Value> {
    liquids
        .iter()
        .filter(|layer| {
            (layer.corner[0] - chunk.position[0]).abs() < 1.0 && (layer.corner[1] - chunk.position[1]).abs() < 1.0
        })
        .map(|layer| {
            let heights = layer.heights.iter().copied().filter(|height| height.is_finite());
            let (low, high) = heights.fold((f32::MAX, f32::MIN), |(low, high), height| {
                (low.min(height), high.max(height))
            });
            let name = types
                .iter()
                .find(|kind| kind.id == u32::from(layer.liquid))
                .map(|kind| kind.name.clone())
                .unwrap_or_default();
            json!({
                "type": layer.liquid,
                "name": name,
                "squares": layer.tiles.count_ones(),
                "lowest": if low <= high { Some(low) } else { None },
                "highest": if low <= high { Some(high) } else { None },
                "deepest": layer.depths.iter().copied().filter(|depth| *depth != 255).max(),
            })
        })
        .collect()
}

/// The components of the chunk `index` of `tile`, its alpha maps the share of each layer: *Chunk*
/// and its raw fields; none for a chunk the tile does not have.
pub fn components(tile: &Tile, index: [u32; 2], liquids: &[LiquidLayer], types: &[LiquidTypeRecord]) -> Option<Value> {
    let chunk = tile.chunks.iter().find(|chunk| chunk.index == index)?;
    let layers: Vec<Value> = chunk
        .layers
        .iter()
        .enumerate()
        .map(|(at, layer)| {
            let texture = tile
                .textures
                .get(layer.texture as usize)
                .map(|file| match file {
                    FileRef::Path(path) => path.clone(),
                    FileRef::Id(id) => format!("file data id {id}"),
                })
                .unwrap_or_else(|| format!("missing texture {}", layer.texture));
            // The first layer lies under the others, everywhere.
            let covers = if at == 0 {
                100.0
            } else {
                chunk.alphas.get(at - 1).map_or(0.0, |map| share(map))
            };
            json!({
                "order": at,
                "texture": texture,
                "flags": named(layer.flags, &LAYER_FLAGS),
                "effect": layer.effect,
                "covers": covers,
            })
        })
        .collect();
    let colours = (!chunk.colours.is_empty()).then(|| {
        let mut sum = [0u64; 3];
        for colour in &chunk.colours {
            for (total, value) in sum.iter_mut().zip(colour) {
                *total += u64::from(*value);
            }
        }
        sum.map(|total| (total as f64 / chunk.colours.len() as f64 / 127.0 * 100.0).round() / 100.0)
    });
    let sounds: Vec<Value> = chunk
        .sounds
        .iter()
        .map(|sound| json!({ "sound": sound.sound, "position": sound.position, "size": sound.size }))
        .collect();
    Some(json!({
        "chunk": {
            "layers": layers,
            "area": chunk.area,
            "flags": named(chunk.flags, &FLAGS),
            "holes": chunk.holes.count_ones(),
            "water": water(chunk, liquids, types),
            "vertex colours": colours,
            "shadow": if chunk.shadow.is_empty() { None } else { Some(share(&chunk.shadow)) },
            "sounds": sounds,
            "doodads": chunk.doodad_refs.len(),
            "buildings": chunk.building_refs.len(),
        },
        "raw": {
            "index": chunk.index,
            "flags": format!("{:#x}", chunk.flags),
            "position": chunk.position,
            "holes": format!("{:#018x}", chunk.holes),
            "layers": chunk.layers.len(),
            "alpha maps": chunk.alphas.len(),
        },
    }))
}
