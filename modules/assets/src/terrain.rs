//! The terrain of a map: its WDT, and its tiles (ADT) of 3.3.5a, or split in a root, a `_tex0` and
//! an `_obj0` as WarcraftXL loads them. Read from the public description of the formats and the
//! layouts of warcraft-rs (wow-adt, wow-wdt: MIT); the split tiles translated from the loaders of
//! wow.export (MIT, see THIRD_PARTY.md). A tile of 3.3.5a finds the parts of its chunks by the
//! offsets of their header, as the client does; a split tile by walking them, as wow.export does.

use uniwow_api::formats::{Building, Chunk, Doodad, FileRef, Layer, Tile, Wdt};

/// The flags of `MPHD` that make the alpha maps 8 bits a texel, as wow.export reads them.
const BIG_ALPHA: u32 = 0x4 | 0x80;

/// The flags of a chunk.
const HAS_SHADOW: u32 = 0x1;
const HAS_COLOURS: u32 = 0x40;
const DO_NOT_FIX_ALPHA: u32 = 0x8000;
const HIGH_RES_HOLES: u32 = 0x10000;

/// The flags of a layer.
const ALPHA_COMPRESSED: u32 = 0x200;

/// The flags of a placement whose name is a FileDataID.
const DOODAD_BY_ID: u16 = 0x40;
const BUILDING_BY_ID: u16 = 0x8;
/// The flag of a building whose scale is given, in 1024ths.
const BUILDING_SCALED: u16 = 0x4;

/// The chunks of a file: the name of each, as the format writes it, and its bytes.
type Chunks<'a> = Vec<([u8; 4], &'a [u8])>;

/// The chunks of a file in their order; the file holds their names reversed. Fewer bytes than a
/// header at the end, as some files of the client have, are not a chunk.
fn chunks(bytes: &[u8]) -> Result<Chunks<'_>, String> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(header) = bytes.get(at..at + 8) {
        let name = [header[3], header[2], header[1], header[0]];
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let data = bytes
            .get(at + 8..at + 8 + size)
            .ok_or_else(|| format!("the chunk {} cut short", String::from_utf8_lossy(&name)))?;
        found.push((name, data));
        at += 8 + size;
    }
    Ok(found)
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("cut short at byte {at}"))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("cut short at byte {at}"))
}

fn f32_at(bytes: &[u8], at: usize) -> Result<f32, String> {
    u32_at(bytes, at).map(f32::from_bits)
}

fn f32s<const N: usize>(bytes: &[u8], at: usize) -> Result<[f32; N], String> {
    let mut values = [0.0; N];
    for (index, value) in values.iter_mut().enumerate() {
        *value = f32_at(bytes, at + index * 4)?;
    }
    Ok(values)
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect()
}

/// The WDT of a map.
pub fn wdt(bytes: &[u8]) -> Result<Wdt, String> {
    let mut flags = 0;
    let mut tiles = None;
    for (name, data) in chunks(bytes)? {
        match &name {
            b"MPHD" => flags = u32_at(data, 0)?,
            b"MAIN" => {
                let entries = data.get(..64 * 64 * 8).ok_or("its tiles cut short")?;
                tiles = Some(
                    entries
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|entry| entry[0] & 1 != 0)
                        .collect(),
                );
            }
            _ => {}
        }
    }
    Ok(Wdt {
        flags,
        tiles: tiles.ok_or("no list of tiles (MAIN)")?,
    })
}

