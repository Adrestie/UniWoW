//! Interface of the "audio" service: the sounds the modules play, as the module `audio` gives the
//! engine of the runtime (`libs/audio`): WAV and MP3 decoded into clips, played on the thread of the
//! device, never waited for, faded in and out, by the categories of volume of the client (effects,
//! ambience, music) under the master volume, which the settings of *Audio* hold. Without a device,
//! nothing is played and the service says why.

use std::sync::Arc;
use std::time::Duration;

pub use uniwow_audio::{BLOCK, Category, Clip, Engine, Output, Sound, SoundId, State};

use crate::ServiceKey;

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("audio");

pub type Handle = Arc<dyn Audio>;

/// Shared between threads (rule T3); nothing in it waits for the device. Decode a clip in a job
/// (`Clip::decode`): a long one takes a while.
pub trait Audio: Send + Sync {
    /// Plays `sound`, at once or as soon as the device is open; nothing without a device.
    fn play(&self, sound: Sound) -> SoundId;
    /// Takes the sound `sound` to `volume`, from 0 to 1, over `fade`.
    fn set_volume(&self, sound: SoundId, volume: f32, fade: Duration);
    /// Stops the sound `sound` once faded out over `fade`.
    fn stop(&self, sound: SoundId, fade: Duration);
    /// Whether the sound `sound` still plays: not once ended or stopped, nor without a device.
    fn playing(&self, sound: SoundId) -> bool;
    /// Where the output stands: opening, playing on a device, or silent and why.
    fn state(&self) -> State;
}

impl Audio for Engine {
    fn play(&self, sound: Sound) -> SoundId {
        Engine::play(self, sound)
    }

    fn set_volume(&self, sound: SoundId, volume: f32, fade: Duration) {
        Engine::set_volume(self, sound, volume, fade);
    }

    fn stop(&self, sound: SoundId, fade: Duration) {
        Engine::stop(self, sound, fade);
    }

    fn playing(&self, sound: SoundId) -> bool {
        Engine::playing(self, sound)
    }

    fn state(&self) -> State {
        Engine::state(self)
    }
}
