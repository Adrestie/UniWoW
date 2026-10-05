//! The tables of the client, DBC of 3.3.5a: rows of columns of 4 bytes each, then their strings.
//! The places of the columns are those WoWDBDefs gives for 3.3.5.12340 (CC BY-SA 4.0, see
//! THIRD_PARTY.md). Each table is read once, by the first thread asking for it.

use std::sync::{Arc, OnceLock};

use uniwow_api::formats::{AreaRecord, CreatureDisplay, CreatureModel, MapRecord};

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
        }
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
                textures: [dbc.string(row, 6)?, dbc.string(row, 7)?, dbc.string(row, 8)?],
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
}

/// The rows of the table `name` of `chain`, of `columns` columns, by increasing id.
fn read<T>(
    chain: &Chain,
    name: &str,
    columns: usize,
    row: impl Fn(&Dbc, usize) -> Result<T, String>,
    id: impl Fn(&T) -> u32,
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
