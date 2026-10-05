//! Interface of the "models" service: the M2 models the modules place in the 3D view, each instance
//! a look of a model at a transform, drawn by the module `models` instanced, a draw per group of
//! instances and batch of its model.

use std::sync::Arc;

use crate::formats::FileRef;
use crate::{ServiceKey, glam};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("models");

pub type Handle = Arc<dyn Models>;

/// What an instance looks like: a model, the textures its display fills, and the submeshes it
/// shows. Plain data, compared and hashed: the same look is loaded once.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Look {
    /// An M2, as a table names it: `.mdx` and `.mdl` are read as `.m2`.
    pub model: FileRef,
    /// The textures its display fills, a kind each (11 to 13 the skins of a creature, 1 the baked
    /// skin of a character, 6 its hair) with its file.
    pub textures: Vec<(u32, FileRef)>,
    pub geosets: Geosets,
}

/// The submeshes a look shows, by the rules of `formats`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Geosets {
    All,
    /// `formats::default_geosets`.
    Default,
    /// A creature's variants, `CreatureDisplayInfo.CreatureGeosetData` (`formats::creature_geosets`).
    Creature(u32),
    /// A character's (`formats::look_geosets`): the submesh of its hair of `CharHairGeosets`, 0 for
    /// none, and the five values of its facial hair of `CharacterFacialHairStyles`.
    Character {
        hair: u32,
        facial: [u32; 5],
    },
}

/// A look, as `Models::look` numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LookId(pub u32);

/// An instance an owner places.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    /// Its owner's id for it, by which `Models::change` replaces or removes it.
    pub id: u64,
    pub look: LookId,
    /// From the model to the world, its scale included.
    pub transform: glam::Mat4,
    /// Its opacity, from 0 to 1: the alpha of a creature's display, for one.
    pub alpha: f32,
}

/// Where a look stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookState {
    /// Placed, or not yet, and not loading: none of its instances near enough, or its turn to come.
    Waiting,
    Loading,
    /// On the GPU, drawn where its instances are in sight.
    Drawn,
    /// Its model or its textures could not be read, and why; or no look has this id.
    Refused(String),
}

/// What a look drawn is made of: `Models::extent`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    /// The bounds of its vertices at rest, in the space of its model.
    pub low: glam::Vec3,
    pub high: glam::Vec3,
    pub radius: f32,
    /// Its batches seen at rest at its finest level: none for a model that draws nothing then.
    pub batches: usize,
    /// The setting `reach` of the module `models`.
    pub reach: f32,
}

impl Extent {
    /// How far from the eye an instance of it at `scale` is drawn, in yards: `reach` times its
    /// radius times its scale, at least `reach`.
    pub fn distance(&self, scale: f32) -> f32 {
        self.reach * (self.radius * scale).max(1.0)
    }
}

/// Shared between threads (rule T3). An owner places its instances from one thread at a time, the
/// thread of its module that moves them, at the frame signal: the service writes the buffers of
/// that owner's instances from that thread.
pub trait Models: Send + Sync {
    /// The id of `look`, the same for the same look; nothing is loaded until an instance of it is
    /// placed. From any thread.
    fn look(&self, look: &Look) -> LookId;

    /// The look of the creature display `display` (`CreatureDisplayInfo`) and its scale, that of
    /// the display times that of its model; for a character's look (`CreatureDisplayInfoExtra`),
    /// the model of its race and sex, its baked skin, its hair, without its equipment. Reads the
    /// tables: from a job.
    fn display(&self, display: u32) -> Result<(Look, f32), String>;

    /// The look of the game object display `display` (`GameObjectDisplayInfo`): its M2 with its
    /// default submeshes; none for a WMO, which the models do not draw. Reads the tables: from a
    /// job.
    fn object(&self, display: u32) -> Result<Option<Look>, String>;

    /// Makes `instances` the whole set of `owner`'s, kept until it gives another: an owner that
    /// moves its instances gives them again at each frame signal, one that does not gives them
    /// once, then when they change. Writes their buffer from the calling thread.
    fn place(&self, owner: &str, instances: &[Instance]);

    /// Changes part of `owner`'s set: the instances `changed` replace those of the same id or join
    /// the set, the ids `removed` leave it.
    fn change(&self, owner: &str, changed: &[Instance], removed: &[u64]);

    /// Removes every instance of `owner`; done for a module that fails.
    fn clear(&self, owner: &str);

    fn state(&self, look: LookId) -> LookState;

    /// What `look` is made of while it is drawn; none otherwise.
    fn extent(&self, look: LookId) -> Option<Extent>;
}
