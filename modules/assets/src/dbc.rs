//! The tables of the client, DBC of 3.3.5a: rows of columns of 4 bytes each, then their strings.
//! The places of the columns are those WoWDBDefs gives for 3.3.5.12340 (CC BY-SA 4.0, see
//! THIRD_PARTY.md). Each table is read once, by the first thread asking for it.

use std::sync::{Arc, OnceLock};

use uniwow_api::formats::{
    AnimationRecord, AreaRecord, CharSection, CreatureDisplay, CreatureLook, CreatureModel, FacialHair,
    GameObjectDisplay, HairGeoset, LightBand, LightParamsRecord, LightRecord, LightSkyboxRecord, LiquidTypeRecord,
    MapRecord,
};

use crate::chain::Chain;

/// The locales of 3.3.5a, in the order of the strings of a localised text; enGB shares the first.
const LOCALES: [&str; 9] = ["enUS", "koKR", "frFR", "deDE", "zhCN", "zhTW", "esES", "esMX", "ruRU"];

/// A DBC: its rows, of `columns` columns, then its strings.
pub struct Dbc<'a> {
    bytes: &'a [u8],
    rows: usize,
    columns: usize,
    strings: usize,
}

impl<'a> Dbc<'a> {
    /// The table in `bytes`, refused unless its rows have `columns` columns of 4 bytes.
    pub fn parse(bytes: &'a [u8], columns: usize) -> Result<Self, String> {
        let header = |at: usize| {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        if bytes.get(..4) != Some(b"WDBC") {
            return Err("not a DBC".to_owned());
        }
        let (Some(rows), Some(found), Some(row_size), Some(strings_size)) =
            (header(4), header(8), header(12), header(16))
        else {
            return Err("cut short in its header".to_owned());
        };
        if found != columns || row_size != columns * 4 {
            return Err(format!(
                "{found} columns in rows of {row_size} bytes, where 3.3.5a has {columns} of 4 bytes"
            ));
        }
        let strings = 20 + rows * row_size;
        if strings + strings_size > bytes.len() {
            return Err("cut short".to_owned());
        }
        Ok(Self {
            bytes: &bytes[..strings + strings_size],
            rows,
            columns,
            strings,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn u32(&self, row: usize, column: usize) -> u32 {
        let at = 20 + (row * self.columns + column) * 4;
        u32::from_le_bytes([
            self.bytes[at],
            self.bytes[at + 1],
            self.bytes[at + 2],
            self.bytes[at + 3],
        ])
    }

    pub fn f32(&self, row: usize, column: usize) -> f32 {
        f32::from_bits(self.u32(row, column))
    }

    pub fn string(&self, row: usize, column: usize) -> Result<String, String> {
        let rest = &self.bytes[self.strings..];
        let offset = self.u32(row, column) as usize;
        let text = rest
            .get(offset..)
            .and_then(|text| Some(&text[..text.iter().position(|byte| *byte == 0)?]))
            .ok_or_else(|| format!("a string at {offset}, out of its strings"))?;
        Ok(String::from_utf8_lossy(text).into_owned())
    }
}

type Rows<T> = OnceLock<Result<Arc<Vec<T>>, String>>;

/// The tables of the client, each read once, its texts in the locale of the client.
pub struct Tables {
    /// The place of the locale among the strings of a localised text.
    slot: usize,
    maps: Rows<MapRecord>,
    areas: Rows<AreaRecord>,
    displays: Rows<CreatureDisplay>,
    models: Rows<CreatureModel>,
    looks: Rows<CreatureLook>,
    hairs: Rows<HairGeoset>,
    facial_hairs: Rows<FacialHair>,
    objects: Rows<GameObjectDisplay>,
    sections: Rows<CharSection>,
    animations: Rows<AnimationRecord>,
    liquids: Rows<LiquidTypeRecord>,
    lights: Rows<LightRecord>,
    light_params: Rows<LightParamsRecord>,
    light_colours: Rows<LightBand<[u8; 3]>>,
    light_numbers: Rows<LightBand<f32>>,
    light_skyboxes: Rows<LightSkyboxRecord>,
}

impl Tables {
    pub fn new(locale: &str) -> Self {
        Self {
            slot: LOCALES
                .iter()
                .position(|known| known.eq_ignore_ascii_case(locale))
                .unwrap_or(0),
            maps: OnceLock::new(),
            areas: OnceLock::new(),
            displays: OnceLock::new(),
            models: OnceLock::new(),
            looks: OnceLock::new(),
            hairs: OnceLock::new(),
            facial_hairs: OnceLock::new(),
            objects: OnceLock::new(),
            sections: OnceLock::new(),
            animations: OnceLock::new(),
            liquids: OnceLock::new(),
            lights: OnceLock::new(),
            light_params: OnceLock::new(),
            light_colours: OnceLock::new(),
            light_numbers: OnceLock::new(),
            light_skyboxes: OnceLock::new(),
        }
    }

    pub fn lights(&self, chain: &Chain) -> Result<Arc<Vec<LightRecord>>, String> {
        self.lights
            .get_or_init(|| {
                let row = |dbc: &Dbc, row| {
                    Ok(LightRecord {
                        id: dbc.u32(row, 0),
                        map: dbc.u32(row, 1),
                        position: [dbc.f32(row, 2), dbc.f32(row, 3), dbc.f32(row, 4)],
                        radii: [dbc.f32(row, 5), dbc.f32(row, 6)],
                        params: std::array::from_fn(|slot| dbc.u32(row, 7 + slot)),
                    })
                };
                read(chain, "Light.dbc", 15, row, |light| light.id)
            })
            .clone()
    }

    pub fn light_params(&self, chain: &Chain) -> Result<Arc<Vec<LightParamsRecord>>, String> {
        self.light_params
            .get_or_init(|| {
                let row = |dbc: &Dbc, row| {
                    Ok(LightParamsRecord {
                        id: dbc.u32(row, 0),
                        highlight_sky: dbc.u32(row, 1) != 0,
                        skybox: dbc.u32(row, 2),
                        cloud: dbc.u32(row, 3),
                        glow: dbc.f32(row, 4),
                        river_alphas: [dbc.f32(row, 5), dbc.f32(row, 6)],
                        ocean_alphas: [dbc.f32(row, 7), dbc.f32(row, 8)],
                    })
                };
                read(chain, "LightParams.dbc", 9, row, |params| params.id)
            })
            .clone()
    }

    /// The bands of colours, each stored `0x00RRGGBB`, given red first.
    pub fn light_colours(&self, chain: &Chain) -> Result<Arc<Vec<LightBand<[u8; 3]>>>, String> {
        self.light_colours
            .get_or_init(|| {
                let row = |dbc: &Dbc, row| {
                    Ok(band(dbc, row, |value| {
                        [(value >> 16) as u8, (value >> 8) as u8, value as u8]
                    }))
                };
                read(chain, "LightIntBand.dbc", 34, row, |band| band.id)
            })
            .clone()
    }

    pub fn light_numbers(&self, chain: &Chain) -> Result<Arc<Vec<LightBand<f32>>>, String> {
        self.light_numbers
            .get_or_init(|| {
                let row = |dbc: &Dbc, row| Ok(band(dbc, row, f32::from_bits));
                read(chain, "LightFloatBand.dbc", 34, row, |band| band.id)
            })
            .clone()
    }

    pub fn light_skyboxes(&self, chain: &Chain) -> Result<Arc<Vec<LightSkyboxRecord>>, String> {
        self.light_skyboxes
            .get_or_init(|| {
                let row = |dbc: &Dbc, row| {
                    Ok(LightSkyboxRecord {
                        id: dbc.u32(row, 0),
                        model: dbc.string(row, 1)?,
                        flags: dbc.u32(row, 2),
                    })
                };
                read(chain, "LightSkybox.dbc", 3, row, |skybox| skybox.id)
            })
            .clone()
    }

    /// The types of liquid, each with the format of the vertices of its material.
    pub fn liquid_types(&self, chain: &Chain) -> Result<Arc<Vec<LiquidTypeRecord>>, String> {
        self.liquids
            .get_or_init(|| {
                let formats = read(
                    chain,
                    "LiquidMaterial.dbc",
                    3,
                    |dbc, row| Ok((dbc.u32(row, 0), dbc.u32(row, 1))),
                    |(id, _)| *id,
                )?;
                let row = |dbc: &Dbc, row| {
                    let material = dbc.u32(row, 14);
                    Ok(LiquidTypeRecord {
                        id: dbc.u32(row, 0),
                        name: dbc.string(row, 1)?,
                        kind: dbc.u32(row, 3),
                        material,
                        vertex_format: formats
                            .iter()
                            .find(|(id, _)| *id == material)
                            .map(|(_, format)| *format),
                        textures: [
                            dbc.string(row, 15)?,
                            dbc.string(row, 16)?,
                            dbc.string(row, 17)?,
                            dbc.string(row, 18)?,
                            dbc.string(row, 19)?,
                            dbc.string(row, 20)?,
                        ],
                        animation: [dbc.f32(row, 23), dbc.f32(row, 24)],
                        depth_table: dbc.u32(row, 41),
                        depth_scale: dbc.f32(row, 25),
                    })
                };
                read(chain, "LiquidType.dbc", 45, row, |liquid| liquid.id)
            })
            .clone()
    }

    pub fn animations(&self, chain: &Chain) -> Result<Arc<Vec<AnimationRecord>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(AnimationRecord {
                id: dbc.u32(row, 0),
                name: dbc.string(row, 1)?,
                fallback: dbc.u32(row, 5),
            })
        };
        self.animations
            .get_or_init(|| read(chain, "AnimationData.dbc", 8, row, |animation| animation.id))
            .clone()
    }

    pub fn maps(&self, chain: &Chain) -> Result<Arc<Vec<MapRecord>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(MapRecord {
                id: dbc.u32(row, 0),
                directory: dbc.string(row, 1)?,
                instance_type: dbc.u32(row, 2),
                name: dbc.string(row, 5 + self.slot)?,
                area: dbc.u32(row, 22),
            })
        };
        self.maps
            .get_or_init(|| read(chain, "Map.dbc", 66, row, |map| map.id))
            .clone()
    }

