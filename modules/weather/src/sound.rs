//! The sounds of the weather: the loop the client's `Weather.dbc` names for the state of the weather
//! (its `AmbienceID`, a row of `SoundEntries.dbc`), faded in and out as the weather changes and as
//! the eye goes in or out of shelter; and the thunder of each strike of the lightning. Their files
//! read from the client's archives and decoded by jobs, played through the service `audio` in its
//! ambience; in the editor alone, nothing sent to the server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use uniwow_api::audio::{self, Category, Clip, Sound, SoundId, State};
use uniwow_api::formats::{SoundEntryRecord, WeatherRecord};
use uniwow_api::vfs::Vfs;
use uniwow_api::{Context, JobId};

use crate::storm::Thunder;

/// How long the loop of a weather fades in, and the one before out, as the weather changes; and how
/// long it fades as the eye goes in or out of shelter. The editor's choices.
pub const CHANGE: Duration = Duration::from_secs(3);
pub const SHELTER: Duration = Duration::from_secs(1);
/// The thunder after a flash, by default: the row `StormPeaksLightning` of `SoundEntries.dbc`, the
/// editor's choice, the client of 3.3.5a naming no thunder for its weathers.
pub const THUNDER: u32 = 13_021;

/// The tables of the sounds: the weathers and the sounds, each by increasing id.
pub struct Tables {
    weathers: Arc<Vec<WeatherRecord>>,
    sounds: Arc<Vec<SoundEntryRecord>>,
}

impl Tables {
    pub fn new(weathers: Arc<Vec<WeatherRecord>>, sounds: Arc<Vec<SoundEntryRecord>>) -> Self {
        Self { weathers, sounds }
    }

    /// The sound of `id`.
    pub fn sound(&self, id: u32) -> Option<&SoundEntryRecord> {
        let at = self.sounds.binary_search_by_key(&id, |sound| sound.id).ok()?;
        self.sounds.get(at)
    }

    /// The sound of the weather of the state `state`: none for a weather without one or unknown.
    pub fn of_state(&self, state: u32) -> Option<&SoundEntryRecord> {
        let at = self.weathers.binary_search_by_key(&state, |weather| weather.id).ok()?;
        match self.weathers[at].sound {
            0 => None,
            sound => self.sound(sound),
        }
    }
}

/// The tables as a job reads them.
pub type Read = Result<(Arc<Vec<WeatherRecord>>, Arc<Vec<SoundEntryRecord>>), String>;

/// A file of a sound: being read and decoded by a job, ready, or why not.
enum Clipped {
    Reading(JobId),
    Ready(Clip),
    Failed(String),
}

/// The loop playing: the row of its sound, its name and its file, its number, and the volume it
/// was taken to last.
struct Looping {
    sound: u32,
    name: String,
    path: String,
    id: SoundId,
    volume: f32,
}

/// What the weather says it plays, for its panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Heard {
    /// No weather, or a weather the client plays nothing for.
    Nothing,
    /// The loop of the weather, by its name and file, being read or playing.
    Loading(String, String),
    Playing(String, String),
    /// Why nothing is heard.
    Unheard(String),
}

#[derive(Default)]
pub struct Sounds {
    /// The tables read, or why not, and the job reading them.
    tables: Option<Result<Tables, String>>,
    reading: Option<JobId>,
    /// The files of the sounds, by their paths.
    clips: HashMap<String, Clipped>,
    looping: Option<Looping>,
    /// The service the sounds were played through, kept to stop them when the module stops.
    audio: Option<audio::Handle>,
    pub heard: Option<Heard>,
    /// The thunder last asked for, by its name, or why it cannot be heard.
    pub thunder: Option<Result<String, String>>,
}

impl Sounds {
    /// Everything forgotten, as when the client's archives change; the loop stopped.
    pub fn reset(&mut self, ctx: &mut Context) {
        if let Some(job) = self.reading.take() {
            ctx.cancel(job);
        }
        for clipped in self.clips.values() {
            if let Clipped::Reading(job) = clipped {
                ctx.cancel(*job);
            }
        }
        self.clips.clear();
        self.tables = None;
        self.stop(CHANGE);
    }

    /// The loop stopped over `fade`.
    pub fn stop(&mut self, fade: Duration) {
        if let (Some(looping), Some(audio)) = (self.looping.take(), &self.audio) {
            audio.stop(looping.id, fade);
        }
    }

    /// The tables, read by a job the first time they are asked for.
    fn tables(&mut self, ctx: &mut Context) -> Option<&Tables> {
        if self.tables.is_none() && self.reading.is_none() {
            let formats = ctx.service(uniwow_api::formats::SERVICE)?;
            self.reading = Some(ctx.spawn("Read the sounds of the weather", move |_| -> Read {
                Ok((formats.weathers()?, formats.sound_entries()?))
            }));
        }
        self.tables.as_ref()?.as_ref().ok()
    }

    /// The file `path` decoded, read by a job the first time it is asked for; why it cannot be.
    fn clip(&mut self, ctx: &mut Context, path: &str) -> Result<Option<Clip>, String> {
        match self.clips.get(path) {
            Some(Clipped::Ready(clip)) => return Ok(Some(clip.clone())),
            Some(Clipped::Failed(why)) => return Err(why.clone()),
            Some(Clipped::Reading(_)) => return Ok(None),
            None => {}
        }
        let files: Arc<dyn Vfs> = ctx
            .service(uniwow_api::vfs::SERVICE)
            .ok_or_else(|| "no service vfs".to_owned())?;
        let read = path.to_owned();
        let job = ctx.spawn("Read a sound of the weather", move |_| -> Result<Clip, String> {
            let bytes = files.read(&read)?.ok_or_else(|| "not in the client".to_owned())?;
            Clip::decode(bytes)
        });
        self.clips.insert(path.to_owned(), Clipped::Reading(job));
        Ok(None)
    }

