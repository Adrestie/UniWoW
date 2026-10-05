//! The contract of the formats of the client's files, offered by the module `assets`: their content
//! parsed, from any thread at once (T3). For now, the rows of the tables of 3.3.5a the next steps
//! need, the texts in the client's locale.

use std::sync::Arc;

use crate::ServiceKey;

/// A map of `Map.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct MapRecord {
    pub id: u32,
    /// The folder of its files, below `World\Maps`.
    pub directory: String,
    /// 0 a continent, 1 a dungeon, 2 a raid, 3 a battleground, 4 an arena.
    pub instance_type: u32,
    pub name: String,
    /// Its area of `AreaTable.dbc`.
    pub area: u32,
}

/// An area of `AreaTable.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct AreaRecord {
    pub id: u32,
    pub map: u32,
    /// The area it lies in, 0 for a zone.
    pub parent: u32,
    pub flags: u32,
    pub name: String,
}

/// What a creature looks like, `CreatureDisplayInfo.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatureDisplay {
    pub id: u32,
    /// Its model, of `CreatureModelData.dbc`.
    pub model: u32,
    pub scale: f32,
    /// Its skins, the textures replacing those of its model, by name without folder nor extension.
    pub textures: [String; 3],
    /// Its looks of a character, of `CreatureDisplayInfoExtra.dbc`, 0 for none.
    pub extra: u32,
}

/// A model of `CreatureModelData.dbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatureModel {
    pub id: u32,
    pub flags: u32,
    /// Its file, as the table names it.
    pub path: String,
    pub scale: f32,
}

/// The formats of the client's files, offered by the module `assets`. Each table is read once, by
/// the first thread asking for it; the others asking meanwhile wait for it: ask from a job. In
/// debug, a table asked from the interface thread is said once in the log.
pub trait Formats: Send + Sync {
    /// The maps, by increasing id.
    fn maps(&self) -> Result<Arc<Vec<MapRecord>>, String>;
    /// The areas, by increasing id.
    fn areas(&self) -> Result<Arc<Vec<AreaRecord>>, String>;
    /// The looks of creatures, by increasing id.
    fn creature_displays(&self) -> Result<Arc<Vec<CreatureDisplay>>, String>;
    /// The models of creatures, by increasing id.
    fn creature_models(&self) -> Result<Arc<Vec<CreatureModel>>, String>;
}

/// The service of the formats of the client's files.
pub const SERVICE: ServiceKey<Arc<dyn Formats>> = ServiceKey::new("formats");
