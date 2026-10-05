//! The M2 models of the client at rest, with their skins (`.skin`): of 3.3.5a (version 264), the
//! layouts of warcraft-rs (wow-m2 0.7.0: MIT) corrected by the description of the format
//! (wowdev.wiki); modern (`MD21`, up to version 274), their skins and textures named by FileDataID
//! (`SFID`, `TXID`) as the loader of wow.export reads them (MIT, see THIRD_PARTY.md). What a batch
//! refers to is checked here, once, so that its readers index without a check: a batch referring
//! to what its model does not have is left out, a skin that does not hold together too, with the
//! next ones, and said in `Model::faults`. The bones and the animations wait for step 9.5.
//!
//! Corrected from warcraft-rs: the triangles of a skin index its list of vertices, which indexes
//! those of the model, where warcraft-rs takes them for the model's; a submesh starts at its
//! `indexStart` plus its `level` × 65,536, which warcraft-rs leaves out; the render flags 0x08 and
//! 0x10 are without depth test and without depth write.

use uniwow_api::formats::{
    Batch, FileRef, Material, Model, ModelTexture, ModelTextureSource, ModelVertex, Skin, Submesh,
};

use crate::terrain::{f32_at, f32s, u16_at, u32_at};

/// The versions read: 264 that of 3.3.5a, the next ones modern.
const VERSIONS: std::ops::RangeInclusive<u32> = 264..=274;

/// The flag of a model whose batches combine their textures by `combiner_combos`.
const COMBINERS: u32 = 0x08;
/// The flag of a sequence whose keys are in the model, not in an `.anim` file.
const EMBEDDED: u32 = 0x20;
/// The sizes of a vertex, a texture, a colour, a track, a sequence, a submesh and a batch.
const VERTEX: usize = 48;
const TEXTURE: usize = 16;
const COLOUR: usize = 40;
const TRACK: usize = 20;
const SEQUENCE: usize = 64;
const SUBMESH: usize = 48;
const BATCH: usize = 24;
/// What a fixed-point number of a track is divided by.
const FIXED16: f32 = 32767.0;

/// A model read without its skins, how many views it has, and the FileDataIDs of its skins when
/// modern: of its views, then of its levels of detail.
pub struct Parsed {
    pub model: Model,
    pub views: u32,
    pub skin_ids: Vec<u32>,
}

/// A skin to read: its FileDataID when the model gives it, and its path by the name of the model.
pub struct SkinRef {
    pub id: Option<u32>,
    pub path: String,
}

/// The path of a model as a table names it: `.mdx` and `.mdl` are read as `.m2`.
pub fn path(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match [".mdx", ".mdl"].iter().find(|old| lower.ends_with(*old)) {
        Some(old) => format!("{}.m2", &name[..name.len() - old.len()]),
        None => name.to_owned(),
    }
}

/// The model in `bytes`, of path `path`, with every skin `skin` reads; refused without its first.
pub fn read(bytes: &[u8], path: &str, skin: impl Fn(SkinRef) -> Result<Vec<u8>, String>) -> Result<Model, String> {
    let Parsed {
        mut model,
        views,
        skin_ids,
    } = self::model(bytes)?;
    let stem = match path.get(path.len().saturating_sub(3)..) {
        Some(extension) if extension.eq_ignore_ascii_case(".m2") => &path[..path.len() - 3],
        _ => path,
    };
    // Its views, then the levels of detail a modern model adds after them.
    for view in 0..skin_ids.len().max(views as usize) {
        let id = skin_ids.get(view).copied().filter(|id| *id != 0);
        let path = match view.checked_sub(views as usize) {
            None => format!("{stem}{view:02}.skin"),
            Some(lod) => format!("{stem}_lod{:02}.skin", lod + 1),
        };
        let mut faults = Vec::new();
        match skin(SkinRef { id, path }).and_then(|bytes| self::skin(&bytes, &model, &mut faults)) {
            Ok(read) => model.skins.push(read),
            Err(reason) if view == 0 => return Err(format!("its skin 0: {reason}")),
            Err(reason) => {
                model
                    .faults
                    .push(format!("its skin {view} and the next ones left out: {reason}"));
                break;
            }
        }
        model
            .faults
            .extend(faults.into_iter().map(|fault| format!("its skin {view}: {fault}")));
    }
    Ok(model)
}