    /// Takes the outcome of the job `job`, if it is one of the sounds'.
    pub fn on_job(&mut self, job: JobId, outcome: uniwow_api::JobOutcome) -> bool {
        use uniwow_api::JobOutcome;
        let failed = |outcome: &JobOutcome| match outcome {
            JobOutcome::Panicked(message) => Some(message.clone()),
            JobOutcome::Cancelled => Some("its reading was cancelled".to_owned()),
            _ => None,
        };
        if self.reading == Some(job) {
            self.reading = None;
            self.tables = Some(match failed(&outcome) {
                Some(why) => Err(why),
                None => match outcome.take::<Read>() {
                    Some(read) => read.map(|(weathers, sounds)| Tables::new(weathers, sounds)),
                    None => return true,
                },
            });
            if let Some(Err(why)) = &self.tables {
                uniwow_api::log::warn!("the sounds of the weather are not read: {why}");
            }
            return true;
        }
        let Some(path) = self
            .clips
            .iter()
            .find(|(_, clipped)| matches!(clipped, Clipped::Reading(reading) if *reading == job))
            .map(|(path, _)| path.clone())
        else {
            return false;
        };
        let clipped = match failed(&outcome) {
            Some(why) => Clipped::Failed(why),
            None => match outcome.take::<Result<Clip, String>>() {
                Some(Ok(clip)) => Clipped::Ready(clip),
                Some(Err(why)) => Clipped::Failed(why),
                None => return true,
            },
        };
        if let Clipped::Failed(why) = &clipped {
            uniwow_api::log::warn!("{path}: the sound cannot be played: {why}");
        }
        self.clips.insert(path, clipped);
        true
    }

    /// At each frame: the loop of the weather of the state `state` (a row of `Weather.dbc`, none
    /// for fine weather), silent where `sheltered`; the thunder `thunder` of the sound `thunder_sound`
    /// of `SoundEntries.dbc` heard now, but where sheltered.
    pub fn steer(
        &mut self,
        ctx: &mut Context,
        state: Option<u32>,
        sheltered: bool,
        thunder: Option<Thunder>,
        thunder_sound: u32,
    ) {
        self.audio = ctx.service(audio::SERVICE);
        if state.is_none() && self.looping.is_none() && thunder.is_none() {
            self.heard = Some(Heard::Nothing);
            return;
        }
        let Some(audio) = self.audio.clone() else {
            self.looping = None;
            self.heard = Some(Heard::Unheard("no service audio".to_owned()));
            return;
        };
        if let State::Silent(why) = audio.state() {
            self.looping = None;
            self.heard = Some(Heard::Unheard(why));
            return;
        }
        let Some(tables) = self.tables(ctx) else {
            self.heard = Some(match &self.tables {
                Some(Err(why)) => Heard::Unheard(format!("the sounds are not read: {why}")),
                _ => Heard::Unheard("reading the sounds of the client".to_owned()),
            });
            return;
        };
        // The loop of this weather: its row, its name, its file (the first of its files), its volume.
        let wanted = state
            .and_then(|state| tables.of_state(state))
            .and_then(|sound| Some((sound.id, sound.name.clone(), sound.path(0)?, sound.volume)));
        let thunder = thunder.filter(|_| !sheltered).and_then(|thunder| {
            let sound = tables.sound(thunder_sound)?;
            Some((
                sound.name.clone(),
                sound.path(thunder.pick)?,
                sound.volume * thunder.loudness,
            ))
        });
        // Another weather, or none, or the loop ended elsewhere: the loop before fades out.
        if self
            .looping
            .as_ref()
            .is_some_and(|looping| Some(looping.sound) != wanted.as_ref().map(|(sound, ..)| *sound))
            || self.looping.as_ref().is_some_and(|looping| !audio.playing(looping.id))
        {
            self.stop(CHANGE);
        }
        let volume = |loud: f32| if sheltered { 0.0 } else { loud.clamp(0.0, 1.0) };
        self.heard = Some(match wanted {
            None => Heard::Nothing,
            Some((sound, name, path, loud)) => match &mut self.looping {
                Some(looping) => {
                    if volume(loud) != looping.volume {
                        looping.volume = volume(loud);
                        audio.set_volume(looping.id, looping.volume, SHELTER);
                    }
                    Heard::Playing(looping.name.clone(), looping.path.clone())
                }
                None => match self.clip(ctx, &path) {
                    Ok(Some(clip)) => {
                        let id = audio.play(Sound {
                            clip,
                            category: Category::Ambience,
                            volume: volume(loud),
                            looped: true,
                            fade: CHANGE,
                        });
                        self.looping = Some(Looping {
                            sound,
                            name: name.clone(),
                            path: path.clone(),
                            id,
                            volume: volume(loud),
                        });
                        Heard::Playing(name, path)
                    }
                    Ok(None) => Heard::Loading(name, path),
                    Err(why) => Heard::Unheard(format!("{path}: {why}")),
                },
            },
        });
        // A thunder now, once its file is read: missed while it is not.
        if let Some((name, path, loud)) = thunder {
            self.thunder = Some(match self.clip(ctx, &path) {
                Ok(Some(clip)) => {
                    audio.play(Sound {
                        clip,
                        category: Category::Ambience,
                        volume: loud.clamp(0.0, 1.0),
                        looped: false,
                        fade: Duration::ZERO,
                    });
                    Ok(name)
                }
                Ok(None) => Ok(name),
                Err(why) => Err(format!("{path}: {why}")),
            });
        }
    }
}