/// The names of a block of strings, each ended by a zero, the empty ones left out.
fn names(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// The name at `offset` in a block of strings.
fn name_at(bytes: &[u8], offset: u32) -> Result<String, String> {
    let rest = bytes
        .get(offset as usize..)
        .ok_or_else(|| format!("a name at {offset}, out of its block"))?;
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// The tile in `root`, of 3.3.5a when `tex` and `obj` are none, else split; `wdt_flags` are those of
/// its map, which say how its alpha maps are stored.
pub fn tile<'a>(root: &'a [u8], tex: Option<&'a [u8]>, obj: Option<&'a [u8]>, wdt_flags: u32) -> Result<Tile, String> {
    let monolithic = tex.is_none();
    let root_chunks = chunks(root)?;
    let tex_chunks = tex.map(chunks).transpose()?.unwrap_or_default();
    let obj_chunks = obj.map(chunks).transpose()?.unwrap_or_default();
    // The chunks of names and placements: in the root of 3.3.5a, else in the `_tex0` and `_obj0`.
    let named = |name: &[u8; 4], split: &[([u8; 4], &'a [u8])]| -> Option<&'a [u8]> {
        let from = if monolithic { &root_chunks[..] } else { split };
        from.iter().find(|(found, _)| found == name).map(|(_, data)| *data)
    };
    let textures = match named(b"MDID", &tex_chunks) {
        Some(ids) => u32s(ids).into_iter().map(FileRef::Id).collect(),
        None => names(named(b"MTEX", &tex_chunks).unwrap_or_default())
            .into_iter()
            .map(FileRef::Path)
            .collect(),
    };
    let doodads = doodads(
        named(b"MDDF", &obj_chunks).unwrap_or_default(),
        named(b"MMDX", &obj_chunks).unwrap_or_default(),
        &u32s(named(b"MMID", &obj_chunks).unwrap_or_default()),
    )?;
    let buildings = buildings(
        named(b"MODF", &obj_chunks).unwrap_or_default(),
        named(b"MWMO", &obj_chunks).unwrap_or_default(),
        &u32s(named(b"MWID", &obj_chunks).unwrap_or_default()),
    )?;

    let cells = |list: &[([u8; 4], &'a [u8])]| -> Vec<&'a [u8]> {
        list.iter()
            .filter(|(name, _)| name == b"MCNK")
            .map(|(_, data)| *data)
            .collect()
    };
    let roots = cells(&root_chunks);
    if roots.len() != 256 {
        return Err(format!("{} chunks, where a tile has 256", roots.len()));
    }
    let texs = cells(&tex_chunks);
    let objs = cells(&obj_chunks);
    let big_alpha = wdt_flags & BIG_ALPHA != 0;
    let chunks = roots
        .iter()
        .enumerate()
        .map(|(index, root)| {
            let parts = if monolithic {
                Parts::by_offsets(root)?
            } else {
                Parts::split(root, texs.get(index).copied(), objs.get(index).copied())?
            };
            chunk(root, &parts, big_alpha).map_err(|e| format!("its chunk {index}: {e}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Tile {
        chunks,
        textures,
        doodads,
        buildings,
    })
}

fn doodads(entries: &[u8], names: &[u8], offsets: &[u32]) -> Result<Vec<Doodad>, String> {
    entries
        .as_chunks::<36>()
        .0
        .iter()
        .map(|entry| {
            let name = u32_at(entry, 0)?;
            let flags = u16_at(entry, 34)?;
            let file = if flags & DOODAD_BY_ID != 0 {
                FileRef::Id(name)
            } else {
                let offset = offsets
                    .get(name as usize)
                    .ok_or_else(|| format!("a doodad named {name}, out of its names"))?;
                FileRef::Path(name_at(names, *offset)?)
            };
            Ok(Doodad {
                file,
                unique_id: u32_at(entry, 4)?,
                position: f32s(entry, 8)?,
                rotation: f32s(entry, 20)?,
                scale: f32::from(u16_at(entry, 32)?) / 1024.0,
                flags,
            })
        })
        .collect()
}

fn buildings(entries: &[u8], names: &[u8], offsets: &[u32]) -> Result<Vec<Building>, String> {
    entries
        .as_chunks::<64>()
        .0
        .iter()
        .map(|entry| {
            let name = u32_at(entry, 0)?;
            let flags = u16_at(entry, 56)?;
            let file = if flags & BUILDING_BY_ID != 0 {
                FileRef::Id(name)
            } else {
                let offset = offsets
                    .get(name as usize)
                    .ok_or_else(|| format!("a building named {name}, out of its names"))?;
                FileRef::Path(name_at(names, *offset)?)
            };
            let scale = u16_at(entry, 62)?;
            Ok(Building {
                file,
                unique_id: u32_at(entry, 4)?,
                position: f32s(entry, 8)?,
                rotation: f32s(entry, 20)?,
                bounds: [f32s(entry, 32)?, f32s(entry, 44)?],
                scale: if flags & BUILDING_SCALED != 0 {
                    f32::from(scale) / 1024.0
                } else {
                    1.0
                },
                flags,
                doodad_set: u16_at(entry, 58)?,
                name_set: u16_at(entry, 60)?,
            })
        })
        .collect()
}

/// The parts of a chunk, wherever they are: the bytes of each, none when it has none.
#[derive(Default)]
struct Parts<'a> {
    heights: Option<&'a [u8]>,
    normals: Option<&'a [u8]>,
    colours: Option<&'a [u8]>,
    layers: Option<&'a [u8]>,
    alphas: Option<&'a [u8]>,
    shadow: Option<&'a [u8]>,
    refs: Option<&'a [u8]>,
    doodad_refs: Option<&'a [u8]>,
    building_refs: Option<&'a [u8]>,
}

/// The size of the header of a chunk of terrain (`MCNK`).
const HEADER: usize = 128;

impl<'a> Parts<'a> {
    /// The parts of a chunk of 3.3.5a, at the offsets of its header, counted from the start of the
    /// chunk's own header, 8 bytes before its data; only those its flags and counts ask for, as
    /// some offsets point at nothing else. A part whose size goes past the chunk reaches its end;
    /// the alpha maps always do: the client reads them by offset, past the size of their part in
    /// some tiles of the client.
    fn by_offsets(mcnk: &'a [u8]) -> Result<Self, String> {
        // Where the data of the part named `name` starts, and where its size ends it.
        let place = |wanted: bool, offset_at: usize, name: &[u8; 4]| -> Result<Option<(usize, usize)>, String> {
            let offset = u32_at(mcnk, offset_at)? as usize;
            if !wanted || offset == 0 {
                return Ok(None);
            }
            let at = offset
                .checked_sub(8)
                .ok_or_else(|| format!("its part at {offset}, inside its header"))?;
            let header = mcnk
                .get(at..at + 8)
                .ok_or_else(|| format!("its part at {offset}, out of it"))?;
            if &[header[3], header[2], header[1], header[0]] != name {
                return Err(format!("no {} at {offset}", String::from_utf8_lossy(name)));
            }
            let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
            Ok(Some((at + 8, (at + 8).saturating_add(size).min(mcnk.len()))))
        };
        let part = |wanted, offset_at, name| -> Result<Option<&'a [u8]>, String> {
            Ok(place(wanted, offset_at, name)?.map(|(start, end)| &mcnk[start..end]))
        };
        let flags = u32_at(mcnk, 0)?;
        let layers = u32_at(mcnk, 0x0C)?;
        let refs = u32_at(mcnk, 0x10)?.saturating_add(u32_at(mcnk, 0x38)?);
        Ok(Self {
            // The high resolution holes take the place of the offset of the heights, in tiles
            // after 3.3.5a.
            heights: part(flags & HIGH_RES_HOLES == 0, 0x14, b"MCVT")?,
            normals: part(true, 0x18, b"MCNR")?,
            layers: part(layers > 0, 0x1C, b"MCLY")?,
            refs: part(refs > 0, 0x20, b"MCRF")?,
            alphas: place(layers > 1, 0x24, b"MCAL")?.map(|(start, _)| &mcnk[start..]),
            shadow: part(flags & HAS_SHADOW != 0, 0x2C, b"MCSH")?,
            colours: part(flags & HAS_COLOURS != 0, 0x74, b"MCCV")?,
            ..Self::default()
        })
    }

    /// The parts of a chunk of a split tile: walked in its root after the header, and in its chunks
    /// of the `_tex0` and `_obj0`, which have no header.
    fn split(root: &'a [u8], tex: Option<&'a [u8]>, obj: Option<&'a [u8]>) -> Result<Self, String> {
        let mut parts = Self::default();
        let body = root.get(HEADER..).ok_or("its header cut short")?;
        for bytes in [Some(body), tex, obj].into_iter().flatten() {
            for (name, data) in chunks(bytes)? {
                let slot = match &name {
                    b"MCVT" => &mut parts.heights,
                    b"MCNR" => &mut parts.normals,
                    b"MCCV" => &mut parts.colours,
                    b"MCLY" => &mut parts.layers,
                    b"MCAL" => &mut parts.alphas,
                    b"MCSH" => &mut parts.shadow,
                    b"MCRD" => &mut parts.doodad_refs,
                    b"MCRW" => &mut parts.building_refs,
                    _ => continue,
                };
                *slot = Some(data);
            }
        }
        Ok(parts)
    }
}

/// A chunk from its header in `root` and its `parts`.
fn chunk(root: &[u8], parts: &Parts, big_alpha: bool) -> Result<Chunk, String> {
    let flags = u32_at(root, 0)?;
    let heights = parts.heights.ok_or("no heights (MCVT)")?;
    let heights = (0..145)
        .map(|i| f32_at(heights, i * 4))
        .collect::<Result<Vec<_>, _>>()?;
    let normals = parts.normals.ok_or("no normals (MCNR)")?;
    let normals = normals
        .get(..145 * 3)
        .ok_or("its normals cut short")?
        .as_chunks::<3>()
        .0
        .iter()
        .map(|n| [n[0] as i8, n[1] as i8, n[2] as i8])
        .collect();
    let colours = match parts.colours {
        Some(colours) => colours
            .get(..145 * 4)
            .ok_or("its colours cut short")?
            .as_chunks::<4>()
            .0
            .iter()
            // Stored blue, green, red, alpha.
            .map(|c| [c[2], c[1], c[0], c[3]])
            .collect(),
        None => Vec::new(),
    };
    let holes = if flags & HIGH_RES_HOLES != 0 {
        u64::from_le_bytes(
            root.get(0x14..0x1C)
                .ok_or("its holes cut short")?
                .try_into()
                .unwrap_or_default(),
        )
    } else {
        low_res_holes(u16_at(root, 0x3C)?)
    };
    let layers: Vec<(Layer, u32)> = parts
        .layers
        .unwrap_or_default()
        .as_chunks::<16>()
        .0
        .iter()
        .map(|layer| {
            Ok((
                Layer {
                    texture: u32_at(layer, 0)?,
                    flags: u32_at(layer, 4)?,
                    effect: u32_at(layer, 12)?,
                },
                u32_at(layer, 8)?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let fix = flags & DO_NOT_FIX_ALPHA == 0;
    let alphas = layers
        .iter()
        .skip(1)
        .map(|(layer, offset)| {
            alpha(
                parts.alphas.unwrap_or_default(),
                *offset as usize,
                layer.flags,
                big_alpha,
                fix,
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    let shadow = match parts.shadow {
        Some(shadow) if flags & HAS_SHADOW != 0 => shadow_map(shadow, fix)?,
        _ => Vec::new(),
    };
    let (doodad_refs, building_refs) = match parts.refs {
        // 3.3.5a: the doodads, then the buildings, as many as the header counts.
        Some(refs) => {
            let refs = u32s(refs);
            let doodads = (u32_at(root, 0x10)? as usize).min(refs.len());
            let buildings = (u32_at(root, 0x38)? as usize).min(refs.len() - doodads);
            (refs[..doodads].to_vec(), refs[doodads..doodads + buildings].to_vec())
        }
        None => (
            u32s(parts.doodad_refs.unwrap_or_default()),
            u32s(parts.building_refs.unwrap_or_default()),
        ),
    };
    Ok(Chunk {
        index: [u32_at(root, 4)?, u32_at(root, 8)?],
        flags,
        position: f32s(root, 0x68)?,
        heights,
        normals,
        colours,
        area: u32_at(root, 0x34)?,
        holes,
        layers: layers.into_iter().map(|(layer, _)| layer).collect(),
        alphas,
        shadow,
        doodad_refs,
        building_refs,
    })
}

/// The holes of 3.3.5a, a bit per 2 × 2 quads of the 4 × 4, as a bit per quad of the 8 × 8.
fn low_res_holes(low: u16) -> u64 {
    let mut holes = 0;
    for bit in 0..16 {
        if low & (1 << bit) != 0 {
            let (row, column) = (bit / 4 * 2, bit % 4 * 2);
            for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                holes |= 1 << ((row + r) * 8 + column + c);
            }
        }
    }
    holes
}

/// Copies the row and the column before the last into the last ones, as the client of 3.3.5a does
/// for a chunk whose maps are 63 × 63.
fn fix_edges(map: &mut [u8]) {
    for row in 0..64 {
        map[row * 64 + 63] = map[row * 64 + 62];
    }
    for column in 0..64 {
        map[63 * 64 + column] = map[62 * 64 + column];
    }
}

/// The alpha map at `offset` in `alphas`: compressed, of 8 bits, or of 4 bits a texel.
fn alpha(alphas: &[u8], offset: usize, flags: u32, big_alpha: bool, fix: bool) -> Result<Vec<u8>, String> {
    let data = alphas
        .get(offset..)
        .ok_or_else(|| format!("an alpha map at {offset}, out of them"))?;
    if flags & ALPHA_COMPRESSED != 0 {
        let mut map = Vec::with_capacity(4096);
        let mut at = 0;
        while map.len() < 4096 {
            let info = *data.get(at).ok_or("a compressed alpha map cut short")?;
            at += 1;
            let count = usize::from(info & 0x7F).min(4096 - map.len());
            if info & 0x80 != 0 {
                let value = *data.get(at).ok_or("a compressed alpha map cut short")?;
                at += 1;
                map.extend(std::iter::repeat_n(value, count));
            } else {
                map.extend_from_slice(data.get(at..at + count).ok_or("a compressed alpha map cut short")?);
                at += count;
            }
        }
        Ok(map)
    } else if big_alpha {
        Ok(data.get(..4096).ok_or("an alpha map cut short")?.to_vec())
    } else {
        let packed = data.get(..2048).ok_or("an alpha map cut short")?;
        let mut map: Vec<u8> = packed
            .iter()
            .flat_map(|byte| [(byte & 0x0F) * 17, (byte >> 4) * 17])
            .collect();
        if fix {
            fix_edges(&mut map);
        }
        Ok(map)
    }
}

/// The shadow baked in a chunk: a bit a texel, from the lowest, 64 a row.
fn shadow_map(bytes: &[u8], fix: bool) -> Result<Vec<u8>, String> {
    let packed = bytes.get(..512).ok_or("its shadow cut short")?;
    let mut map: Vec<u8> = packed
        .iter()
        .flat_map(|byte| (0..8).map(move |bit| if byte & (1 << bit) != 0 { 255 } else { 0 }))
        .collect();
    if fix {
        fix_edges(&mut map);
    }
    Ok(map)
}
