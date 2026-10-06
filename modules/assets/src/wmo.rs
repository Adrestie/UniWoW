//! The buildings (WMO, version 17) of 3.3.5a and modern: their root and their groups at their
//! finest level, as plain data, read from the public description of the format. Of the modern
//! ones, their groups and their doodads by FileDataID (`GFID`, `MODI`) as the loader of wow.export
//! reads them (MIT, see THIRD_PARTY.md); their triangles of `MPY2`, and the material of a batch past
//! 255, as the description gives them. What refers to what the building does not have is left
//! out, or drawn as nothing, and said in its faults.

use std::sync::Mutex;

use uniwow_api::formats::{
    DoodadSet, FileRef, FogBand, Portal, PortalRef, Wmo, WmoBatch, WmoDoodad, WmoFace, WmoFog, WmoGroup, WmoLight,
    WmoLiquid, WmoMaterial,
};
use uniwow_api::parallel::parallel_for;

use crate::terrain::{chunks, f32_at, f32s, name_at, u16_at, u32_at};

/// The flag of a batch whose material is past 255, given in the last of its bounds.
const LARGE_MATERIAL: u8 = 0x2;
/// The last shader of 3.3.5a, whose materials hold other data where later clients name a third
/// texture.
const LAST_SHADER_OF_3_3_5A: u32 = 6;
/// The size of the header of a group.
const GROUP_HEADER: usize = 68;

/// Where a group of a building is: its FileDataID when the root names one, and its path beside
/// the root, its name followed by its index.
pub struct GroupRef {
    pub id: Option<u32>,
    pub path: String,
}

/// A colour as the file holds it, blue, green, red and alpha, made red, green, blue and alpha.
fn colour(bytes: &[u8], at: usize) -> Result<[u8; 4], String> {
    let [blue, green, red, alpha] = u32_at(bytes, at)?.to_le_bytes();
    Ok([red, green, blue, alpha])
}

/// The records of `size` bytes of `data`, each read by `read`.
fn records<T>(data: &[u8], size: usize, read: impl FnMut(&[u8]) -> Result<T, String>) -> Result<Vec<T>, String> {
    data.chunks_exact(size).map(read).collect()
}

/// The path of the group `index` of the building at `path`.
pub fn group_path(path: &str, index: usize) -> String {
    let stem = if path.to_ascii_lowercase().ends_with(".wmo") {
        &path[..path.len() - 4]
    } else {
        path
    };
    format!("{stem}_{index:03}.wmo")
}

/// What the root says of a group: its flags, its bounds and its name.
struct GroupInfo {
    flags: u32,
    bounds: [[f32; 3]; 2],
    name: String,
}