/// An `M2Array`: how many, and where, from the start of its model or skin.
fn array(bytes: &[u8], at: usize) -> Result<(usize, usize), String> {
    Ok((u32_at(bytes, at)? as usize, u32_at(bytes, at + 4)? as usize))
}

/// The bytes of `count` items of `size` bytes at `offset`.
fn items<'a>(bytes: &'a [u8], (count, offset): (usize, usize), size: usize, what: &str) -> Result<&'a [u8], String> {
    count
        .checked_mul(size)
        .and_then(|length| bytes.get(offset..offset.checked_add(length)?))
        .ok_or_else(|| format!("its {what}, {count} at byte {offset}, out of the file"))
}

fn u16s(bytes: &[u8], at: usize, what: &str) -> Result<Vec<u16>, String> {
    let found = items(bytes, array(bytes, at)?, 2, what)?;
    Ok(found
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect())
}

/// The chunks of a file: the name of each, as written, and its bytes.
type Chunks<'a> = Vec<([u8; 4], &'a [u8])>;

/// The chunks of a modern model.
fn chunks(bytes: &[u8]) -> Result<Chunks<'_>, String> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(header) = bytes.get(at..at + 8) {
        let name = [header[0], header[1], header[2], header[3]];
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let data = bytes
            .get(at + 8..at + 8 + size)
            .ok_or_else(|| format!("its chunk {} cut short", String::from_utf8_lossy(&name)))?;
        found.push((name, data));
        at += 8 + size;
    }
    Ok(found)
}

fn ids(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect()
}

