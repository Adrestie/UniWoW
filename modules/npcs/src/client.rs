//! What the client's tables tell the sheet of a creature: the names of the factions
//! (`FactionTemplate.dbc`, named by `Faction.dbc`), the models of the displays
//! (`CreatureDisplayInfo.dbc`, `CreatureModelData.dbc`) and the models, textures and icons of the
//! items (`ItemDisplayInfo.dbc`), read once by a job.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
const ITEM_MODEL: usize = 1;
const ITEM_TEXTURE: usize = 3;
const ITEM_ICON: usize = 5;

/// What an item shows of itself: its model and texture (the files' names alone, their folder
/// following its kind) and its icon (a name of `Interface\Icons`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemDisplay {
    pub model: String,
    pub texture: String,
    pub icon: String,
}

/// A template of faction (`FactionTemplate.dbc`): its id, the names of its faction (`Faction.dbc`)
/// by language of the client, the groups it belongs to, is friendly and hostile to (1 players, 2
/// the Alliance, 4 the Horde, 8 the monsters), the factions it is hostile and friendly to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Faction {
    pub id: u32,
    pub faction: u32,
    pub names: Vec<(String, String)>,
    pub ours: u32,
    pub friends_mask: u32,
    pub enemies_mask: u32,
    pub enemies: [u32; 4],
    pub friends: [u32; 4],
}

/// The factions of the two sides (`Faction.dbc`), and the bits of their groups.
const ALLIANCE: (u32, u32) = (469, 2);
const HORDE: (u32, u32) = (67, 4);

/// How a template stands to a side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stance {
    Friendly,
    Neutral,
    Hostile,
}

impl Faction {
    /// Its name in `locale`, else in the first language that has one, else its faction's id.
    pub fn name(&self, locale: &str) -> String {
        self.names
            .iter()
            .find(|(held, name)| held == locale && !name.is_empty())
            .or_else(|| self.names.iter().find(|(_, name)| !name.is_empty()))
            .map_or_else(|| format!("Faction {}", self.faction), |(_, name)| name.clone())
    }

    /// How it stands to the side of `(faction, group)`.
    /// The group of the players (1) holds both sides.
    fn towards(&self, (faction, group): (u32, u32)) -> Stance {
        let players = group | 1;
        if self.enemies_mask & players != 0 || self.enemies.contains(&faction) {
            Stance::Hostile
        } else if self.friends_mask & players != 0 || self.ours & group != 0 || self.friends.contains(&faction) {
            Stance::Friendly
        } else {
            Stance::Neutral
        }
    }

    pub fn alliance(&self) -> Stance {
        self.towards(ALLIANCE)
    }

    pub fn horde(&self) -> Stance {
        self.towards(HORDE)
    }

    /// The groups it belongs to, named.
    pub fn groups(&self) -> String {
        let names: Vec<&str> = [(1, "players"), (2, "Alliance"), (4, "Horde"), (8, "monsters")]
            .iter()
            .filter(|(bit, _)| self.ours & bit != 0)
            .map(|(_, name)| *name)
            .collect();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    }
}

/// What the client tells: its templates of faction, by id; the file of each display's model; what
/// each item display shows.
#[derive(Clone, Debug, Default)]
pub struct Client {
    pub factions: Vec<Faction>,
    pub models: HashMap<u32, String>,
    pub items: HashMap<u32, ItemDisplay>,
    /// The languages whose archives were read, and the client's own (that of its names).
    pub locales: Vec<String>,
    pub locale: String,
    /// The spells, by increasing id; the files of their icons, by id of `SpellIcon.dbc`.
    pub spells: Vec<Spell>,
    pub spell_icons: HashMap<u32, String>,
    /// The emotes (`Emotes.dbc`), by id, their names.
    pub emotes: Vec<(u32, String)>,
}

/// A spell: its id, its names by language (`Spell.dbc` of each), its rank (as `Rank 2`), its icon
/// (an id of `SpellIcon.dbc`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spell {
    pub id: u32,
    pub names: Vec<(String, String)>,
    pub rank: String,
    pub icon: u32,
}

impl Spell {
    /// Its name in `locale`, else in the first language that has one.
    pub fn name(&self, locale: &str) -> String {
        self.names
            .iter()
            .find(|(held, name)| held == locale && !name.is_empty())
            .or_else(|| self.names.iter().find(|(_, name)| !name.is_empty()))
            .map_or_else(|| format!("Spell {}", self.id), |(_, name)| name.clone())
    }
}

/// A spell as `Spell.dbc` gives it: its id, name, rank and icon.
pub type SpellRow = (u32, String, String, u32);