/// The building `root`, read from `path`; its groups read by `group`, on the workers of the pool.
pub fn read(
    root: &[u8],
    path: &str,
    group: impl Fn(&GroupRef) -> Result<Vec<u8>, String> + Sync,
) -> Result<Wmo, String> {
    let found = chunks(root)?;
    let named = |name: &[u8; 4]| found.iter().find(|(found, _)| found == name).map(|(_, data)| *data);
    let version = named(b"MVER").map(|data| u32_at(data, 0)).transpose()?;
    if version != Some(17) {
        return Err(format!("of version {version:?}, not 17"));
    }
    let header = named(b"MOHD").ok_or("no MOHD")?;
    let group_count = u32_at(header, 4)? as usize;
    let mut faults = Vec::new();

    // Its textures by their name in `MOTX`, or by FileDataID when it has none, as a modern one.
    let names = named(b"MOTX");
    let texture = |value: u32| -> Result<Option<FileRef>, String> {
        match names {
            Some(block) => Ok(Some(name_at(block, value)?)
                .filter(|name| !name.is_empty())
                .map(FileRef::Path)),
            None => Ok((value != 0).then_some(FileRef::Id(value))),
        }
    };
    let materials = records(named(b"MOMT").unwrap_or_default(), 64, |data| {
        let word = |index: usize| u32_at(data, 4 * index);
        let shader = word(1)?;
        let mut textures = [None, None, None];
        let third = (shader > LAST_SHADER_OF_3_3_5A).then_some(9);
        for (slot, field) in [Some(3), Some(6), third].into_iter().enumerate() {
            if let Some(field) = field {
                textures[slot] = texture(word(field)?).unwrap_or_else(|reason| {
                    faults.push(format!("a material's texture {}: {reason}", slot + 1));
                    None
                });
            }
        }
        Ok(WmoMaterial {
            flags: word(0)?,
            shader,
            blending: word(2)?,
            textures,
            emissive: colour(data, 16)?,
            diffuse: colour(data, 28)?,
            colour: colour(data, 40)?,
            ground: word(8)?,
        })
    })?;

    let group_names = named(b"MOGN").unwrap_or_default();
    let infos = records(named(b"MOGI").unwrap_or_default(), 32, |data| {
        let name = match u32_at(data, 28)? as i32 {
            -1 => String::new(),
            offset => name_at(group_names, offset as u32).unwrap_or_default(),
        };
        Ok(GroupInfo {
            flags: u32_at(data, 0)?,
            bounds: [f32s(data, 4)?, f32s(data, 16)?],
            name,
        })
    })?;

    let skybox = match named(b"MOSB") {
        Some(data) => Some(name_at(data, 0)?)
            .filter(|name| !name.is_empty())
            .map(FileRef::Path),
        None => None,
    };

    let portal_vertices = records(named(b"MOPV").unwrap_or_default(), 12, |data| f32s::<3>(data, 0))?;
    let portals = records(named(b"MOPT").unwrap_or_default(), 20, |data| {
        let (first, count) = (usize::from(u16_at(data, 0)?), usize::from(u16_at(data, 2)?));
        let vertices = portal_vertices.get(first..first + count).map(<[_]>::to_vec);
        if vertices.is_none() {
            faults.push(format!(
                "a portal of the vertices {first} to {}, of {}",
                first + count,
                portal_vertices.len()
            ));
        }
        Ok(Portal {
            vertices: vertices.unwrap_or_default(),
            plane: f32s(data, 4)?,
        })
    })?;
    let portal_refs = records(named(b"MOPR").unwrap_or_default(), 8, |data| {
        Ok(PortalRef {
            portal: u16_at(data, 0)?,
            group: u16_at(data, 2)?,
            side: u16_at(data, 4)? as i16,
        })
    })?;

    let lights = records(named(b"MOLT").unwrap_or_default(), 48, |data| {
        Ok(WmoLight {
            kind: data[0],
            attenuated: data[1] != 0,
            colour: colour(data, 4)?,
            position: f32s(data, 8)?,
            intensity: f32_at(data, 20)?,
            attenuation: f32s(data, 40)?,
        })
    })?;

    let doodad_sets = records(named(b"MODS").unwrap_or_default(), 32, |data| {
        let name = &data[..20];
        let end = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
        Ok(DoodadSet {
            name: String::from_utf8_lossy(&name[..end]).into_owned(),
            first: u32_at(data, 20)?,
            count: u32_at(data, 24)?,
        })
    })?;
    // Its doodads by their place in `MODI` when it has one, as a modern one, else by their name.
    let doodad_ids = named(b"MODI").map(|data| records(data, 4, |id| u32_at(id, 0)));
    let doodad_ids = doodad_ids.transpose()?;
    let doodad_names = named(b"MODN").unwrap_or_default();
    let doodads = records(named(b"MODD").unwrap_or_default(), 40, |data| {
        let word = u32_at(data, 0)?;
        let name = word & 0xFF_FFFF;
        let file = match &doodad_ids {
            Some(ids) => ids.get(name as usize).map(|id| FileRef::Id(*id)),
            None => name_at(doodad_names, name)
                .ok()
                .filter(|name| !name.is_empty())
                .map(FileRef::Path),
        };
        if file.is_none() {
            faults.push(format!("a doodad named by {name}, which names no model"));
        }
        Ok(WmoDoodad {
            file: file.unwrap_or(FileRef::Path(String::new())),
            flags: (word >> 24) as u8,
            position: f32s(data, 4)?,
            rotation: f32s(data, 16)?,
            scale: f32_at(data, 32)?,
            colour: colour(data, 36)?,
        })
    })?;

    let band = |data: &[u8], at: usize| -> Result<FogBand, String> {
        Ok(FogBand {
            end: f32_at(data, at)?,
            start: f32_at(data, at + 4)?,
            colour: colour(data, at + 8)?,
        })
    };
    let fogs = records(named(b"MFOG").unwrap_or_default(), 48, |data| {
        Ok(WmoFog {
            flags: u32_at(data, 0)?,
            position: f32s(data, 4)?,
            radii: f32s(data, 16)?,
            fog: band(data, 24)?,
            underwater: band(data, 36)?,
        })
    })?;

    // Its groups at their finest level, the first of `GFID`, each read by a worker.
    let group_ids = named(b"GFID")
        .map(|data| records(data, 4, |id| u32_at(id, 0)))
        .transpose()?;
    let reads: Vec<Mutex<Option<Result<WmoGroup, String>>>> = (0..group_count).map(|_| Mutex::new(None)).collect();
    parallel_for(group_count, 1, |range| {
        for index in range {
            let at = GroupRef {
                id: group_ids
                    .as_ref()
                    .and_then(|ids| ids.get(index).copied())
                    .filter(|id| *id != 0),
                path: group_path(path, index),
            };
            let read = group(&at).and_then(|bytes| read_group(&bytes));
            *reads[index].lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
        }
    });
    let mut groups = Vec::with_capacity(group_count);
    for (index, read) in reads.into_iter().enumerate() {
        let info = infos.get(index);
        let read = read.into_inner().unwrap_or_else(|e| e.into_inner());
        let mut group = match read {
            Some(Ok(group)) => group,
            Some(Err(reason)) => {
                faults.push(format!("group {index} ({}): {reason}", group_path(path, index)));
                WmoGroup {
                    flags: info.map_or(0, |info| info.flags),
                    bounds: info.map_or([[0.0; 3]; 2], |info| info.bounds),
                    ..WmoGroup::default()
                }
            }
            None => WmoGroup::default(),
        };
        group.name = info.map(|info| info.name.clone()).unwrap_or_default();
        check_group(
            &mut group,
            materials.len(),
            doodads.len(),
            portal_refs.len(),
            &mut |fault| {
                faults.push(format!("group {index}: {fault}"));
            },
        );
        groups.push(group);
    }

    for (index, reference) in portal_refs.iter().enumerate() {
        if usize::from(reference.portal) >= portals.len() || usize::from(reference.group) >= groups.len() {
            faults.push(format!(
                "the portal reference {index}: the portal {} of {}, the group {} of {}",
                reference.portal,
                portals.len(),
                reference.group,
                groups.len()
            ));
        }
    }
    let mut doodad_sets = doodad_sets;
    for set in &mut doodad_sets {
        let held = (doodads.len() as u32).saturating_sub(set.first);
        if set.count > held {
            faults.push(format!(
                "the doodad set {:?}: {} doodads from {}, of {}",
                set.name,
                set.count,
                set.first,
                doodads.len()
            ));
            set.count = held;
        }
    }

    Ok(Wmo {
        flags: u16_at(header, 60)?,
        ambient: colour(header, 28)?,
        id: u32_at(header, 32)?,
        bounds: [f32s(header, 36)?, f32s(header, 48)?],
        skybox,
        materials,
        groups,
        portals,
        portal_refs,
        lights,
        doodad_sets,
        doodads,
        fogs,
        faults,
    })
}