/// The model in `bytes`, of 3.3.5a (`MD20`) or modern (`MD21`), without its skins.
pub fn model(bytes: &[u8]) -> Result<Parsed, String> {
    let (data, skin_ids, texture_ids) = match bytes.get(..4) {
        Some(b"MD20") => (bytes, Vec::new(), None),
        Some(b"MD21") => {
            let found = chunks(bytes)?;
            let chunk = |name: &[u8; 4]| found.iter().find(|(found, _)| found == name).map(|(_, data)| *data);
            let data = chunk(b"MD21").ok_or("its chunk MD21 missing")?;
            (
                data,
                chunk(b"SFID").map(ids).unwrap_or_default(),
                chunk(b"TXID").map(ids),
            )
        }
        _ => return Err("not an M2".to_owned()),
    };
    if data.get(..4) != Some(b"MD20") {
        return Err("its header is not MD20".to_owned());
    }
    let version = u32_at(data, 4)?;
    if !VERSIONS.contains(&version) {
        return Err(format!("version {version}, where 264 to 274 are read"));
    }
    let flags = u32_at(data, 0x10)?;
    let sequences = items(data, array(data, 0x1C)?, SEQUENCE, "sequences")?;
    // The keys at rest: those of the first sequence, when it holds them.
    let first_held = sequences.len() >= SEQUENCE && u32_at(sequences, 12)? & EMBEDDED != 0;
    let vertices = items(data, array(data, 0x3C)?, VERTEX, "vertices")?
        .as_chunks::<VERTEX>()
        .0
        .iter()
        .map(vertex)
        .collect::<Result<_, _>>()?;
    let views = u32_at(data, 0x44)?;
    let (colour_count, colour_offset) = array(data, 0x48)?;
    items(data, (colour_count, colour_offset), COLOUR, "colours")?;
    let colours = (0..colour_count)
        .map(|index| {
            let at = colour_offset + index * COLOUR;
            let colour = rest(data, at, first_held, 12, [1.0; 3], |v| f32s::<3>(v, 0))?;
            let alpha = rest(data, at + TRACK, first_held, 2, 1.0, fixed16)?;
            Ok([colour[0], colour[1], colour[2], alpha])
        })
        .collect::<Result<_, String>>()?;
    let (texture_count, texture_offset) = array(data, 0x50)?;
    items(data, (texture_count, texture_offset), TEXTURE, "textures")?;
    if let Some(found) = &texture_ids
        && found.len() < texture_count
    {
        return Err(format!(
            "{} FileDataIDs in TXID for {texture_count} textures",
            found.len()
        ));
    }
    let textures = (0..texture_count)
        .map(|index| {
            let at = texture_offset + index * TEXTURE;
            let kind = u32_at(data, at)?;
            let flags = u32_at(data, at + 4)?;
            let source = match (kind, texture_ids.as_ref().map(|found| found[index])) {
                (0, Some(id)) if id != 0 => ModelTextureSource::File(FileRef::Id(id)),
                (0, _) => {
                    let name = items(data, array(data, at + 8)?, 1, "name of a texture")?;
                    let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                    match name.is_empty() {
                        true => ModelTextureSource::Unnamed,
                        false => ModelTextureSource::File(FileRef::Path(String::from_utf8_lossy(name).into_owned())),
                    }
                }
                (kind, _) => ModelTextureSource::Filled(kind),
            };
            Ok(ModelTexture { source, flags })
        })
        .collect::<Result<_, String>>()?;
    let (weight_count, weight_offset) = array(data, 0x58)?;
    items(data, (weight_count, weight_offset), TRACK, "weights")?;
    let weights = (0..weight_count)
        .map(|index| rest(data, weight_offset + index * TRACK, first_held, 2, 1.0, fixed16))
        .collect::<Result<_, _>>()?;
    let materials = items(data, array(data, 0x70)?, 4, "materials")?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|m| Material {
            flags: u16::from_le_bytes([m[0], m[1]]),
            blending: u16::from_le_bytes([m[2], m[3]]),
        })
        .collect();
    let bounds = f32s::<6>(data, 0xA0)?;
    let model = Model {
        version,
        flags,
        vertices,
        textures,
        materials,
        texture_combos: u16s(data, 0x80, "texture combos")?,
        uv_combos: u16s(data, 0x88, "combos of coordinates")?,
        weight_combos: u16s(data, 0x90, "weight combos")?,
        transform_combos: u16s(data, 0x98, "transform combos")?,
        combiner_combos: match flags & COMBINERS {
            0 => Vec::new(),
            _ => u16s(data, 0x130, "combiner combos")?,
        },
        colours,
        weights,
        bounds: [[bounds[0], bounds[1], bounds[2]], [bounds[3], bounds[4], bounds[5]]],
        radius: f32_at(data, 0xB8)?,
        skins: Vec::new(),
        faults: Vec::new(),
    };
    Ok(Parsed { model, views, skin_ids })
}

fn vertex(v: &[u8; VERTEX]) -> Result<ModelVertex, String> {
    Ok(ModelVertex {
        position: f32s(v, 0)?,
        bone_weights: [v[12], v[13], v[14], v[15]],
        bone_indices: [v[16], v[17], v[18], v[19]],
        normal: f32s(v, 20)?,
        uv: [f32s(v, 32)?, f32s(v, 40)?],
    })
}

fn fixed16(value: &[u8]) -> Result<f32, String> {
    Ok(f32::from(u16_at(value, 0)? as i16) / FIXED16)
}

/// The value at rest of the track at `at`, its values of `size` bytes: its first key of the first
/// sequence, or of its global sequence; `default` when it has none, or when the first sequence's
/// keys are in an `.anim` file.
fn rest<T>(
    data: &[u8],
    at: usize,
    first_held: bool,
    size: usize,
    default: T,
    value: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    let global = u16_at(data, at + 2)? != 0xFFFF;
    let sequences = array(data, at + 12)?;
    if sequences.0 == 0 || !(global || first_held) {
        return Ok(default);
    }
    let first = items(data, (1, sequences.1), 8, "keys of a track")?;
    let keys = array(first, 0)?;
    if keys.0 == 0 {
        return Ok(default);
    }
    value(items(data, (1, keys.1), size, "a key of a track")?)
}

