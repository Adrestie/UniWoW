//! The sounds of the runtime, which every module reaches through the service `audio`
//! (`uniwow_api::audio`): WAV and MP3 decoded into clips (`Clip::decode`), played by the thread of
//! the audio device, mixed by rodio, each faded in and out over the time it is given, at a volume
//! of its own times that of its category and the master volume, all silenced at once while muted.
//! The device is opened by a thread of the engine, so that nothing waits for it; without one,
//! nothing is played, which the log says once. The thread of the device never waits for a lock: a
//! sound reads what changes of it from atomics, every `BLOCK` frames.

#[cfg(test)]
mod tests;

use std::any::Any;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

pub use rodio;
use rodio::mixer::Mixer;
use rodio::{ChannelCount, Decoder, SampleRate, Source};

/// How many frames a sound plays between two readings of what changes of it; the volumes of the
/// categories and the master volume move to a new value over that many frames, without a click.
pub const BLOCK: usize = 256;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A sound decoded: its samples, its channels interleaved, and their rate a second.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    channels: u16,
    rate: u32,
    samples: Arc<[f32]>,
}

impl Clip {
    /// `samples` of `channels` channels interleaved, `rate` frames a second; refused without a
    /// channel, a rate, or a sample, or with a frame cut short.
    pub fn new(channels: u16, rate: u32, samples: Vec<f32>) -> Result<Clip, String> {
        if channels == 0 || rate == 0 {
            return Err(format!("{channels} channels at {rate} Hz"));
        }
        if samples.is_empty() {
            return Err("no sample".to_owned());
        }
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(format!("{} samples for {channels} channels", samples.len()));
        }
        Ok(Clip {
            channels,
            rate,
            samples: samples.into(),
        })
    }

    /// The WAV (PCM or ADPCM) or MP3 file `bytes`, whatever its name says, decoded whole.
    pub fn decode(bytes: Vec<u8>) -> Result<Clip, String> {
        let decoder = Decoder::new(Cursor::new(bytes)).map_err(|why| format!("not a sound that can be read: {why}"))?;
        let (channels, rate) = (decoder.channels().get(), decoder.sample_rate().get());
        Clip::new(channels, rate, decoder.collect())
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// How long it plays once.
    pub fn duration(&self) -> Duration {
        let frames = self.samples.len() / usize::from(self.channels);
        Duration::from_secs_f64(frames as f64 / f64::from(self.rate))
    }
}

/// The categories of volume, as the client of 3.3.5a has them besides the master volume: the
/// sounds (effects), the ambience, the music.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Effects,
    Ambience,
    Music,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::Effects, Category::Ambience, Category::Music];

    fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Effects => "effects",
            Category::Ambience => "ambience",
            Category::Music => "music",
        }
    }
}

/// A sound to play: its clip, its category, its own volume from 0 to 1, whether it loops, and how
/// long it takes to fade in from silence (none: at once).
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub clip: Clip,
    pub category: Category,
    pub volume: f32,
    pub looped: bool,
    pub fade: Duration,
}

/// A sound played, by the number `Engine::play` gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SoundId(pub u64);

/// Where the output stands: its device being opened, open (the device, in words), or none, with
/// the reason, nothing then being played.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Opening,
    Playing(String),
    Silent(String),
}

/// The output the sounds go to: its mixer, its device in words, and what keeps it open while held,
/// dropped on the thread of the engine.
pub struct Output {
    pub mixer: Mixer,
    pub device: String,
    pub keep: Box<dyn Any>,
}

/// What opens the output, on the thread of the engine.
pub type Opener = Box<dyn FnOnce() -> Result<Output, String> + Send>;

/// The volumes every sound plays at besides its own, as the bits of their `f32`: the master volume
/// and that of each category; and whether all are muted.
struct Levels {
    master: AtomicU32,
    categories: [AtomicU32; 3],
    muted: AtomicBool,
}

impl Levels {
    fn new() -> Self {
        Self {
            master: AtomicU32::new(1f32.to_bits()),
            categories: std::array::from_fn(|_| AtomicU32::new(1f32.to_bits())),
            muted: AtomicBool::new(false),
        }
    }

    /// What a sound of `category` is multiplied by besides its own volume.
    fn gain(&self, category: usize) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            return 0.0;
        }
        let read = |value: &AtomicU32| f32::from_bits(value.load(Ordering::Relaxed));
        read(&self.master) * read(&self.categories[category])
    }
}

/// A volume within 0 and 1, none for a value that is not a number.
fn volume(value: f32) -> f32 {
    if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) }
}

/// What changes of a sound as it plays: the volume it goes to and in how many milliseconds, as one
/// number (the bits of the `f32`, then the milliseconds); whether it stops once silent; whether it
/// has ended, which the thread of the device says.
struct Control {
    aim: AtomicU64,
    stopping: AtomicBool,
    ended: AtomicBool,
}

impl Control {
    fn new(volume: f32, fade: Duration) -> Self {
        Self {
            aim: AtomicU64::new(pack(volume, fade)),
            stopping: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        }
    }

