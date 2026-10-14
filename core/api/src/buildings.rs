//! Interface of the "buildings" service: where the eye of the view stands among the buildings the
//! module `buildings` draws, as its layer found it at the last frame drawn: the group of a building
//! it is in, found as the client finds the group of its camera, with what the light of the map takes
//! from it (the fogs and the sky of its building), and the liquid of the buildings it lies in.

use std::sync::Arc;

use crate::ServiceKey;
use crate::formats::{FileRef, FogBand};
use crate::liquids::Immersed;

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("buildings");

pub type Handle = Arc<dyn Buildings>;

/// Shared between threads (rule T3).
pub trait Buildings: Send + Sync {
    /// Where the eye stood among the buildings at the last frame drawn; outside every building, and
    /// leaving the liquid to the terrain, before the first.
    fn eye(&self) -> Eye;
}

/// Where the eye stands among the buildings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Eye {
    /// The group inside of a building the eye is in, none outside every building.
    pub group: Option<EyeGroup>,
    /// The liquid the eye lies in as the buildings decide it, as the client finds the one its
    /// camera is in (0x790920), the depth in the axes of its building: in a group inside, that of
    /// that group alone, `Some(None)` where its surface is not over the eye; otherwise that of a
    /// group outside of a building whose bounds hold the eye, its surface over it; `None` leaves it
    /// to the liquids of the terrain.
    pub liquid: Option<Option<Immersed>>,
}

/// The group inside of a building the eye is in.
#[derive(Clone, Debug, PartialEq)]
pub struct EyeGroup {
    /// Its flags (`WmoGroup::flags`): 0x40 lit as outside, a street open to the sky.
    pub flags: u32,
    /// The fog of its building at the eye, in the air then under the water, as the client mixes
    /// the fogs of the building (`MFOG`) the group names; none where its building gives none.
    pub fog: Option<[FogBand; 2]>,
    /// How far the eye is from the nearest portal of the group, in yards: the fog of the building
    /// weighs over that of the light by it.
    pub exit: f32,
    /// The model of the sky of its building, when the group shows it (its flag 0x40000).
    pub skybox: Option<FileRef>,
}

impl EyeGroup {
    /// Whether the group is a room, not lit as outside: the weather does not reach it, and the fog
    /// of its building weighs there.
    pub fn room(&self) -> bool {
        self.flags & LIT_OUTSIDE == 0
    }
}

/// The flag of a group lit as outside, inside but open to the sky.
const LIT_OUTSIDE: u32 = 0x40;