    pub fn areas(&self, chain: &Chain) -> Result<Arc<Vec<AreaRecord>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(AreaRecord {
                id: dbc.u32(row, 0),
                map: dbc.u32(row, 1),
                parent: dbc.u32(row, 2),
                flags: dbc.u32(row, 4),
                name: dbc.string(row, 11 + self.slot)?,
            })
        };
        self.areas
            .get_or_init(|| read(chain, "AreaTable.dbc", 36, row, |area| area.id))
            .clone()
    }

    pub fn creature_displays(&self, chain: &Chain) -> Result<Arc<Vec<CreatureDisplay>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(CreatureDisplay {
                id: dbc.u32(row, 0),
                model: dbc.u32(row, 1),
                extra: dbc.u32(row, 3),
                scale: dbc.f32(row, 4),
                alpha: dbc.u32(row, 5),
                textures: [dbc.string(row, 6)?, dbc.string(row, 7)?, dbc.string(row, 8)?],
                geosets: dbc.u32(row, 14),
            })
        };
        self.displays
            .get_or_init(|| read(chain, "CreatureDisplayInfo.dbc", 16, row, |display| display.id))
            .clone()
    }

    pub fn creature_models(&self, chain: &Chain) -> Result<Arc<Vec<CreatureModel>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(CreatureModel {
                id: dbc.u32(row, 0),
                flags: dbc.u32(row, 1),
                path: dbc.string(row, 2)?,
                scale: dbc.f32(row, 4),
            })
        };
        self.models
            .get_or_init(|| read(chain, "CreatureModelData.dbc", 28, row, |model| model.id))
            .clone()
    }

    pub fn creature_looks(&self, chain: &Chain) -> Result<Arc<Vec<CreatureLook>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(CreatureLook {
                id: dbc.u32(row, 0),
                race: dbc.u32(row, 1),
                sex: dbc.u32(row, 2),
                skin: dbc.u32(row, 3),
                face: dbc.u32(row, 4),
                hair_style: dbc.u32(row, 5),
                hair_colour: dbc.u32(row, 6),
                facial_hair: dbc.u32(row, 7),
                items: std::array::from_fn(|item| dbc.u32(row, 8 + item)),
                flags: dbc.u32(row, 19),
                baked: dbc.string(row, 20)?,
            })
        };
        self.looks
            .get_or_init(|| read(chain, "CreatureDisplayInfoExtra.dbc", 21, row, |look| look.id))
            .clone()
    }

    pub fn hair_geosets(&self, chain: &Chain) -> Result<Arc<Vec<HairGeoset>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(HairGeoset {
                race: dbc.u32(row, 1),
                sex: dbc.u32(row, 2),
                variation: dbc.u32(row, 3),
                geoset: dbc.u32(row, 4),
                scalp: dbc.u32(row, 5) != 0,
            })
        };
        self.hairs
            .get_or_init(|| {
                read(chain, "CharHairGeosets.dbc", 6, row, |hair| {
                    (hair.race, hair.sex, hair.variation)
                })
            })
            .clone()
    }

    pub fn facial_hairs(&self, chain: &Chain) -> Result<Arc<Vec<FacialHair>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(FacialHair {
                race: dbc.u32(row, 0),
                sex: dbc.u32(row, 1),
                variation: dbc.u32(row, 2),
                geosets: std::array::from_fn(|value| dbc.u32(row, 3 + value)),
            })
        };
        self.facial_hairs
            .get_or_init(|| {
                read(chain, "CharacterFacialHairStyles.dbc", 8, row, |facial| {
                    (facial.race, facial.sex, facial.variation)
                })
            })
            .clone()
    }

    pub fn game_object_displays(&self, chain: &Chain) -> Result<Arc<Vec<GameObjectDisplay>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(GameObjectDisplay {
                id: dbc.u32(row, 0),
                path: dbc.string(row, 1)?,
            })
        };
        self.objects
            .get_or_init(|| read(chain, "GameObjectDisplayInfo.dbc", 19, row, |object| object.id))
            .clone()
    }

    pub fn char_sections(&self, chain: &Chain) -> Result<Arc<Vec<CharSection>>, String> {
        let row = |dbc: &Dbc, row| {
            Ok(CharSection {
                race: dbc.u32(row, 1),
                sex: dbc.u32(row, 2),
                section: dbc.u32(row, 3),
                textures: [dbc.string(row, 4)?, dbc.string(row, 5)?, dbc.string(row, 6)?],
                variation: dbc.u32(row, 8),
                colour: dbc.u32(row, 9),
            })
        };
        self.sections
            .get_or_init(|| {
                read(chain, "CharSections.dbc", 10, row, |section| {
                    (
                        section.race,
                        section.sex,
                        section.section,
                        section.variation,
                        section.colour,
                    )
                })
            })
            .clone()
    }
}