    fn aim(&self, volume: f32, fade: Duration) {
        self.aim.store(pack(volume, fade), Ordering::Release);
    }
}

fn pack(value: f32, fade: Duration) -> u64 {
    let millis = fade.as_millis().min(u128::from(u32::MAX)) as u64;
    (u64::from(volume(value).to_bits()) << 32) | millis
}

fn unpack(aim: u64) -> (f32, u64) {
    (f32::from_bits((aim >> 32) as u32), aim & 0xFFFF_FFFF)
}

/// A sound as the mixer plays it.
struct Voice {
    clip: Clip,
    looped: bool,
    category: usize,
    levels: Arc<Levels>,
    control: Arc<Control>,
    /// The next sample of the clip, and the channel of the frame it belongs to.
    at: usize,
    channel: u16,
    /// Its own volume, going from `from` to `to` over `ramp` frames, `ramped` of them played; the
    /// aim last read.
    from: f32,
    to: f32,
    ramp: u64,
    ramped: u64,
    seen: u64,
    stopping: bool,
    /// The gain of the levels at the start of the block and at its end, and the frames of the block
    /// played.
    level_from: f32,
    level_to: f32,
    block: usize,
    /// The gain of the frame being played.
    gain: f32,
    done: bool,
}

impl Voice {
    fn new(sound: Sound, control: Arc<Control>, levels: Arc<Levels>) -> Self {
        let category = sound.category.index();
        let level = levels.gain(category);
        let seen = control.aim.load(Ordering::Acquire);
        let (to, millis) = unpack(seen);
        let ramp = millis * u64::from(sound.clip.rate) / 1000;
        Self {
            looped: sound.looped,
            category,
            levels,
            control,
            at: 0,
            channel: 0,
            from: if ramp == 0 { to } else { 0.0 },
            to,
            ramp,
            ramped: 0,
            seen,
            stopping: false,
            level_from: level,
            level_to: level,
            block: 0,
            gain: 0.0,
            done: false,
            clip: sound.clip,
        }
    }

    /// Its own volume now.
    fn own(&self) -> f32 {
        if self.ramped >= self.ramp {
            self.to
        } else {
            self.from + (self.to - self.from) * (self.ramped as f32 / self.ramp as f32)
        }
    }

    /// What changed of it and of the levels, read at the start of a block.
    fn read(&mut self) {
        let aim = self.control.aim.load(Ordering::Acquire);
        if aim != self.seen {
            let (to, millis) = unpack(aim);
            self.from = self.own();
            self.to = to;
            self.ramp = millis * u64::from(self.clip.rate) / 1000;
            self.ramped = 0;
            self.seen = aim;
        }
        self.stopping = self.control.stopping.load(Ordering::Acquire);
        self.level_from = self.level_to;
        self.level_to = self.levels.gain(self.category);
        self.block = 0;
    }

    fn end(&mut self) -> Option<f32> {
        self.done = true;
        self.control.ended.store(true, Ordering::Release);
        None
    }
}

impl Iterator for Voice {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.done {
            return None;
        }
        if self.channel == 0 {
            // A frame begins: the reading of a block, then the gain of this frame.
            if self.block == BLOCK {
                self.read();
            }
            let own = self.own();
            if self.stopping && own <= 0.0 && self.ramped >= self.ramp {
                return self.end();
            }
            if self.at >= self.clip.samples.len() {
                if !self.looped {
                    return self.end();
                }
                self.at = 0;
            }
            let level = self.level_from + (self.level_to - self.level_from) * (self.block as f32 / BLOCK as f32);
            self.gain = own * level;
            self.ramped = (self.ramped + 1).min(self.ramp);
            self.block += 1;
        }
        let sample = self.clip.samples[self.at] * self.gain;
        self.at += 1;
        self.channel = (self.channel + 1) % self.clip.channels;
        Some(sample)
    }
}

impl Source for Voice {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.clip.channels).expect("a clip has channels")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(self.clip.rate).expect("a clip has a rate")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Where the output stands, and the sounds played before the device was open.
enum Phase {
    Opening(Vec<Voice>),
    Open { mixer: Mixer, device: String },
    Silent(String),
}

struct Shared {
    levels: Arc<Levels>,
    phase: Mutex<Phase>,
    sounds: Mutex<HashMap<SoundId, Arc<Control>>>,
    next: AtomicU64,
}

/// The engine of the sounds: shared between threads; what it is asked never waits for the device.
pub struct Engine {
    shared: Arc<Shared>,
    /// Dropped to end the thread of the engine, which closes the device.
    quit: Mutex<Option<mpsc::Sender<()>>>,
}

/// The default device of the system, as rodio opens it, trying the others if it cannot.
fn open_default() -> Result<Output, String> {
    let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|why| why.to_string())?;
    sink.log_on_drop(false);
    let config = *sink.config();
    Ok(Output {
        mixer: sink.mixer().clone(),
        device: format!(
            "the default device, {} channels at {} Hz",
            config.channel_count(),
            config.sample_rate()
        ),
        keep: Box::new(sink),
    })
}

