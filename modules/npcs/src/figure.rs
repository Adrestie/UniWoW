//! What the NPC view draws (step 18.2): the NPC's model as the client dresses it (a creature's skins
//! and variants, or a character's look: race, baked skin, hair) and its weapons on the points of its
//! hands, read from the client's archives by a job and put on the GPU by it.

use std::sync::Arc;

use uniwow_api::formats::{self, FileRef, Formats};
use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::models::{self, Geosets};
use uniwow_api::vfs::Vfs;
use uniwow_api::vfx::anim::{self, Clock};
use uniwow_api::vfx::m2::{self, Model, Skin};
use uniwow_api::vfx::texgen::Image;
use uniwow_api::wgpu;

use crate::catalog::SHIELD;
use crate::client::Client;
use crate::gpu::{self, Gpu};

/// The attachment points of the hands (`M2Attachment.id`): the right, the left, the shield's.
pub const RIGHT_HAND: u32 = 1;
pub const LEFT_HAND: u32 = 2;
pub const SHIELD_POINT: u32 = 0;

/// What to draw: a display, and the weapons of the hands (the display of each item and its kind of
/// slot), none for an empty hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wanted {
    pub display: u32,
    pub hands: [Option<(u32, u8)>; 2],
}

/// A model read: its skin cut to the submeshes it shows, the images of its textures.
pub struct Part {
    pub model: Model,
    pub skin: Skin,
    pub images: Vec<Option<Image>>,
    /// The point of the body it is held at, none for the body.
    pub point: Option<u32>,
}

/// A model on the GPU.
pub struct PartGpu {
    pub model: Model,
    pub skin: Skin,
    pub groups: Vec<Option<wgpu::BindGroup>>,
    pub point: Option<u32>,
}

/// The NPC drawn: its parts, the body first, its scale; what could not be read, said.
pub struct Figure {
    pub wanted: Wanted,
    pub parts: Vec<PartGpu>,
    pub scale: f32,
    pub faults: Vec<String>,
}

/// A path of a table as the archives hold it: `.mdx` and `.mdl` are `.m2`.
fn m2_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase().replace('/', "\\");
    match [".mdx", ".mdl"].iter().find(|old| lower.ends_with(*old)) {
        Some(old) => format!("{}.m2", &lower[..lower.len() - old.len()]),
        None => lower,
    }
}

fn read(vfs: &dyn Vfs, path: &str) -> Result<Vec<u8>, String> {
    vfs.read(path)
        .map_err(|error| format!("{path}: {error}"))?
        .ok_or_else(|| format!("{path} is not in the client"))
}

/// The name a texture of a model gives itself.
fn texture_name(texture: &m2::Texture) -> String {
    let end = texture
        .filename
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(texture.filename.len());
    String::from_utf8_lossy(&texture.filename[..end]).into_owned()
}

fn image(formats: &dyn Formats, file: &FileRef) -> Option<Image> {
    let texture = formats.texture_rgba(file).ok()?;
    let rgba = texture.levels.first()?.clone();
    (rgba.len() >= (texture.width * texture.height * 4) as usize).then_some(Image {
        width: texture.width,
        height: texture.height,
        rgba,
    })
}

/// The model at `path`, its first skin cut by `shown`, its textures: its own files for those it
/// names, `filled` for the kinds it leaves to its display.
fn part(
    vfs: &dyn Vfs,
    formats: &dyn Formats,
    path: &str,
    shown: impl Fn(&[u16]) -> Vec<bool>,
    filled: &[(u32, FileRef)],
    point: Option<u32>,
) -> Result<Part, String> {
    let path = m2_path(path);
    let model = m2::read_without_anims(&read(vfs, &path)?).map_err(|error| format!("{path}: {error}"))?;
    let skin_path = format!("{}00.skin", &path[..path.len() - 3]);
    let mut skin = m2::read_skin(&read(vfs, &skin_path)?).map_err(|error| format!("{skin_path}: {error}"))?;
    let ids: Vec<u16> = skin.submeshes.iter().map(|submesh| submesh.id).collect();
    let shown = shown(&ids);
    skin.batches
        .retain(|batch| shown.get(batch.submesh as usize).copied().unwrap_or(false));
    let images = model
        .textures
        .iter()
        .map(|texture| match texture.kind {
            0 => image(formats, &FileRef::Path(texture_name(texture))),
            kind => filled
                .iter()
                .find(|(filled, _)| *filled == kind)
                .and_then(|(_, file)| image(formats, file)),
        })
        .collect();
    Ok(Part {
        model,
        skin,
        images,
        point,
    })
}

