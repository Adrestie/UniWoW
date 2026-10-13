//! The lightning of a thunderstorm: strikes at intervals that shorten as the storm grows, each one to
//! three quick flashes that light the scene, then its thunder, later as the strike is farther. The
//! client of 3.3.5a names no lightning in its tables (its `Weather.dbc` has no field for it; the
//! table `Lightning` comes with 8.0.1): every number here is the editor's choice, after the fields
//! of that later table (an interval between strikes, a strike, a fade, a brightness). Seeded, so
//! that the same seed and the same moments give the same storm; its state is a few numbers, nothing
//! allocated as it runs.

/// The mean interval between two strikes in seconds, at the lowest intensity and at the highest;
/// each interval is drawn between half and one and a half times it.
pub const LONGEST: f32 = 24.0;
pub const SHORTEST: f32 = 6.0;
/// A flash: how long it takes to light, then to fade out, in seconds; the flashes of a strike one
/// after the other, `GAP` apart, each dimmer than the one before, as `FLASHES` says.
pub const RISE: f64 = 0.04;
pub const FADE: f64 = 0.3;
pub const GAP: f64 = 0.13;
pub const FLASHES: [f32; 3] = [1.0, 0.65, 0.4];
/// How far a strike falls from the eye, in yards, and how fast its thunder comes (the speed of the
/// sound in the air, 343 metres a second).
pub const NEAREST: f32 = 200.0;
pub const FARTHEST: f32 = 1_600.0;
pub const SOUND: f32 = 375.0;
/// The brightness of a flash and the loudness of a thunder at the farthest, both whole at the
/// nearest.
pub const DIMMEST: f32 = 0.4;
pub const FAINTEST: f32 = 0.35;
/// The thunders waiting to be heard at once, the strikes coming faster than their thunders at most.
const WAITING: usize = 4;

/// A strike: when it fell, how many flashes it makes, how bright it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strike {
    pub at: f64,
    pub flashes: usize,
    pub brightness: f32,
}

/// The thunder of a strike: when it is heard, how loud from 0 to 1, and a number drawn with it, to
/// choose among the files of its sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thunder {
    pub at: f64,
    pub loudness: f32,
    pub pick: u32,
}

/// The lightning of a storm as it goes on.
#[derive(Clone, Debug, PartialEq)]
pub struct Storm {
    random: u64,
    /// When the next strike falls, none while there is no storm.
    next: Option<f64>,
    strike: Option<Strike>,
    thunders: [Option<Thunder>; WAITING],
}

/// The seed of the storms of the editor.
pub const SEED: u64 = 0x5745_4154_4845_5221;

impl Default for Storm {
    fn default() -> Self {
        Self::new(SEED)
    }
}

impl Storm {
    pub fn new(seed: u64) -> Self {
        Self {
            random: seed,
            next: None,
            strike: None,
            thunders: [None; WAITING],
        }
    }

    /// A number drawn from 0 to 1, by SplitMix64.
    fn draw(&mut self) -> f32 {
        self.random = self.random.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.random;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    /// The interval to the next strike in a storm of `intensity`.
    fn interval(&mut self, intensity: f32) -> f64 {
        let mean = LONGEST + (SHORTEST - LONGEST) * intensity.clamp(0.0, 1.0);
        f64::from(mean * (0.5 + self.draw()))
    }

    /// The strike being drawn, if any.
    #[cfg(test)]
    pub fn strike(&self) -> Option<Strike> {
        self.strike
    }

    /// At `now`, in seconds, growing from one call to the next, in a thunderstorm of `intensity`, or
    /// none: how much the flash of the strike lights the scene, from 0 to 1, and a thunder to be
    /// heard now, if one is due. The first strike of a storm falls after one interval; without a
    /// storm, nothing, and the thunders still waiting are forgotten.
    pub fn step(&mut self, now: f64, intensity: Option<f32>) -> (f32, Option<Thunder>) {
        let Some(intensity) = intensity.filter(|intensity| *intensity > 0.0) else {
            self.next = None;
            self.strike = None;
            self.thunders = [None; WAITING];
            return (0.0, None);
        };
        let next = match self.next {
            Some(next) => next,
            None => {
                let next = now + self.interval(intensity);
                self.next = Some(next);
                next
            }
        };
        if now >= next {
            // From the nearest to the farthest: the nearer, the brighter, the louder and the sooner.
            let near = self.draw();
            let distance = NEAREST + (FARTHEST - NEAREST) * (1.0 - near);
            let flashes = 1 + (self.draw() * FLASHES.len() as f32) as usize;
            self.strike = Some(Strike {
                at: now,
                flashes: flashes.min(FLASHES.len()),
                brightness: DIMMEST + (1.0 - DIMMEST) * near,
            });
            let thunder = Thunder {
                at: now + f64::from(distance / SOUND),
                loudness: FAINTEST + (1.0 - FAINTEST) * near,
                pick: (self.draw() * u32::MAX as f32) as u32,
            };
            // The oldest waiting gives its place when all are taken.
            let free = self.thunders.iter().position(Option::is_none).unwrap_or(0);
            self.thunders[free] = Some(thunder);
            self.next = Some(now + self.interval(intensity));
        }
        let flash = self.strike.map_or(0.0, |strike| brightness(&strike, now));
        if self
            .strike
            .is_some_and(|strike| now - strike.at > strike.flashes as f64 * GAP + RISE + FADE)
        {
            self.strike = None;
        }
        // The thunder due first, if any.
        let due = self
            .thunders
            .iter()
            .enumerate()
            .filter_map(|(at, thunder)| thunder.filter(|thunder| thunder.at <= now).map(|thunder| (at, thunder)))
            .min_by(|(_, a), (_, b)| a.at.total_cmp(&b.at));
        let heard = due.map(|(at, thunder)| {
            self.thunders[at] = None;
            thunder
        });
        (flash, heard)
    }
}

/// How much `strike` lights the scene at `now`: its brightest flash then, each rising in `RISE` and
/// fading out in `FADE`.
pub fn brightness(strike: &Strike, now: f64) -> f32 {
    let since = now - strike.at;
    FLASHES[..strike.flashes]
        .iter()
        .enumerate()
        .map(|(at, share)| {
            let into = since - at as f64 * GAP;
            let lit = if into < 0.0 {
                0.0
            } else if into < RISE {
                into / RISE
            } else {
                (1.0 - (into - RISE) / FADE).max(0.0)
            };
            share * lit as f32
        })
        .fold(0.0, f32::max)
        * strike.brightness
}
