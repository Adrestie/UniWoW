//! A non-player character kept as a file, a prefab (milestone 18, step 18.1): its sheet (the
//! columns of `creature_template`), its name and title in every language of the client, its models,
//! its weapons, its skills and their texts; JSON a person can read, `<name>.npc.json`. Placed in
//! the world it is one template of the editor's block and as many spawns as the user puts down.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use uniwow_api::serde_json::{self, Value, json};

/// The entries of the templates the editor makes: its block of `ID_RANGES.md`.
pub const RANGE: RangeInclusive<u32> = 82800..=82999;

/// The end of the name of the files.
pub const EXTENSION: &str = ".npc.json";

/// The version of the format, written in each file.
pub const FORMAT: u64 = 1;

/// The languages of the client, enUS the template's own columns.
pub const LOCALES: [&str; 5] = ["enUS", "frFR", "deDE", "esES", "ruRU"];

/// The columns of `creature_template` a prefab starts with: a humanoid of level 1, friendly, a
/// warrior, standing; the others left to the database's defaults.
pub const DEFAULTS: [(&str, f64); 30] = [
    ("minlevel", 1.0),
    ("maxlevel", 1.0),
    ("exp", 0.0),
    ("faction", 35.0),
    ("npcflag", 0.0),
    ("speed_walk", 1.0),
    ("speed_run", 1.14286),
    ("speed_swim", 1.0),
    ("speed_flight", 1.0),
    ("detection_range", 20.0),
    ("rank", 0.0),
    ("dmgschool", 0.0),
    ("DamageModifier", 1.0),
    ("BaseAttackTime", 2000.0),
    ("RangeAttackTime", 2000.0),
    ("unit_class", 1.0),
    ("unit_flags", 0.0),
    ("unit_flags2", 2048.0),
    ("family", 0.0),
    ("type", 7.0),
    ("type_flags", 0.0),
    ("mingold", 0.0),
    ("maxgold", 0.0),
    ("MovementType", 0.0),
    ("HoverHeight", 1.0),
    ("HealthModifier", 1.0),
    ("ManaModifier", 1.0),
    ("ArmorModifier", 1.0),
    ("ExperienceModifier", 1.0),
    ("flags_extra", 0.0),
];

/// A model of the creature (`creature_template_model`): its display, its scale, how often it is
/// the one shown among the others.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub display: u32,
    pub scale: f32,
    pub probability: f32,
}

/// The slots of its weapons (`creature_equip_template.ItemID1..3`).
pub const SLOTS: [(&str, &str, &str); 3] = [
    (
        "main_hand",
        "Main hand",
        "The weapon in its right hand (creature_equip_template.ItemID1)",
    ),
    (
        "off_hand",
        "Off hand",
        "The weapon or shield in its left hand (creature_equip_template.ItemID2)",
    ),
    (
        "ranged",
        "Ranged",
        "Its bow, gun, crossbow or thrown weapon (creature_equip_template.ItemID3)",
    ),
];

/// A non-player character as its file keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefab {
    /// The entry of its template once given one, of `RANGE`.
    pub entry: Option<u32>,
    /// The columns of `creature_template` other than its entry, its name and its title.
    pub template: BTreeMap<String, Value>,
    /// Its name and title by language.
    pub names: BTreeMap<String, (String, String)>,
    pub models: Vec<Model>,
    /// Its weapons, by slot: 0 for none.
    pub equipment: [u32; 3],
    /// Its skills and the texts they say, as step 18.3 writes them.
    pub skills: Vec<Value>,
    pub texts: Vec<Value>,
    /// Its phases, named, when each starts and ends (`phases`).
    pub phases: Vec<Value>,
}

