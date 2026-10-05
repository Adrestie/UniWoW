//! The look of a creature display, as the client builds it from its tables: its model, the skins
//! it gives the model in the folder of the model, and the submeshes it shows; a character's look,
//! whose display names the model of its race and sex, with its skin baked into one texture, the
//! texture of its hair, and its hair and facial hair. Its equipment is not drawn in this milestone.

use uniwow_api::formats::{FileRef, Formats};
use uniwow_api::models::{Geosets, Look};

/// The kinds of texture a display fills: a creature's three skins, a character's skin and hair.
pub const CREATURE_SKIN: u32 = 11;
pub const CHARACTER_SKIN: u32 = 1;
pub const HAIR: u32 = 6;
/// The section of `CharSections` holding a hair's texture, and the folder of the baked skins.
const HAIR_SECTION: u32 = 3;
const BAKED: &str = "Textures\\BakedNpcTextures\\";

/// The row of `rows`, sorted by `key`, whose key is `id`.
fn find<T>(rows: &[T], id: u32, key: impl Fn(&T) -> u32) -> Option<&T> {
    rows.binary_search_by_key(&id, key).ok().map(|at| &rows[at])
}

/// The folder of `path`, with its separator.
fn folder(path: &str) -> &str {
    path.rfind(['\\', '/']).map_or("", |at| &path[..=at])
}

/// A scale of a table: 1 where it gives none.
fn scale(value: f32) -> f32 {
    if value > 0.0 { value } else { 1.0 }
}

/// The look of the display `id`, and its scale: that of the display times that of its model.
pub fn display(formats: &dyn Formats, id: u32) -> Result<(Look, f32), String> {
    let displays = formats.creature_displays()?;
    let display = find(&displays, id, |display| display.id)
        .ok_or_else(|| format!("the display {id}: not in CreatureDisplayInfo"))?;
    let models = formats.creature_models()?;
    let model = find(&models, display.model, |model| model.id)
        .ok_or_else(|| format!("the display {id}: its model {} not in CreatureModelData", display.model))?;
    let scale = scale(display.scale) * scale(model.scale);
    let path = model.path.clone();
    if display.extra == 0 {
        let textures = display
            .textures
            .iter()
            .enumerate()
            .filter(|(_, name)| !name.is_empty())
            .map(|(slot, name)| {
                let file = format!("{}{name}.blp", folder(&path));
                (CREATURE_SKIN + slot as u32, FileRef::Path(file))
            })
            .collect();
        let geosets = match display.geosets {
            0 => Geosets::All,
            chosen => Geosets::Creature(chosen),
        };
        return Ok((
            Look {
                model: FileRef::Path(path),
                textures,
                geosets,
            },
            scale,
        ));
    }
    let looks = formats.creature_looks()?;
    let look = find(&looks, display.extra, |look| look.id).ok_or_else(|| {
        format!(
            "the display {id}: its look {} not in CreatureDisplayInfoExtra",
            display.extra
        )
    })?;
    let style = |race, sex, variation| (race, sex, variation);
    let wanted = style(look.race, look.sex, look.hair_style);
    let hairs = formats.hair_geosets()?;
    let hair = hairs
        .iter()
        .find(|hair| style(hair.race, hair.sex, hair.variation) == wanted)
        .map_or(0, |hair| hair.geoset);
    let facials = formats.facial_hairs()?;
    let facial = facials
        .iter()
        .find(|facial| style(facial.race, facial.sex, facial.variation) == (look.race, look.sex, look.facial_hair))
        .map_or([0; 5], |facial| facial.geosets);
    let mut textures = Vec::new();
    if !look.baked.is_empty() {
        textures.push((CHARACTER_SKIN, FileRef::Path(format!("{BAKED}{}", look.baked))));
    }
    let sections = formats.char_sections()?;
    if let Some(texture) = sections
        .iter()
        .find(|section| {
            (
                section.race,
                section.sex,
                section.section,
                section.variation,
                section.colour,
            ) == (look.race, look.sex, HAIR_SECTION, look.hair_style, look.hair_colour)
        })
        .map(|section| &section.textures[0])
        .filter(|texture| !texture.is_empty())
    {
        textures.push((HAIR, FileRef::Path(texture.clone())));
    }
    Ok((
        Look {
            model: FileRef::Path(path),
            textures,
            geosets: Geosets::Character { hair, facial },
        },
        scale,
    ))
}