/// The fields of `Spell.dbc` as AzerothCore's DBCfmt.h reads them: its icon, its name and its rank
/// (16 languages each and a mask).
const SPELL_ICON: usize = 133;
const SPELL_NAME: usize = 136;
const SPELL_RANK: usize = 153;

/// Each spell of `bytes` (a `Spell.dbc`): its id, its name and rank in the first language that has
/// one, its icon. Read field by field: the table is too large to be held whole in each language.
pub fn spells_of(bytes: &[u8]) -> Vec<SpellRow> {
    let word = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
    if bytes.get(0..4) != Some(b"WDBC") {
        return Vec::new();
    }
    let (count, fields, size, strings) = (
        word(4).unwrap_or(0) as usize,
        word(8).unwrap_or(0) as usize,
        word(12).unwrap_or(0) as usize,
        word(16).unwrap_or(0) as usize,
    );
    if fields <= SPELL_RANK + 16 || size < fields * 4 {
        return Vec::new();
    }
    let block = 20 + count * size;
    let Some(table) = bytes.get(block..block + strings) else {
        return Vec::new();
    };
    let string = |offset: u32| -> String {
        let start = offset as usize;
        if start == 0 || start >= table.len() {
            return String::new();
        }
        let end = table[start..]
            .iter()
            .position(|b| *b == 0)
            .map_or(table.len(), |n| start + n);
        String::from_utf8_lossy(&table[start..end]).into_owned()
    };
    let first = |record: usize, from: usize| -> String {
        (from..from + 16)
            .filter_map(|field| word(20 + record * size + field * 4))
            .map(string)
            .find(|text| !text.is_empty())
            .unwrap_or_default()
    };
    (0..count)
        .filter_map(|record| {
            let at = 20 + record * size;
            Some((
                word(at)?,
                first(record, SPELL_NAME),
                first(record, SPELL_RANK),
                word(at + SPELL_ICON * 4).unwrap_or(0),
            ))
        })
        .collect()
}

/// The spells of the client's own `Spell.dbc` (`own`, its custom ones too), named in each language
/// of `named`.
pub fn spells_named(own: &[SpellRow], named: &[(String, Vec<SpellRow>)]) -> Vec<Spell> {
    let by_locale: Vec<(String, HashMap<u32, String>)> = named
        .iter()
        .map(|(locale, spells)| {
            (
                locale.clone(),
                spells.iter().map(|(id, name, _, _)| (*id, name.clone())).collect(),
            )
        })
        .collect();
    let mut out: Vec<Spell> = own
        .iter()
        .map(|(id, name, rank, icon)| {
            let mut names: Vec<(String, String)> = by_locale
                .iter()
                .map(|(locale, names)| (locale.clone(), names.get(id).cloned().unwrap_or_default()))
                .collect();
            if names.iter().all(|(_, held)| held.is_empty()) {
                names.push((String::new(), name.clone()));
            }
            Spell {
                id: *id,
                names,
                rank: rank.clone(),
                icon: *icon,
            }
        })
        .collect();
    out.sort_by_key(|spell| spell.id);
    out
}

/// The archives of each language of the client in `data` (its folder `Data`), the one read first
/// first: the patches of the letters from z, of the numbers from 9, the language's patch, then its
/// tables of the expansions and its own; those switched off (`.disabled`, `.backup…`) left out.
pub fn locale_archives(data: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let mut out = Vec::new();
    for locale in crate::prefab::LOCALES {
        let lower = locale.to_lowercase();
        let folder = data.join(&lower);
        let Ok(read) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut patches: Vec<(String, PathBuf)> = read
            .flatten()
            .filter_map(|item| {
                let name = item.file_name().to_string_lossy().to_lowercase();
                let rest = name.strip_prefix(&format!("patch-{lower}-"))?.strip_suffix(".mpq")?;
                (rest.len() == 1).then(|| (rest.to_owned(), item.path()))
            })
            .collect();
        // the letters after the numbers, each read before those it comes after
        patches.sort_by(|a, b| b.0.cmp(&a.0));
        let mut archives: Vec<PathBuf> = patches.into_iter().map(|(_, path)| path).collect();
        for name in [
            format!("patch-{lower}.mpq"),
            format!("lichking-locale-{lower}.mpq"),
            format!("expansion-locale-{lower}.mpq"),
            format!("locale-{lower}.mpq"),
        ] {
            let path = folder.join(name);
            if path.is_file() {
                archives.push(path);
            }
        }
        if !archives.is_empty() {
            out.push((locale.to_owned(), archives));
        }
    }
    out
}

/// The folder `Data` of the client whose archives are `sources`.
pub fn data_folder(sources: &[PathBuf]) -> Option<PathBuf> {
    sources.iter().find_map(|source| {
        source
            .ancestors()
            .find(|folder| folder.file_name().is_some_and(|name| name.eq_ignore_ascii_case("data")))
            .map(Path::to_path_buf)
    })
}