/// The chunks of the file of a group: some of the client declare their `MOGP` longer than the
/// file, the chunks inside it ending with the file; it is read to the end of the file, as the
/// client reads it.
fn group_chunks(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(header) = bytes.get(at..at + 8) {
        let name = [header[3], header[2], header[1], header[0]];
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let end = (at + 8).saturating_add(size).min(bytes.len());
        found.push((name, &bytes[at + 8..end]));
        at = end;
    }
    found
}

/// A group of a building, from its file.
fn read_group(bytes: &[u8]) -> Result<WmoGroup, String> {
    let found = group_chunks(bytes);
    let named = |name: &[u8; 4]| found.iter().find(|(found, _)| found == name).map(|(_, data)| *data);
    let version = named(b"MVER").map(|data| u32_at(data, 0)).transpose()?;
    if version != Some(17) {
        return Err(format!("of version {version:?}, not 17"));
    }
    let data = named(b"MOGP").ok_or("no MOGP")?;
    if data.len() < GROUP_HEADER {
        return Err(format!("a header of {} bytes", data.len()));
    }
    let mut group = WmoGroup {
        flags: u32_at(data, 8)?,
        bounds: [f32s(data, 12)?, f32s(data, 24)?],
        portals: [u16_at(data, 36)?, u16_at(data, 38)?],
        batch_counts: [u16_at(data, 40)?, u16_at(data, 42)?, u16_at(data, 44)?],
        fogs: [data[48], data[49], data[50], data[51]],
        liquid_type: u32_at(data, 52)?,
        id: u32_at(data, 56)?,
        ..WmoGroup::default()
    };
    for (name, data) in chunks(&data[GROUP_HEADER..])? {
        match &name {
            b"MOPY" => {
                group.faces = records(data, 2, |face| {
                    Ok(WmoFace {
                        flags: u16::from(face[0]),
                        material: (face[1] != 0xFF).then_some(u16::from(face[1])),
                    })
                })?
            }
            b"MPY2" => {
                group.faces = records(data, 4, |face| {
                    let material = u16_at(face, 2)?;
                    Ok(WmoFace {
                        flags: u16_at(face, 0)?,
                        material: (material != 0xFFFF).then_some(material),
                    })
                })?
            }
            b"MOVI" => group.triangles = records(data, 2, |index| u16_at(index, 0))?,
            b"MOVT" => group.vertices = records(data, 12, |vertex| f32s(vertex, 0))?,
            b"MONR" => group.normals = records(data, 12, |normal| f32s(normal, 0))?,
            b"MOTV" => group.coordinates.push(records(data, 8, |pair| f32s(pair, 0))?),
            b"MOCV" => group.colours.push(records(data, 4, |value| colour(value, 0))?),
            b"MOBA" => {
                group.batches = records(data, 24, |batch| {
                    let flags = batch[22];
                    let material = if flags & LARGE_MATERIAL != 0 {
                        u16_at(batch, 10)?
                    } else {
                        u16::from(batch[23])
                    };
                    Ok(WmoBatch {
                        first: u32_at(batch, 12)?,
                        count: u32::from(u16_at(batch, 16)?),
                        vertices: [u16_at(batch, 18)?, u16_at(batch, 20)?],
                        flags,
                        material,
                    })
                })?
            }
            b"MODR" => group.doodad_refs = records(data, 2, |index| u16_at(index, 0))?,
            b"MLIQ" => group.liquid = Some(liquid(data).map_err(|reason| format!("its liquid: {reason}"))?),
            _ => {}
        }
    }
    Ok(group)
}