impl Engine {
    /// The engine of the default device, opened by its thread.
    pub fn start() -> Engine {
        Engine::with_output(Box::new(open_default))
    }

    /// The engine of the output `open` opens, on the thread of the engine.
    pub fn with_output(open: Opener) -> Engine {
        let shared = Arc::new(Shared {
            levels: Arc::new(Levels::new()),
            phase: Mutex::new(Phase::Opening(Vec::new())),
            sounds: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        });
        let (quit, quitting) = mpsc::channel::<()>();
        let kept = shared.clone();
        let started = std::thread::Builder::new()
            .name("uniwow audio".to_owned())
            .spawn(move || {
                // A device whose opening panics is no device.
                let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(open)).unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    Err(format!("its opening failed ({message})"))
                });
                let output = match opened {
                    Ok(output) => output,
                    Err(why) => {
                        log::warn!("no sound is played: {why}");
                        *lock(&kept.phase) = Phase::Silent(why);
                        return;
                    }
                };
                {
                    let mut phase = lock(&kept.phase);
                    if let Phase::Opening(waiting) = &mut *phase {
                        // Those stopped meanwhile never heard.
                        for mut voice in waiting.drain(..) {
                            if voice.control.stopping.load(Ordering::Acquire) {
                                voice.end();
                            } else {
                                output.mixer.add(voice);
                            }
                        }
                        *phase = Phase::Open {
                            mixer: output.mixer.clone(),
                            device: output.device.clone(),
                        };
                    }
                }
                // Open until the engine ends.
                let _ = quitting.recv();
                drop(output);
            });
        if let Err(why) = started {
            log::warn!("no sound is played: its thread could not start ({why})");
            *lock(&shared.phase) = Phase::Silent(format!("its thread could not start ({why})"));
        }
        Engine {
            shared,
            quit: Mutex::new(Some(quit)),
        }
    }

    /// Plays `sound`, at once or as soon as the device is open; nothing without a device. Never
    /// waits for the device.
    pub fn play(&self, sound: Sound) -> SoundId {
        let id = SoundId(self.shared.next.fetch_add(1, Ordering::Relaxed));
        let control = Arc::new(Control::new(sound.volume, sound.fade));
        let voice = Voice::new(sound, control.clone(), self.shared.levels.clone());
        match &mut *lock(&self.shared.phase) {
            Phase::Opening(waiting) => waiting.push(voice),
            Phase::Open { mixer, .. } => mixer.add(voice),
            Phase::Silent(_) => return id,
        }
        let mut sounds = lock(&self.shared.sounds);
        sounds.retain(|_, control| !control.ended.load(Ordering::Acquire));
        sounds.insert(id, control);
        id
    }

    /// Takes the sound `id` to `volume`, from 0 to 1, over `fade`.
    pub fn set_volume(&self, id: SoundId, volume: f32, fade: Duration) {
        if let Some(control) = lock(&self.shared.sounds).get(&id) {
            control.aim(volume, fade);
        }
    }

    /// Stops the sound `id` once faded out over `fade`.
    pub fn stop(&self, id: SoundId, fade: Duration) {
        if let Some(control) = lock(&self.shared.sounds).remove(&id) {
            control.aim(0.0, fade);
            control.stopping.store(true, Ordering::Release);
        }
    }

    /// Whether the sound `id` is still playing, or waiting for the device to open: neither once it
    /// has ended, been stopped, or without a device.
    pub fn playing(&self, id: SoundId) -> bool {
        lock(&self.shared.sounds)
            .get(&id)
            .is_some_and(|control| !control.ended.load(Ordering::Acquire))
    }

    /// How many sounds are playing.
    pub fn count(&self) -> usize {
        lock(&self.shared.sounds)
            .values()
            .filter(|control| !control.ended.load(Ordering::Acquire))
            .count()
    }

    pub fn state(&self) -> State {
        match &*lock(&self.shared.phase) {
            Phase::Opening(_) => State::Opening,
            Phase::Open { device, .. } => State::Playing(device.clone()),
            Phase::Silent(why) => State::Silent(why.clone()),
        }
    }

    /// Sets the master volume, from 0 to 1.
    pub fn set_master(&self, value: f32) {
        self.shared
            .levels
            .master
            .store(volume(value).to_bits(), Ordering::Relaxed);
    }

    /// Sets the volume of `category`, from 0 to 1.
    pub fn set_category(&self, category: Category, value: f32) {
        self.shared.levels.categories[category.index()].store(volume(value).to_bits(), Ordering::Relaxed);
    }

    /// Silences every sound, or lets them be heard again.
    pub fn set_muted(&self, muted: bool) {
        self.shared.levels.muted.store(muted, Ordering::Relaxed);
    }

    /// Stops every sound at once and closes the device; nothing is played after.
    pub fn shutdown(&self) {
        for (_, control) in lock(&self.shared.sounds).drain() {
            control.aim(0.0, Duration::ZERO);
            control.stopping.store(true, Ordering::Release);
        }
        *lock(&self.shared.phase) = Phase::Silent("stopped".to_owned());
        lock(&self.quit).take();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