/// The table `name` of the first of `archives` that holds it.
pub fn read_in(archives: &[PathBuf], name: &str) -> Option<Dbc> {
    Dbc::read(&bytes_in(archives, name)?).ok()
}

/// The bytes of the table `name` of the first of `archives` that holds it.
pub fn bytes_in(archives: &[PathBuf], name: &str) -> Option<Vec<u8>> {
    let path = format!("DBFilesClient\\{name}");
    archives.iter().find_map(|archive| {
        let opened = uniwow_api::mpq::Archive::open(archive).ok()?;
        if !opened.contains(&path) {
            return None;
        }
        opened.read(&path).ok()
    })
}

impl Client {
    /// The client's tables read from its archives, `Faction.dbc` in each of its languages; a table
    /// missing left empty, said.
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
        let mut named: Vec<(String, Dbc)> = Vec::new();
        let sources = vfs.sources();
        out.locale = crate::prefab::LOCALES
            .iter()
            .find(|locale| {
                sources.iter().any(|source| {
                    source
                        .components()
                        .any(|part| part.as_os_str().eq_ignore_ascii_case(locale))
                })
            })
            .map_or("enUS", |locale| *locale)
            .to_owned();
        if let Some(data) = data_folder(&sources) {
            for (locale, archives) in locale_archives(&data) {
                if let Some(dbc) = read_in(&archives, "Faction.dbc") {
                    out.locales.push(locale.clone());
                    named.push((locale, dbc));
                }
            }
        }
        let mut spells_by_locale = Vec::new();
        if let Some(data) = data_folder(&sources) {
            for (locale, archives) in locale_archives(&data) {
                if let Some(bytes) = bytes_in(&archives, "Spell.dbc") {
                    spells_by_locale.push((locale, spells_of(&bytes)));
                }
            }
        }
        let own = vfs
            .read(r"DBFilesClient\Spell.dbc")
            .ok()
            .flatten()
            .map(|bytes| spells_of(&bytes))
            .unwrap_or_default();
        out.spells = spells_named(&own, &spells_by_locale);
        if let Some(icons) = table("SpellIcon.dbc") {
            out.spell_icons = icons
                .records
                .iter()
                .filter_map(|record| Some((*record.first()?, icons.string(*record.get(1)?))))
                .collect();
        }
        if let Some(emotes) = table("Emotes.dbc") {
            out.emotes = emotes
                .records
                .iter()
                .filter_map(|record| Some((*record.first()?, emotes.string(*record.get(1)?))))
                .collect();
            out.emotes.sort_by_key(|(id, _)| *id);
        }
        if named.is_empty()
            && let Some(dbc) = table("Faction.dbc")
        {
            named.push((String::new(), dbc));
        }
        if let Some(templates) = table("FactionTemplate.dbc") {
            out.factions = factions_of(&templates, &named);
        }
        if let (Some(displays), Some(models)) = (table("CreatureDisplayInfo.dbc"), table("CreatureModelData.dbc")) {
            out.models = models_of(&displays, &models);
        }
        if let Some(items) = table("ItemDisplayInfo.dbc") {
            out.items = items_of(&items);
        }
        out
    }

    pub fn spell(&self, id: u32) -> Option<&Spell> {
        self.spells
            .binary_search_by_key(&id, |spell| spell.id)
            .ok()
            .map(|at| &self.spells[at])
    }

    /// The name of the emote `id`, as the client names it (`ONESHOT_TALK(DNR)` read `talk`).
    pub fn emote(&self, id: u32) -> Option<String> {
        let at = self.emotes.binary_search_by_key(&id, |(held, _)| *held).ok()?;
        Some(emote_name(&self.emotes[at].1))
    }

    /// The template of faction `id`.
    pub fn faction(&self, id: u32) -> Option<&Faction> {
        self.factions
            .binary_search_by_key(&id, |faction| faction.id)
            .ok()
            .map(|at| &self.factions[at])
    }
}

/// The name of an emote of `Emotes.dbc` read plainly: `ONESHOT_TALK(DNR)` is `talk`, `STATE_DANCE`
/// is `dance (held)`.
pub fn emote_name(raw: &str) -> String {
    let raw = raw.split('(').next().unwrap_or(raw).trim();
    let (held, rest) = match raw.strip_prefix("STATE_") {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix("ONESHOT_").unwrap_or(raw)),
    };
    let name = rest.replace('_', " ").to_lowercase();
    if held { format!("{name} (held)") } else { name }
}

