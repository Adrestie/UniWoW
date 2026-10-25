//! What the client's tables tell the sheet of a creature: the names of the factions
//! (`FactionTemplate.dbc`, named by `Faction.dbc`) and the models of the displays
//! (`CreatureDisplayInfo.dbc`, `CreatureModelData.dbc`), read once by a job.

use std::collections::HashMap;
use std::sync::Arc;

/// The records of a DBC file of the client: each its fields as numbers, and its block of strings.
pub struct Dbc {
    pub records: Vec<Vec<u32>>,
    strings: Vec<u8>,
}

impl Dbc {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
        if bytes.get(0..4) != Some(b"WDBC") {
            return Err("no DBC".to_owned());
        }
        let (count, fields, size, strings) = (
            word(4).unwrap_or(0) as usize,
            word(8).unwrap_or(0) as usize,
            word(12).unwrap_or(0) as usize,
            word(16).unwrap_or(0) as usize,
        );
        if size < fields * 4 {
            return Err("a DBC whose records are smaller than their fields".to_owned());
        }
        let records = (0..count)
            .map(|record| {
                (0..fields)
                    .filter_map(|field| word(20 + record * size + field * 4))
                    .collect()
            })
            .collect();
        let block = 20 + count * size;
        let strings = bytes.get(block..block + strings).ok_or("a DBC cut short")?.to_vec();
        Ok(Self { records, strings })
    }

    /// The string at `offset` of the block of strings.
    pub fn string(&self, offset: u32) -> String {
        let start = offset as usize;
        if start == 0 || start >= self.strings.len() {
            return String::new();
        }
        let end = self.strings[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(self.strings.len(), |length| start + length);
        String::from_utf8_lossy(&self.strings[start..end]).into_owned()
    }

    /// The name of `record` in the first language that has one, its 16 languages from `first`.
    pub fn name(&self, record: &[u32], first: usize) -> String {
        (first..first + 16)
            .filter_map(|field| record.get(field))
            .map(|offset| self.string(*offset))
            .find(|name| !name.is_empty())
            .unwrap_or_default()
    }
}

/// The fields of the tables as AzerothCore's DBCfmt.h reads them.
const FACTION_NAME: usize = 23;
const TEMPLATE_FACTION: usize = 1;
const DISPLAY_MODEL: usize = 1;
const MODEL_PATH: usize = 2;

/// What the client tells: each template of faction with the name of its faction, by id; the file
/// of each display's model.
#[derive(Clone, Debug, Default)]
pub struct Client {
    pub factions: Vec<(u32, String)>,
    pub models: HashMap<u32, String>,
}

impl Client {
    /// The client's tables read from its archives; a table missing left empty, said.
    pub fn read(vfs: &Arc<dyn uniwow_api::vfs::Vfs>) -> Self {
        let table = |name: &str| -> Option<Dbc> {
            let bytes = vfs.read(&format!(r"DBFilesClient\{name}")).ok().flatten()?;
            match Dbc::read(&bytes) {
                Ok(dbc) => Some(dbc),
                Err(reason) => {
                    uniwow_api::log::warn!("{name} is not read: {reason}");
                    None
                }
            }
        };
        let mut out = Client::default();
        if let (Some(templates), Some(factions)) = (table("FactionTemplate.dbc"), table("Faction.dbc")) {
            out.factions = factions_of(&templates, &factions);
        }
        if let (Some(displays), Some(models)) = (table("CreatureDisplayInfo.dbc"), table("CreatureModelData.dbc")) {
            out.models = models_of(&displays, &models);
        }
        out
    }

    /// The name of the faction of the template `id`.
    pub fn faction(&self, id: u32) -> Option<&str> {
        self.factions
            .binary_search_by_key(&id, |(template, _)| *template)
            .ok()
            .map(|at| self.factions[at].1.as_str())
    }
}

/// Each template of faction, by id, named by its faction.
pub fn factions_of(templates: &Dbc, factions: &Dbc) -> Vec<(u32, String)> {
    let names: HashMap<u32, String> = factions
        .records
        .iter()
        .filter_map(|record| Some((*record.first()?, factions.name(record, FACTION_NAME))))
        .collect();
    let mut out: Vec<(u32, String)> = templates
        .records
        .iter()
        .filter_map(|record| {
            let faction = record.get(TEMPLATE_FACTION)?;
            let name = names.get(faction).cloned().unwrap_or_default();
            Some((
                *record.first()?,
                if name.is_empty() {
                    format!("Faction {faction}")
                } else {
                    name
                },
            ))
        })
        .collect();
    out.sort_by_key(|(id, _)| *id);
    out
}

/// The model file of each display, its folder left out.
pub fn models_of(displays: &Dbc, models: &Dbc) -> HashMap<u32, String> {
    let paths: HashMap<u32, String> = models
        .records
        .iter()
        .filter_map(|record| Some((*record.first()?, models.string(*record.get(MODEL_PATH)?))))
        .collect();
    displays
        .records
        .iter()
        .filter_map(|record| {
            let path = paths.get(record.get(DISPLAY_MODEL)?)?;
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
            let file = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            Some((*record.first()?, file.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DBC of `records` of numbers and `strings` after.
    fn dbc(records: &[Vec<u32>], strings: &[u8]) -> Vec<u8> {
        let fields = records.first().map_or(0, Vec::len) as u32;
        let mut bytes = b"WDBC".to_vec();
        for value in [records.len() as u32, fields, fields * 4, strings.len() as u32] {
            bytes.extend(value.to_le_bytes());
        }
        for record in records {
            for value in record {
                bytes.extend(value.to_le_bytes());
            }
        }
        bytes.extend(strings);
        bytes
    }

    #[test]
    fn a_template_of_faction_is_named_by_its_faction_and_a_display_by_its_model() {
        // Faction: id, 22 fields, then 16 names, the French alone (the 2nd) filled.
        let mut faction = vec![72u32];
        faction.extend([0; 22]);
        faction.extend((0..16).map(|at| if at == 1 { 1 } else { 0 }));
        let factions = Dbc::read(&dbc(&[faction], b"\0Hurlevent\0")).unwrap();
        let templates = Dbc::read(&dbc(&[vec![11, 72, 0], vec![12, 999, 0]], b"\0")).unwrap();
        let named = factions_of(&templates, &factions);
        assert_eq!(named, [(11, "Hurlevent".to_owned()), (12, "Faction 999".to_owned())]);
        let client = Client {
            factions: named,
            ..Client::default()
        };
        assert_eq!(client.faction(12), Some("Faction 999"));
        assert_eq!(client.faction(13), None);

        let models = Dbc::read(&dbc(&[vec![5, 0, 1]], b"\0Creature\\Murloc\\Murloc.mdx\0")).unwrap();
        let displays = Dbc::read(&dbc(&[vec![15, 5], vec![16, 6]], b"\0")).unwrap();
        let files = models_of(&displays, &models);
        assert_eq!(files.get(&15).map(String::as_str), Some("Murloc"));
        assert!(!files.contains_key(&16));
    }
}