/// The band of a light in the row `row`: its count of keys (16 at most), then 16 times, then 16
/// values, each made by `value`; those past its count left out.
fn band<T>(dbc: &Dbc, row: usize, value: impl Fn(u32) -> T) -> LightBand<T> {
    let count = (dbc.u32(row, 1) as usize).min(16);
    LightBand {
        id: dbc.u32(row, 0),
        keys: (0..count)
            .map(|key| (dbc.u32(row, 2 + key), value(dbc.u32(row, 18 + key))))
            .collect(),
    }
}

/// The rows of the table `name` of `chain`, of `columns` columns, sorted by `id`.
fn read<T, K: Ord>(
    chain: &Chain,
    name: &str,
    columns: usize,
    row: impl Fn(&Dbc, usize) -> Result<T, String>,
    id: impl Fn(&T) -> K,
) -> Result<Arc<Vec<T>>, String> {
    let path = format!("DBFilesClient\\{name}");
    let bytes = chain.read(&path)?.ok_or_else(|| format!("{path}: not in the client"))?;
    let dbc = Dbc::parse(&bytes, columns).map_err(|e| format!("{path}: {e}"))?;
    let mut rows = (0..dbc.rows())
        .map(|index| row(&dbc, index))
        .collect::<Result<Vec<T>, String>>()
        .map_err(|e| format!("{path}: {e}"))?;
    rows.sort_by_key(id);
    Ok(Arc::new(rows))
}