impl Default for Prefab {
    fn default() -> Self {
        Self {
            entry: None,
            template: DEFAULTS
                .iter()
                .map(|(column, value)| ((*column).to_owned(), number(*value)))
                .collect(),
            names: BTreeMap::from([("enUS".to_owned(), ("New NPC".to_owned(), String::new()))]),
            models: Vec::new(),
            equipment: [0; 3],
            skills: Vec::new(),
            texts: Vec::new(),
            phases: Vec::new(),
        }
    }
}

/// A number as JSON: whole when it is.
pub fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

pub fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

impl Prefab {
    /// Its name in `locale`, empty when it has none there.
    pub fn name(&self, locale: &str) -> &str {
        self.names.get(locale).map_or("", |(name, _)| name.as_str())
    }

    pub fn title(&self, locale: &str) -> &str {
        self.names.get(locale).map_or("", |(_, title)| title.as_str())
    }

    /// The column `column` of its template as a number, 0 when it has none.
    /// The column `column` of its template: its own, else the value a prefab starts with, else none
    /// (the database's default).
    pub fn value(&self, column: &str) -> Option<Value> {
        self.template.get(column).cloned().or_else(|| {
            DEFAULTS
                .iter()
                .find(|(default, _)| *default == column)
                .map(|(_, value)| number(*value))
        })
    }

    pub fn whole(&self, column: &str) -> i64 {
        self.value(column)
            .as_ref()
            .and_then(as_number)
            .map_or(0, |value| value as i64)
    }

    pub fn to_json(&self) -> Value {
        let names: serde_json::Map<String, Value> = self
            .names
            .iter()
            .filter(|(_, (name, title))| !name.is_empty() || !title.is_empty())
            .map(|(locale, (name, title))| (locale.clone(), json!({ "name": name, "title": title })))
            .collect();
        let equipment: serde_json::Map<String, Value> = SLOTS
            .iter()
            .zip(self.equipment)
            .map(|((slot, _, _), item)| ((*slot).to_owned(), json!(item)))
            .collect();
        json!({
            "format": FORMAT,
            "entry": self.entry,
            "names": names,
            "template": self.template,
            "models": self.models.iter().map(|model| json!({
                "display": model.display,
                "scale": model.scale,
                "probability": model.probability,
            })).collect::<Vec<_>>(),
            "equipment": equipment,
            "skills": self.skills,
            "texts": self.texts,
            "phases": self.phases,
        })
    }

