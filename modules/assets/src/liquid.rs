//! The liquids of a tile of 3.3.5a, from the public description of the format: the layers of its
//! chunk `MH2O`, each its type, the tiles of its chunk it covers and its vertices, by the format its
//! layer names; and in the chunks without, the older `MCLQ` of the chunk itself, its kind by the
//! flags of the chunk. What the client reads at a vertex outside a layer's tiles is its least
//! height, at full depth.

use uniwow_api::formats::{CHUNK, LIQUID_SIDE, LiquidLayer, ORIGIN, TILE};

use crate::terrain::{chunks, f32_at, u16_at, u32_at};

/// The size of an entry of the header of `MH2O`, and of a layer.
const ENTRY: usize = 12;
const INSTANCE: usize = 24;
/// The size of the header of a chunk of terrain, and where it gives its liquid: its offset, its size.
const HEADER: usize = 128;
const LIQUID_OFFSET: usize = 0x60;
/// An older liquid: its two heights, 81 vertices of 8 bytes, 64 tiles, its flows.
const OLD_LIQUID: usize = 8 + 81 * 8 + 64 + 4 + 2 * 40;
/// The flags of a chunk naming the kind of its older liquid, in order, with the type of
/// `LiquidType.dbc` each is: river, ocean, magma, slime.
const OLD_KINDS: [(u32, u16); 4] = [(0x4, 1), (0x8, 2), (0x10, 3), (0x20, 4)];
/// The flag of an older tile without liquid.
const OLD_DRY: u8 = 0x8;

/// The corner of highest X and Y of the chunk of `column` and `row` of the tile `<x>_<y>`.
pub fn corner(tile: [u32; 2], column: u32, row: u32) -> [f32; 2] {
    [
        ORIGIN - tile[1] as f32 * TILE - row as f32 * CHUNK,
        ORIGIN - tile[0] as f32 * TILE - column as f32 * CHUNK,
    ]
}

/// The layers of liquid of the root of the tile `<x>_<y>`, its bytes `root`.
pub fn liquids(root: &[u8], tile: [u32; 2]) -> Result<Vec<LiquidLayer>, String> {
    let found = chunks(root)?;
    let mut layers = Vec::new();
    let mut covered = [false; 256];
    if let Some((_, data)) = found.iter().find(|(name, _)| name == b"MH2O") {
        for (index, chunk) in covered.iter_mut().enumerate() {
            let at = index * ENTRY;
            if at + ENTRY > data.len() {
                break;
            }
            let (offset, count) = (u32_at(data, at)? as usize, u32_at(data, at + 4)? as usize);
            for layer in 0..count {
                let read = new_layer(data, offset + layer * INSTANCE, tile, index as u32)
                    .map_err(|reason| format!("the liquid of its chunk {index}: {reason}"))?;
                layers.push(read);
                *chunk = true;
            }
        }
    }
    for (index, (_, mcnk)) in found.iter().filter(|(name, _)| name == b"MCNK").enumerate() {
        if covered.get(index).copied().unwrap_or(true) {
            continue;
        }
        layers.extend(
            old_layers(mcnk, tile).map_err(|reason| format!("the older liquid of its chunk {index}: {reason}"))?,
        );
    }
    Ok(layers)
}