/// The liquid of a group, from its chunk `MLIQ`.
fn liquid(data: &[u8]) -> Result<WmoLiquid, String> {
    let size = [u32_at(data, 0)?, u32_at(data, 4)?];
    let tiles = [u32_at(data, 8)?, u32_at(data, 12)?];
    let count = |pair: [u32; 2]| (pair[0] as usize).checked_mul(pair[1] as usize).ok_or("too many");
    let vertices_end = count(size)?
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(30))
        .ok_or("too many vertices")?;
    let vertices = data.get(30..vertices_end).ok_or("its vertices cut short")?;
    let tile_flags = data
        .get(vertices_end..vertices_end + count(tiles)?)
        .ok_or("its tiles cut short")?;
    Ok(WmoLiquid {
        size,
        tiles,
        corner: f32s(data, 16)?,
        material: u16_at(data, 28)?,
        heights: records(vertices, 8, |vertex| f32_at(vertex, 4))?,
        data: records(vertices, 8, |vertex| Ok([vertex[0], vertex[1], vertex[2], vertex[3]]))?,
        tile_flags: tile_flags.to_vec(),
    })
}

/// Checks what `group` refers to, within a building of `materials`, `doodads` and `portal_refs`:
/// a batch out of its triangles or vertices, or of a material it does not have, drawn as nothing;
/// a triangle of a material it does not have, only collided with; a doodad it does not have, left
/// out. Each fault said once by `fault`.
fn check_group(
    group: &mut WmoGroup,
    materials: usize,
    doodads: usize,
    portal_refs: usize,
    fault: &mut dyn FnMut(String),
) {
    let vertices = group.vertices.len();
    if group.normals.len() != vertices {
        fault(format!("{} normals for {vertices} vertices", group.normals.len()));
    }
    for (kind, lengths) in [
        (
            "coordinates",
            group.coordinates.iter().map(Vec::len).collect::<Vec<_>>(),
        ),
        ("colours", group.colours.iter().map(Vec::len).collect()),
    ] {
        if let Some(length) = lengths.iter().find(|length| **length != vertices) {
            fault(format!("a set of {length} {kind} for {vertices} vertices"));
        }
    }
    let triangles = group.triangles.len();
    let beyond = group
        .triangles
        .iter()
        .filter(|index| usize::from(**index) >= vertices)
        .count();
    if !triangles.is_multiple_of(3) || beyond > 0 {
        fault(format!(
            "{triangles} indices of triangles, {beyond} past its {vertices} vertices"
        ));
    }
    if group.faces.len() != triangles / 3 {
        fault(format!("{} faces for {} triangles", group.faces.len(), triangles / 3));
    }
    let mut unknown = 0;
    for face in &mut group.faces {
        if face.material.is_some_and(|material| usize::from(material) >= materials) {
            face.material = None;
            unknown += 1;
        }
    }
    if unknown > 0 {
        fault(format!("{unknown} triangles of a material past its {materials}"));
    }
    for (index, batch) in group.batches.iter_mut().enumerate() {
        let end = u64::from(batch.first) + u64::from(batch.count);
        if end > triangles as u64
            || usize::from(batch.vertices[1]) >= vertices
            || usize::from(batch.material) >= materials
        {
            fault(format!(
                "its batch {index} (the indices {} to {end} of {triangles}, the vertices {:?} of {vertices}, \
                 the material {} of {materials}) drawn as nothing",
                batch.first, batch.vertices, batch.material
            ));
            batch.count = 0;
        }
    }
    let held = group.doodad_refs.len();
    group.doodad_refs.retain(|doodad| usize::from(*doodad) < doodads);
    if group.doodad_refs.len() != held {
        fault(format!(
            "{} of its doodads past the {doodads} of the building",
            held - group.doodad_refs.len()
        ));
    }
    let [first, count] = group.portals.map(usize::from);
    if first + count > portal_refs {
        fault(format!(
            "its portals {first} to {} past the {portal_refs} references",
            first + count
        ));
    }
}
