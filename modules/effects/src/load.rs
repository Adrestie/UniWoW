//! An effect read: a description of forge made into its model here, or a model of the disk or of
//! the client with its skin; the images of its textures, drawn by forge or read from the client.

use std::path::{Path, PathBuf};

use uniwow_api::glam::Vec3;
use uniwow_api::vfs::Vfs;
use uniwow_api::vfx::anim::{self, Clock};
use uniwow_api::vfx::{blp, forge, json, m2};

use crate::Loaded;

/// The bytes of a file: on the disk when it is there, from the client otherwise.
fn read(path: &str, vfs: Option<&dyn Vfs>) -> Result<Vec<u8>, String> {
    if Path::new(path).is_file() {
        return std::fs::read(path).map_err(|e| format!("{path}: {e}"));
    }
    match vfs {
        Some(vfs) => vfs.read(path)?.ok_or_else(|| format!("{path}: not in the client")),
        None => Err(format!("{path}: not on the disk, and no client")),
    }
}

/// The image of a texture of the client.
fn game_image(path: &str, vfs: Option<&dyn Vfs>) -> Option<uniwow_api::vfx::texgen::Image> {
    let bytes = vfs?.read(path).ok()??;
    blp::decode(&bytes).ok()
}

/// The path a texture names, without its ending zero.
fn texture_path(texture: &m2::Texture) -> String {
    let end = texture
        .filename
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(texture.filename.len());
    String::from_utf8_lossy(&texture.filename[..end]).into_owned()
}

/// What was opened: a description to edit (its file, its JSON, how its lines end), or a model.
pub enum Opened {
    Description(PathBuf, json::Value, json::Layout),
    Model(Box<Loaded>),
}

pub fn open(path: &str, vfs: Option<&dyn Vfs>) -> Result<Opened, String> {
    if path.to_lowercase().ends_with(".json") {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let spec = json::parse(&text).map_err(|e| format!("{path}: {e}"))?;
        if spec.has("effects") {
            return Err(format!("{path}: an effect sheet; open one of its models' descriptions"));
        }
        return Ok(Opened::Description(PathBuf::from(path), spec, json::Layout::of(&text)));
    }
    load(path, vfs).map(|loaded| Opened::Model(Box::new(loaded)))
}

/// The model a description makes, the images of its textures with it.
pub fn forged(spec: &json::Value, dir: &Path, source: &str, vfs: Option<&dyn Vfs>) -> Result<Loaded, String> {
    let options = forge::Options {
        dir: dir.to_path_buf(),
        ..forge::Options::default()
    };
    let forged = forge::forge(spec, &options).map_err(|e| format!("refused: {e}"))?;
    let images = forged
        .model
        .textures
        .iter()
        .map(|texture| {
            let name = texture_path(texture);
            match forged
                .textures
                .iter()
                .find(|t| name.to_lowercase().ends_with(&t.file_name))
            {
                Some(made) => Some(made.image.clone()),
                None => game_image(&name, vfs),
            }
        })
        .collect();
    Ok(Loaded {
        name: forged.name,
        source: source.to_owned(),
        model: forged.model,
        skin: Some(forged.skin),
        images,
        notes: forged.notes,
        version: 0,
    })
}

pub fn load(path: &str, vfs: Option<&dyn Vfs>) -> Result<Loaded, String> {
    let lower = path.to_lowercase();
    if !lower.ends_with(".m2") {
        return Err(format!("{path}: a description (.json) or a model (.m2)"));
    }
    let model = m2::read(&read(path, vfs)?).map_err(|e| format!("{path}: {e}"))?;
    let skin_path = format!("{}00.skin", &path[..path.len() - 3]);
    let skin = match read(&skin_path, vfs) {
        Ok(bytes) => Some(m2::read_skin(&bytes).map_err(|e| format!("{skin_path}: {e}"))?),
        Err(_) => None,
    };
    let dir = Path::new(path).parent().map(Path::to_path_buf);
    let images = model
        .textures
        .iter()
        .map(|texture| {
            if texture.kind != 0 {
                return None;
            }
            let name = texture_path(texture);
            // beside the model on the disk (a model written here), or in the client
            let beside = dir
                .as_ref()
                .and_then(|d| Path::new(&name).file_name().map(|f| d.join(f)))
                .filter(|p| p.is_file());
            match beside {
                Some(file) => std::fs::read(file).ok().and_then(|b| blp::decode(&b).ok()),
                None => game_image(&name, vfs),
            }
        })
        .collect();
    let name = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Loaded {
        name,
        source: path.to_owned(),
        model,
        skin,
        images,
        notes: Vec::new(),
        version: 0,
    })
}

/// The attachment points of a character at the start of its Stand: (id, place in its model).
pub fn attachment_points(vfs: Option<&dyn Vfs>, path: &str) -> Result<Vec<(u32, Vec3)>, String> {
    let model = m2::read_without_anims(&read(path, vfs)?).map_err(|e| format!("{path}: {e}"))?;
    let stand = model.sequences.iter().position(|s| s.id == 0).unwrap_or(0);
    let clock = Clock {
        sequence: stand,
        time: 0.0,
        global: 0.0,
    };
    let pose = anim::pose(&model, &clock, None);
    Ok(model
        .attachments
        .iter()
        .map(|a| {
            let m = pose.get(a.bone as usize).copied().unwrap_or_default();
            (a.id, m.transform_point3(Vec3::from(a.position)))
        })
        .collect())
}