/// The layer of `MH2O`, its chunk's data `data`, at `at`, of the chunk `index` of `tile`.
fn new_layer(data: &[u8], at: usize, tile: [u32; 2], index: u32) -> Result<LiquidLayer, String> {
    let instance = data.get(at..at + INSTANCE).ok_or("its layer past the chunk")?;
    let liquid = u16_at(instance, 0)?;
    let format = u16_at(instance, 2)?;
    let [low, high] = [f32_at(instance, 4)?, f32_at(instance, 8)?];
    let [x0, y0, width, height] = [instance[12], instance[13], instance[14], instance[15]].map(usize::from);
    if x0 + width > 8 || y0 + height > 8 {
        return Err(format!("its tiles {x0}, {y0} of {width} × {height} past the chunk"));
    }
    let (bitmap, vertices) = (u32_at(instance, 16)? as usize, u32_at(instance, 20)? as usize);
    // Its tiles: the bits of its rectangle, row by row, all where it gives none.
    let mut tiles = 0u64;
    let cells = width * height;
    let bits = if bitmap == 0 {
        None
    } else {
        Some(
            data.get(bitmap..bitmap + cells.div_ceil(8))
                .ok_or("its tiles past the chunk")?,
        )
    };
    for cell in 0..cells {
        if bits.is_none_or(|bits| bits[cell / 8] >> (cell % 8) & 1 != 0) {
            tiles |= 1 << ((y0 + cell / width) * 8 + x0 + cell % width);
        }
    }
    let size = LIQUID_SIDE * LIQUID_SIDE;
    let mut layer = LiquidLayer {
        liquid,
        corner: corner(tile, index % 16, index / 16),
        tiles,
        heights: vec![low; size],
        // Of no depths: 0 where its format has none, as the client reads them; 255 otherwise, as
        // the client reads those of the format 2.
        depths: vec![if format == 1 { 0 } else { 255 }; size],
        coordinates: Vec::new(),
    };
    if vertices == 0 {
        return Ok(layer);
    }
    // Its vertices over its rectangle, row by row: their heights, coordinates and depths, as its
    // format has them.
    let places: Vec<usize> = (y0..=y0 + height)
        .flat_map(|row| (x0..=x0 + width).map(move |column| row * LIQUID_SIDE + column))
        .collect();
    let (has_heights, has_coordinates, has_depths) = match format {
        0 => (true, false, true),
        1 => (true, true, false),
        2 => (false, false, true),
        3 => (true, true, true),
        other => return Err(format!("its vertices of the format {other}")),
    };
    let mut at = vertices;
    let mut take = |bytes: usize| -> Result<&[u8], String> {
        let slice = data.get(at..at + bytes).ok_or("its vertices past the chunk")?;
        at += bytes;
        Ok(slice)
    };
    if has_heights {
        let heights = take(places.len() * 4)?;
        for (index, place) in places.iter().enumerate() {
            layer.heights[*place] = f32_at(heights, index * 4)?.clamp(low, high);
        }
    }
    if has_coordinates {
        let coordinates = take(places.len() * 4)?;
        layer.coordinates = vec![[0.0; 2]; size];
        for (index, place) in places.iter().enumerate() {
            layer.coordinates[*place] = [
                f32::from(u16_at(coordinates, index * 4)?) / 255.0,
                f32::from(u16_at(coordinates, index * 4 + 2)?) / 255.0,
            ];
        }
    }
    if has_depths {
        let depths = take(places.len())?;
        for (index, place) in places.iter().enumerate() {
            layer.depths[*place] = depths[index];
        }
    }
    Ok(layer)
}

/// The older liquids of the chunk of terrain `mcnk`, one for each kind its flags name.
fn old_layers(mcnk: &[u8], tile: [u32; 2]) -> Result<Vec<LiquidLayer>, String> {
    let flags = u32_at(mcnk, 0)?;
    let (column, row) = (u32_at(mcnk, 4)?, u32_at(mcnk, 8)?);
    let offset = u32_at(mcnk, LIQUID_OFFSET)? as usize;
    if offset < HEADER + 8 || OLD_KINDS.iter().all(|(flag, _)| flags & flag == 0) {
        return Ok(Vec::new());
    }
    // Counted from the chunk's own header, 8 bytes before its data: the part's header, then its data.
    let header = mcnk.get(offset - 8..offset).ok_or("its liquid past the chunk")?;
    if &[header[3], header[2], header[1], header[0]] != b"MCLQ" {
        return Err(format!("no MCLQ at {offset}"));
    }
    let mut at = offset;
    let mut layers = Vec::new();
    for (_, liquid) in OLD_KINDS.iter().filter(|(flag, _)| flags & flag != 0) {
        let data = mcnk.get(at..at + OLD_LIQUID).ok_or("its liquid past the chunk")?;
        at += OLD_LIQUID;
        let [low, high] = [f32_at(data, 0)?, f32_at(data, 4)?];
        let magma = *liquid == 3 || *liquid == 4;
        let vertex = |index: usize| &data[8 + index * 8..16 + index * 8];
        let size = LIQUID_SIDE * LIQUID_SIDE;
        let mut tiles = 0u64;
        for (index, cell) in data[8 + size * 8..8 + size * 8 + 64].iter().enumerate() {
            if cell & OLD_DRY == 0 {
                tiles |= 1 << index;
            }
        }
        layers.push(LiquidLayer {
            liquid: *liquid,
            corner: corner(tile, column, row),
            tiles,
            heights: (0..size)
                .map(|index| f32_at(vertex(index), 4).map(|height| height.clamp(low, high)))
                .collect::<Result<_, _>>()?,
            depths: (0..size)
                .map(|index| if magma { 255 } else { vertex(index)[0] })
                .collect(),
            coordinates: if magma {
                (0..size)
                    .map(|index| {
                        let corner = vertex(index);
                        Ok([
                            f32::from(u16_at(corner, 0)?) / 255.0,
                            f32::from(u16_at(corner, 2)?) / 255.0,
                        ])
                    })
                    .collect::<Result<_, String>>()?
            } else {
                Vec::new()
            },
        });
    }
    Ok(layers)
}