/// Which submeshes of `ids` the look shows, as the module `models` chooses them.
fn shown(ids: &[u16], geosets: Geosets) -> Vec<bool> {
    match geosets {
        Geosets::All | Geosets::Sky | Geosets::Stars => vec![true; ids.len()],
        Geosets::Default => formats::default_geosets(ids),
        Geosets::Creature(chosen) => formats::creature_geosets(ids, chosen),
        Geosets::Character { hair, facial } => {
            let hair = formats::HairGeoset {
                race: 0,
                sex: 0,
                variation: 0,
                geoset: hair,
                scalp: false,
            };
            let facial = formats::FacialHair {
                race: 0,
                sex: 0,
                variation: 0,
                geosets: facial,
            };
            formats::look_geosets(ids, Some(&hair), Some(&facial))
        }
    }
}

/// The parts of `wanted`, read: its body, then each weapon it holds; a weapon that is not read left
/// out, said. From a job.
pub fn read_parts(
    wanted: &Wanted,
    vfs: &dyn Vfs,
    formats: &dyn Formats,
    models: &dyn models::Models,
    client: &Client,
) -> Result<(Vec<Part>, f32, Vec<String>), String> {
    let (look, scale) = models.display(wanted.display)?;
    let FileRef::Path(path) = &look.model else {
        return Err(format!("the display {}: its model is no path", wanted.display));
    };
    let geosets = look.geosets;
    let mut parts = vec![part(
        vfs,
        formats,
        path,
        |ids| shown(ids, geosets),
        &look.textures,
        None,
    )?];
    let mut faults = Vec::new();
    for (hand, held) in wanted.hands.iter().enumerate() {
        let Some((display, slot)) = held else {
            continue;
        };
        let Some(item) = client.items.get(display) else {
            faults.push(format!("the item display {display} is not in ItemDisplayInfo.dbc"));
            continue;
        };
        let folder = if *slot == SHIELD { "Shield" } else { "Weapon" };
        let path = format!("Item\\ObjectComponents\\{folder}\\{}", item.model);
        let texture = FileRef::Path(format!("Item\\ObjectComponents\\{folder}\\{}.blp", item.texture));
        let point = match (hand, *slot) {
            (0, _) => RIGHT_HAND,
            (_, SHIELD) => SHIELD_POINT,
            _ => LEFT_HAND,
        };
        match part(
            vfs,
            formats,
            &path,
            |ids| vec![true; ids.len()],
            &[(2, texture)],
            Some(point),
        ) {
            Ok(part) => parts.push(part),
            Err(reason) => faults.push(reason),
        }
    }
    Ok((parts, scale, faults))
}

/// The parts on the GPU, from the job that read them.
pub fn upload(gpu: &uniwow_api::egui_wgpu::RenderState, resources: &Arc<Gpu>, parts: Vec<Part>) -> Vec<PartGpu> {
    parts
        .into_iter()
        .map(|part| {
            let groups = part
                .images
                .iter()
                .zip(&part.model.textures)
                .map(|(image, texture)| {
                    image.as_ref().map(|image| {
                        gpu::texture_group(
                            &gpu.device,
                            &gpu.queue,
                            &resources.texture_layout,
                            &resources.samplers,
                            image,
                            texture.flags,
                        )
                    })
                })
                .collect();
            PartGpu {
                model: part.model,
                skin: part.skin,
                groups,
                point: part.point,
            }
        })
        .collect()
}

/// The sequence `id` of `model` (Stand is 0), else its first.
pub fn sequence(model: &Model, id: u16) -> usize {
    model.sequences.iter().position(|s| s.id == id).unwrap_or(0)
}

/// The clock of `model`'s Stand at `seconds`, looped over its length.
pub fn standing(model: &Model, seconds: f64) -> Clock {
    let at = sequence(model, 0);
    let length = model.sequences.get(at).map_or(0, |s| s.duration).max(1);
    let millis = seconds * 1000.0;
    Clock {
        sequence: at,
        time: millis % f64::from(length),
        global: millis,
    }
}

/// Where the point `id` of `model` posed as `pose` is, as a matrix from the held model's space.
pub fn point(model: &Model, pose: &[Mat4], id: u32) -> Option<Mat4> {
    let attachment = model.attachments.iter().find(|a| a.id == id)?;
    let bone = pose.get(attachment.bone as usize).copied().unwrap_or_default();
    Some(bone * Mat4::from_translation(Vec3::from(attachment.position)))
}

/// The pose of `model` at `clock`.
pub fn pose(model: &Model, clock: &Clock) -> Vec<Mat4> {
    anim::pose(model, clock, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_of_a_table_is_the_m2_of_the_archives() {
        assert_eq!(m2_path("Creature/Murloc/Murloc.mdx"), "creature\\murloc\\murloc.m2");
        assert_eq!(m2_path("Item\\Weapon\\Sword.M2"), "item\\weapon\\sword.m2");
    }
}
