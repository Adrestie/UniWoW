//! The component *Materials* of the Inspector (milestone 15, step 15.13b, decision 23): the
//! textures of a model or a building with their paths in the archives, how each material is drawn,
//! and the variants: the textures a creature's display fills, the doodad sets of a building.

use uniwow_api::formats::{FileRef, Model, ModelTextureSource, Wmo};
use uniwow_api::models::Look;
use uniwow_api::serde_json::{Value, json};

/// A file as the Inspector shows it: its path, or its FileDataID.
pub fn file_text(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => path.clone(),
        FileRef::Id(id) => format!("FileDataID {id}"),
    }
}

/// The kinds of texture a display fills, by their number.
fn filled(kind: u32) -> String {
    match kind {
        1 => "the skin of a character".to_owned(),
        2 => "an object".to_owned(),
        6 => "the hair of a character".to_owned(),
        8 => "the fur of a character".to_owned(),
        11..=13 => format!("the skin {} of a creature", kind - 10),
        other => format!("filled, type {other}"),
    }
}

fn named(flags: u32, names: &'static [(u32, &'static str)]) -> Vec<&'static str> {
    names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// The render flags of a material of a model.
const MODEL_FLAGS: [(u32, &str); 5] = [
    (0x01, "unlit"),
    (0x02, "unfogged"),
    (0x04, "two-sided"),
    (0x08, "no depth test"),
    (0x10, "no depth write"),
];

/// The blendings of the models.
const MODEL_BLENDING: [&str; 8] = [
    "opaque",
    "alpha key",
    "alpha",
    "add without alpha",
    "add",
    "mod",
    "mod2x",
    "blend add",
];

/// The flags of a material of a building.
const WMO_FLAGS: [(u32, &str); 8] = [
    (0x01, "unlit"),
    (0x02, "unfogged"),
    (0x04, "two-sided"),
    (0x08, "lit as outside"),
    (0x10, "glowing at night"),
    (0x20, "window"),
    (0x40, "clamped across"),
    (0x80, "clamped up and down"),
];

/// The shaders of the buildings.
const WMO_SHADERS: [&str; 7] = [
    "diffuse",
    "specular",
    "metal",
    "environment",
    "opaque",
    "environment metal",
    "two layers",
];

/// The blendings of the buildings.
const WMO_BLENDING: [&str; 6] = ["opaque", "alpha key", "alpha", "add", "mod", "mod2x"];

fn name_of(names: &[&str], value: u32) -> String {
    names
        .get(value as usize)
        .map_or_else(|| format!("{value}"), |name| (*name).to_owned())
}

/// The materials of the model `model` drawn as `look`: its textures, each by its index, its file
/// (that of the display, or one the user put in its place) and its source; its materials.
pub fn of_model(model: &Model, look: Option<&Look>) -> Value {
    let textures: Vec<Value> = model
        .textures
        .iter()
        .enumerate()
        .map(|(index, texture)| {
            let replaced = look.and_then(|look| {
                look.textures
                    .iter()
                    .find(|(kind, _)| *kind == uniwow_api::models::REPLACED | index as u32)
                    .map(|(_, file)| file_text(file))
            });
            let (file, source) = match &texture.source {
                ModelTextureSource::File(file) => (Some(file_text(file)), "its file".to_owned()),
                ModelTextureSource::Filled(kind) => (
                    look.and_then(|look| {
                        look.textures
                            .iter()
                            .find(|(filled, _)| filled == kind)
                            .map(|(_, file)| file_text(file))
                    }),
                    filled(*kind),
                ),
                ModelTextureSource::Unnamed => (None, "none".to_owned()),
            };
            json!({
                "texture": index,
                "file": replaced.clone().or(file),
                "source": source,
                "replaced": replaced.is_some(),
                "wrapped": [texture.flags & 0x1 != 0, texture.flags & 0x2 != 0],
            })
        })
        .collect();
    let materials: Vec<Value> = model
        .materials
        .iter()
        .map(|material| {
            json!({
                "flags": named(u32::from(material.flags), &MODEL_FLAGS),
                "blending": name_of(&MODEL_BLENDING, u32::from(material.blending)),
            })
        })
        .collect();
    json!({ "textures": textures, "materials": materials })
}