    pub fn of_json(value: &Value) -> Result<Self, String> {
        let format = value["format"].as_u64().ok_or("no NPC prefab: it has no 'format'")?;
        if format > FORMAT {
            return Err(format!(
                "a prefab of format {format}, made by a newer editor (this one reads {FORMAT})"
            ));
        }
        let list = |key: &str| value[key].as_array().cloned().unwrap_or_default();
        let names = value["names"]
            .as_object()
            .map(|names| {
                names
                    .iter()
                    .map(|(locale, both)| {
                        let text = |key: &str| both[key].as_str().unwrap_or_default().to_owned();
                        (locale.clone(), (text("name"), text("title")))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let template = value["template"]
            .as_object()
            .map(|template| {
                template
                    .iter()
                    .filter(|(column, _)| !matches!(column.as_str(), "entry" | "name" | "subname"))
                    .map(|(column, value)| (column.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let models = list("models")
            .iter()
            .filter_map(|model| {
                Some(Model {
                    display: model["display"].as_u64()? as u32,
                    scale: model["scale"].as_f64().unwrap_or(1.0) as f32,
                    probability: model["probability"].as_f64().unwrap_or(1.0) as f32,
                })
            })
            .collect();
        let mut equipment = [0; 3];
        for (at, (slot, _, _)) in SLOTS.iter().enumerate() {
            equipment[at] = value["equipment"][*slot].as_u64().unwrap_or(0) as u32;
        }
        Ok(Self {
            entry: value["entry"].as_u64().map(|entry| entry as u32),
            template,
            names,
            models,
            equipment,
            skills: list("skills"),
            texts: list("texts"),
            phases: list("phases"),
        })
    }

    /// Its file's text: JSON indented, a line feed at its end.
    pub fn text(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.to_json()).expect("JSON of values");
        text.push('\n');
        text
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|error| format!("{} is no JSON: {error}", path.display()))?;
        Self::of_json(&value).map_err(|reason| format!("{}: {reason}", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        }
        std::fs::write(path, self.text()).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// A name for its file: its English name, letters, digits and spaces kept.
    pub fn file_name(&self) -> String {
        let name: String = self
            .name("enUS")
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = name.trim();
        format!("{}{EXTENSION}", if name.is_empty() { "npc" } else { name })
    }
}

/// The folder of the prefabs: `Documents\UniWoW\Workspace\Prefabs`.
pub fn folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Documents").join("UniWoW").join("Workspace").join("Prefabs")
}

/// The prefabs of `folder` and of its folders, by path, their English name beside; those that are
/// not read left out.
pub fn listed(folder: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(at) = folders.pop() {
        let Ok(read) = std::fs::read_dir(&at) else {
            continue;
        };
        for item in read.flatten() {
            let path = item.path();
            if path.is_dir() {
                folders.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_lowercase().ends_with(EXTENSION))
                && let Ok(prefab) = Prefab::read(&path)
            {
                out.push((path, prefab.name("enUS").to_owned()));
            }
        }
    }
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()).then(a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefab_is_written_as_json_a_person_reads_and_read_back_the_same() {
        let mut prefab = Prefab::default();
        prefab
            .names
            .insert("frFR".to_owned(), ("Garde".to_owned(), "Veilleur".to_owned()));
        prefab.template.insert("npcflag".to_owned(), json!(129));
        prefab.template.insert("speed_run".to_owned(), json!(1.5));
        prefab.models.push(Model {
            display: 16480,
            scale: 1.0,
            probability: 1.0,
        });
        prefab.equipment = [1899, 0, 2504];
        prefab.entry = Some(82800);
        let text = prefab.text();
        assert!(text.contains("\"frFR\": {\n"), "indented: {text}");
        assert!(text.ends_with("}\n"));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["equipment"]["ranged"], 2504);
        assert_eq!(Prefab::of_json(&value).unwrap(), prefab);
        assert_eq!(prefab.file_name(), "New NPC.npc.json");
    }

    #[test]
    fn what_is_not_a_prefab_is_said_and_its_own_columns_are_not_taken_from_its_template() {
        assert!(Prefab::of_json(&json!({ "name": "x" })).is_err());
        assert!(Prefab::of_json(&json!({ "format": 99 })).unwrap_err().contains("newer"));
        let read = Prefab::of_json(&json!({
            "format": 1,
            "template": { "entry": 5, "name": "x", "minlevel": 3 },
        }))
        .unwrap();
        assert_eq!(read.template.len(), 1, "{:?}", read.template);
        assert_eq!(read.whole("minlevel"), 3);
        assert_eq!(
            read.whole("maxlevel"),
            1,
            "not in the file: the value a prefab starts with"
        );
        assert_eq!(read.value("lootid"), None, "the database's default");
        assert!(read.entry.is_none());
    }

    #[test]
    fn the_prefabs_of_a_folder_are_listed_by_name() {
        let folder = std::env::temp_dir().join(format!("uniwow-npcs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let mut prefab = Prefab::default();
        prefab
            .names
            .insert("enUS".to_owned(), ("Zed".to_owned(), String::new()));
        prefab.write(&folder.join("z.npc.json")).unwrap();
        prefab
            .names
            .insert("enUS".to_owned(), ("Abe".to_owned(), String::new()));
        prefab.write(&folder.join("guards").join("a.npc.json")).unwrap();
        std::fs::write(folder.join("other.json"), "{}").unwrap();
        let found: Vec<String> = listed(&folder).into_iter().map(|(_, name)| name).collect();
        assert_eq!(found, ["Abe", "Zed"]);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