/// Each template of faction, by id, named by its faction in each language of `named`.
pub fn factions_of(templates: &Dbc, named: &[(String, Dbc)]) -> Vec<Faction> {
    let names: Vec<(String, HashMap<u32, String>)> = named
        .iter()
        .map(|(locale, dbc)| {
            let names = dbc
                .records
                .iter()
                .filter_map(|record| Some((*record.first()?, dbc.name(record, FACTION_NAME))))
                .collect();
            (locale.clone(), names)
        })
        .collect();
    let mut out: Vec<Faction> = templates
        .records
        .iter()
        .filter_map(|record| {
            let field = |at: usize| record.get(at).copied().unwrap_or(0);
            let faction = *record.get(TEMPLATE_FACTION)?;
            Some(Faction {
                id: *record.first()?,
                faction,
                names: names
                    .iter()
                    .map(|(locale, names)| (locale.clone(), names.get(&faction).cloned().unwrap_or_default()))
                    .collect(),
                ours: field(3),
                friends_mask: field(4),
                enemies_mask: field(5),
                enemies: [field(6), field(7), field(8), field(9)],
                friends: [field(10), field(11), field(12), field(13)],
            })
        })
        .collect();
    out.sort_by_key(|faction| faction.id);
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

/// Each item display's model, texture and icon.
pub fn items_of(items: &Dbc) -> HashMap<u32, ItemDisplay> {
    items
        .records
        .iter()
        .filter_map(|record| {
            let text = |field: usize| {
                record
                    .get(field)
                    .map(|offset| items.string(*offset))
                    .unwrap_or_default()
            };
            Some((
                *record.first()?,
                ItemDisplay {
                    model: text(ITEM_MODEL),
                    texture: text(ITEM_TEXTURE),
                    icon: text(ITEM_ICON),
                },
            ))
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
        // id, faction, flags, ours, friends, enemies, 4 enemies, 4 friends
        let templates = Dbc::read(&dbc(
            &[
                vec![11, 72, 0, 3, 2, 4, 0, 0, 0, 0, 0, 0, 0, 0],
                vec![12, 999, 0, 8, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            ],
            b"\0",
        ))
        .unwrap();
        let named = factions_of(&templates, &[("frFR".to_owned(), factions)]);
        assert_eq!(named[0].name("deDE"), "Hurlevent", "the first language that has one");
        assert_eq!(named[1].name("frFR"), "Faction 999");
        assert_eq!(
            (named[0].alliance(), named[0].horde(), named[0].groups()),
            (Stance::Friendly, Stance::Hostile, "players, Alliance".to_owned())
        );
        assert_eq!(
            (named[1].alliance(), named[1].horde()),
            (Stance::Hostile, Stance::Hostile)
        );
        let client = Client {
            factions: named,
            ..Client::default()
        };
        assert_eq!(client.faction(12).map(|f| f.faction), Some(999));
        assert!(client.faction(13).is_none());

        let models = Dbc::read(&dbc(&[vec![5, 0, 1]], b"\0Creature\\Murloc\\Murloc.mdx\0")).unwrap();
        let displays = Dbc::read(&dbc(&[vec![15, 5], vec![16, 6]], b"\0")).unwrap();
        let files = models_of(&displays, &models);
        assert_eq!(files.get(&15).map(String::as_str), Some("Murloc"));
        assert!(!files.contains_key(&16));

        let items = Dbc::read(&dbc(
            &[vec![1542, 1, 0, 25, 0, 49]],
            b"\0Sword_1H_Short_A_01.mdx\0Sword_1H_Short_A_01Blue\0INV_Sword_04\0",
        ))
        .unwrap();
        assert_eq!(emote_name("ONESHOT_TALK(DNR)"), "talk");
        assert_eq!(emote_name("STATE_DANCE"), "dance (held)");
        let mut record = vec![133u32];
        record.extend(vec![0u32; 132]);
        record.push(7); // its icon
        record.extend([0, 0]);
        record.extend((0..17).map(|at| if at == 2 { 1 } else { 0 })); // its name, the French
        record.extend((0..17).map(|_| 0));
        record.extend(vec![0u32; 30]);
        let spells = spells_of(&dbc(&[record], b"\0Boule de feu\0"));
        assert_eq!(spells, [(133, "Boule de feu".to_owned(), String::new(), 7)]);
        let named = spells_named(&spells, &[("frFR".to_owned(), spells.clone())]);
        assert_eq!(named[0].name("enUS"), "Boule de feu");
        let shown = items_of(&items);
        assert_eq!(
            shown[&1542],
            ItemDisplay {
                model: "Sword_1H_Short_A_01.mdx".to_owned(),
                texture: "Sword_1H_Short_A_01Blue".to_owned(),
                icon: "INV_Sword_04".to_owned(),
            }
        );
    }
}