/// The materials of the building `wmo`: its materials, each its textures, flags, shader and
/// blending; its doodad sets.
pub fn of_wmo(wmo: &Wmo) -> Value {
    let materials: Vec<Value> = wmo
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| {
            let textures: Vec<String> = material.textures.iter().flatten().map(file_text).collect();
            json!({
                "material": index,
                "textures": textures,
                "flags": named(material.flags, &WMO_FLAGS),
                "shader": name_of(&WMO_SHADERS, material.shader),
                "blending": name_of(&WMO_BLENDING, material.blending),
            })
        })
        .collect();
    let sets: Vec<Value> = wmo
        .doodad_sets
        .iter()
        .enumerate()
        .map(|(index, set)| json!({ "set": index, "name": set.name, "doodads": set.count }))
        .collect();
    json!({ "materials": materials, "doodad sets": sets })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::formats::{DoodadSet, Material, ModelTexture, WmoMaterial};
    use uniwow_api::models::Geosets;

    #[test]
    fn the_materials_of_a_model_name_their_files_sources_and_drawing() {
        let model = Model {
            textures: vec![
                ModelTexture {
                    source: ModelTextureSource::File(FileRef::Path("world\\lamp.blp".to_owned())),
                    flags: 0x3,
                },
                ModelTexture {
                    source: ModelTextureSource::Filled(11),
                    flags: 0,
                },
            ],
            materials: vec![Material {
                flags: 0x5,
                blending: 2,
            }],
            ..crate::tests::square(0, 0)
        };
        let look = Look {
            model: FileRef::Path("lamp.m2".to_owned()),
            textures: vec![
                (11, FileRef::Path("creature\\wolf\\grey.blp".to_owned())),
                (uniwow_api::models::REPLACED, FileRef::Path("world\\red.blp".to_owned())),
            ],
            geosets: Geosets::All,
        };
        let read = of_model(&model, Some(&look));
        assert_eq!(read["textures"][0]["file"], "world\\red.blp");
        assert_eq!(read["textures"][0]["replaced"], true);
        assert_eq!(read["textures"][0]["wrapped"], json!([true, true]));
        assert_eq!(read["textures"][1]["file"], "creature\\wolf\\grey.blp");
        assert_eq!(read["textures"][1]["source"], "the skin 1 of a creature");
        assert_eq!(read["materials"][0]["flags"], json!(["unlit", "two-sided"]));
        assert_eq!(read["materials"][0]["blending"], "alpha");
        assert_eq!(of_model(&model, None)["textures"][0]["file"], "world\\lamp.blp");
    }

    #[test]
    fn the_materials_of_a_building_name_their_textures_and_its_doodad_sets() {
        let wmo = Wmo {
            materials: vec![WmoMaterial {
                flags: 0x14,
                shader: 3,
                blending: 1,
                textures: [Some(FileRef::Path("dungeons\\inn.blp".to_owned())), None, None],
                emissive: [0; 4],
                diffuse: [0; 4],
                colour: [0; 4],
                ground: 0,
            }],
            doodad_sets: vec![
                DoodadSet {
                    name: "Set_$DefaultGlobal".to_owned(),
                    first: 0,
                    count: 12,
                },
                DoodadSet {
                    name: "Set_Christmas".to_owned(),
                    first: 12,
                    count: 4,
                },
            ],
            ..Wmo::default()
        };
        let read = of_wmo(&wmo);
        assert_eq!(read["materials"][0]["textures"], json!(["dungeons\\inn.blp"]));
        assert_eq!(read["materials"][0]["flags"], json!(["two-sided", "glowing at night"]));
        assert_eq!(read["materials"][0]["shader"], "environment");
        assert_eq!(read["materials"][0]["blending"], "alpha key");
        assert_eq!(
            read["doodad sets"][1],
            json!({ "set": 1, "name": "Set_Christmas", "doodads": 4 })
        );
    }
}