/// The skin in `bytes` of `model`, without the batches referring to what the model does not have,
/// said in `faults`.
pub fn skin(bytes: &[u8], model: &Model, faults: &mut Vec<String>) -> Result<Skin, String> {
    if bytes.get(..4) != Some(b"SKIN") {
        return Err("not a skin".to_owned());
    }
    let lookup = u16s(bytes, 4, "vertices")?;
    let indices = u16s(bytes, 12, "triangles")?;
    if !indices.len().is_multiple_of(3) {
        return Err(format!("{} indices, not whole triangles", indices.len()));
    }
    let triangles = indices
        .iter()
        .map(|index| {
            let vertex = *lookup
                .get(usize::from(*index))
                .ok_or_else(|| format!("a triangle to its vertex {index} of {}", lookup.len()))?;
            if usize::from(vertex) >= model.vertices.len() {
                return Err(format!("its vertex {vertex} of {} of the model", model.vertices.len()));
            }
            Ok(u32::from(vertex))
        })
        .collect::<Result<Vec<u32>, String>>()?;
    let submeshes = items(bytes, array(bytes, 28)?, SUBMESH, "submeshes")?
        .as_chunks::<SUBMESH>()
        .0
        .iter()
        .map(|s| {
            let level = u32::from(u16_at(s, 2)?);
            let submesh = Submesh {
                id: u16_at(s, 0)?,
                start: u32::from(u16_at(s, 8)?) + (level << 16),
                count: u32::from(u16_at(s, 10)?),
                centre: f32s(s, 32)?,
                radius: f32_at(s, 44)?,
            };
            if submesh.start as usize + submesh.count as usize > triangles.len() {
                return Err(format!(
                    "its submesh {} past its {} indices",
                    submesh.id,
                    triangles.len()
                ));
            }
            Ok(submesh)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut batches = Vec::new();
    for (index, b) in items(bytes, array(bytes, 36)?, BATCH, "batches")?
        .as_chunks::<BATCH>()
        .0
        .iter()
        .enumerate()
    {
        let colour = u16_at(b, 8)?;
        let batch = Batch {
            flags: b[0],
            priority: b[1] as i8,
            shader: u16_at(b, 2)?,
            submesh: u16_at(b, 4)?,
            colour: (colour != 0xFFFF).then_some(colour),
            material: u16_at(b, 10)?,
            layer: u16_at(b, 12)?,
            texture_count: u16_at(b, 14)?,
            texture_combo: u16_at(b, 16)?,
            uv_combo: u16_at(b, 18)?,
            weight_combo: u16_at(b, 20)?,
            transform_combo: u16_at(b, 22)?,
        };
        match check(&batch, submeshes.len(), model) {
            Ok(()) => batches.push(batch),
            Err(reason) => faults.push(format!("its batch {index} left out, {reason}")),
        }
    }
    Ok(Skin {
        triangles,
        submeshes,
        batches,
    })
}

/// Whether what `batch` refers to is in its skin of `submeshes` and in `model`.
fn check(batch: &Batch, submeshes: usize, model: &Model) -> Result<(), String> {
    let within = |what: &str, index: u16, count: usize| {
        if usize::from(index) < count {
            Ok(())
        } else {
            Err(format!("to its {what} {index} of {count}"))
        }
    };
    within("submesh", batch.submesh, submeshes)?;
    within("material", batch.material, model.materials.len())?;
    if let Some(colour) = batch.colour {
        within("colour", colour, model.colours.len())?;
    }
    let combos = usize::from(batch.texture_combo)..usize::from(batch.texture_combo) + usize::from(batch.texture_count);
    let textures = model
        .texture_combos
        .get(combos)
        .ok_or_else(|| format!("to its texture combos {} and on", batch.texture_combo))?;
    for texture in textures {
        within("texture", *texture, model.textures.len())?;
    }
    within("weight combo", batch.weight_combo, model.weight_combos.len())?;
    within(
        "weight",
        model.weight_combos[usize::from(batch.weight_combo)],
        model.weights.len(),
    )?;
    // Without combos of coordinates, as since Cataclysm, a batch takes its coordinates by its shader.
    match model.uv_combos.is_empty() {
        true => Ok(()),
        false => within("combo of coordinates", batch.uv_combo, model.uv_combos.len()),
    }
}
