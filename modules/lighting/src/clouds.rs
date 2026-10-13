//! The clouds of the sky, made as the client makes them, by the public implementation benilla's
//! reading of the client 1.12.1 (`0x6cffc0`, `0x6cfb00`, `0x6d0530`), read for its facts only; 3.3.5a
//! makes its clouds by code too (its loop of the sky and the clouds, `0x7efd00`, and its setting
//! `SkyCloudLOD`), not read here. A tile of 128 × 128 texels of value noise of four octaves, which
//! morphs and drifts with the time: their sum thresholded by the density of the clouds of the light
//! (its number 3) and shaped by a fixed curve into how much each texel is covered; each covered
//! texel coloured from the bands 11 and 12 of the light by how thin it is, plus the glow of the sun
//! or the moon (the band 10) on the slopes facing it. The tile is laid on a dome over the eye, its
//! edge at the horizon, faded there; the glare of the sun is dimmed by the cover at the sun.
//!
//! The tables of the noise are the editor's own (a permutation and values of its own drawing): the
//! client's are not copied, and any such tables look alike.

use std::f32::consts::{FRAC_PI_4, PI};

/// The side of the tile, in texels (`SkyCloudLOD` 0, the default).
pub const SIDE: usize = 128;
/// How often a band of the tile is made again, in seconds, and its rows: the whole tile in four,
/// after which the noise moves a step on in time.
pub const PERIOD: f32 = 0.1;
const ROWS: usize = 32;
/// The steps of the lattice of each octave along a texel, in 256ths of a cell: 8, 16, 32 and 64
/// cells across the tile; each octave half as strong as the one before.
const STEPS: [u32; 4] = [16, 32, 64, 128];
/// The octave whose sum with those before it gives the slopes of the clouds.
const SLOPED: usize = 2;

/// The co-latitudes of the rings of the dome of the clouds, from its top to its edge, in turns of
/// half a circle, the dome lowered by cos 45° so that its edge, 45° from its top, stands at the
/// horizon; the alpha of each ring: whole to the ninth, a half at the tenth, none at the edge; the
/// radius of the tile at the ring `i` is `i / 24` (`0x6d0530`).
const RINGS: [f32; 12] = [
    0.0, 0.025, 0.05, 0.075, 0.1, 0.125, 0.15, 0.175, 0.205, 0.23, 0.245, 0.25,
];
const RING_ALPHAS: [f32; 12] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 128.0 / 255.0, 0.0, 0.0];
const RING_RADIUS: f32 = 1.0 / 24.0;

/// Where `towards`, a direction from the eye, meets the dome of the clouds: the place on the tile,
/// in texels along the north then the west, and how much of the clouds is drawn there (the alpha of
/// its rings); past the edge, the edge, of no alpha.
pub fn on_dome(towards: [f32; 3]) -> ([f32; 2], f32) {
    let length = towards.iter().map(|value| value * value).sum::<f32>().sqrt().max(1e-6);
    let [x, y, z] = towards.map(|value| value / length);
    let lowered = FRAC_PI_4.cos();
    let along = -lowered * z + (lowered * lowered * z * z + 1.0 - lowered * lowered).sqrt();
    let up = (along * z + lowered).clamp(-1.0, 1.0);
    let turns = up.acos() / PI;
    let (ring, alpha) = match RINGS.windows(2).position(|pair| turns <= pair[1]) {
        Some(at) => {
            let share = ((turns - RINGS[at]) / (RINGS[at + 1] - RINGS[at])).clamp(0.0, 1.0);
            let alpha = RING_ALPHAS[at] + share * (RING_ALPHAS[at + 1] - RING_ALPHAS[at]);
            (at as f32 + share, alpha)
        }
        None => ((RINGS.len() - 1) as f32, 0.0),
    };
    let across = (x * x + y * y).sqrt();
    let (north, west) = if across > 1e-6 {
        (x / across, y / across)
    } else {
        (0.0, 0.0)
    };
    let radius = ring * RING_RADIUS;
    (
        [
            (0.5 + north * radius) * SIDE as f32,
            (0.5 + west * radius) * SIDE as f32,
        ],
        alpha,
    )
}

/// What colours the clouds: the glow of the body on them (the band 10 of the light), the colour
/// added by how thin they are (11) and their colour (12), in gamma; the weight of the storm, which
/// dims the glow and lifts it; towards the body that glows (the sun by day, the moon by night), and
/// the envelope of its glow (`heavens::cloud_glow`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub glow: [f32; 3],
    pub thin: [f32; 3],
    pub body: [f32; 3],
    pub storm: f32,
    pub towards: [f32; 3],
    pub envelope: f32,
}

/// The tables of the noise: a permutation of 256, values from −1 to 1, the fade between two cells
/// (a raised cosine), and the curve shaping the cover (`255 − 255 × 0.96^(0.6015625 i)`, `0x6d0900`).
struct Tables {
    order: [u8; 256],
    values: [f32; 256],
    fade: [f32; 256],
    curve: [u8; 256],
}

impl Tables {
    fn new() -> Self {
        // The editor's own drawing: a linear congruential sequence from a fixed seed.
        let mut seed: u32 = 0x5eed_c10d;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 8
        };
        let mut order: [u8; 256] = std::array::from_fn(|at| at as u8);
        for at in (1..256).rev() {
            order.swap(at, next() as usize % (at + 1));
        }
        let values = std::array::from_fn(|_| 1.0 - 2.0 * (next() & 0x7fff) as f32 / 32_767.0);
        let fade = std::array::from_fn(|at| 0.5 * (1.0 - (at as f32 * PI / 256.0).cos()));
        let curve = std::array::from_fn(|at| (255.0 - 255.0 * 0.96f64.powf(at as f64 * 0.601_562_5)) as u8);
        Self {
            order,
            values,
            fade,
            curve,
        }
    }

    fn order(&self, at: u32) -> u32 {
        u32::from(self.order[(at & 0xff) as usize])
    }

    fn value(&self, at: u32) -> f32 {
        self.values[self.order(at) as usize]
    }
}

/// The clouds: how much each texel of the tile is covered, a byte, and its colour, in gamma, the
/// cover its alpha; the sums of the noise, their slopes, the band made next and the step of the
/// noise in time.
pub struct Field {
    cover: Vec<u8>,
    texels: Vec<[u8; 4]>,
    sums: Vec<f32>,
    slopes: Vec<[f32; 2]>,
    row_before: Vec<f32>,
    band: usize,
    step: u16,
    waited: f32,
    started: bool,
    tables: Tables,
}

impl Default for Field {
    fn default() -> Self {
        Self {
            cover: vec![0; SIDE * SIDE],
            texels: vec![[255, 255, 255, 0]; SIDE * SIDE],
            sums: vec![0.0; SIDE * SIDE],
            slopes: vec![[0.0; 2]; SIDE * SIDE],
            row_before: vec![0.0; SIDE],
            band: 0,
            step: 0,
            waited: 0.0,
            started: false,
            tables: Tables::new(),
        }
    }
}

impl Field {
    /// `seconds` passed: the whole tile made at once the first time, then a band each `PERIOD`, as
    /// the client makes it, of the density `density` and coloured by `palette`; whether it changed.
    pub fn tick(&mut self, seconds: f32, density: f32, palette: &Palette) -> bool {
        if !self.started {
            self.started = true;
            self.band = 0;
            self.make(density, SIDE, palette);
            return true;
        }
        self.waited += seconds.max(0.0);
        if self.waited < PERIOD {
            return false;
        }
        self.waited = (self.waited - PERIOD).min(PERIOD);
        self.make(density, ROWS, palette);
        true
    }

    /// How much the clouds cover towards `towards`, from 0 to 1: the texel of the tile the dome puts
    /// there.
    pub fn cover_towards(&self, towards: [f32; 3]) -> f32 {
        let ([u, v], _) = on_dome(towards);
        let at = |value: f32| (value.max(0.0) as usize).min(SIDE - 1);
        f32::from(self.cover[at(v) * SIDE + at(u)]) / 255.0
    }

    /// The texels of the tile, row by row, in gamma, their alpha the cover.
    pub fn texels(&self) -> &[[u8; 4]] {
        &self.texels
    }

    /// The step of the noise in time, one each time the whole tile was made again.
    #[cfg(test)]
    pub fn step(&self) -> u16 {
        self.step
    }

    /// The rows `count` from the band made next: the noise summed, thresholded and shaped, then
    /// coloured; the band after, and the step after the last band.
    fn make(&mut self, density: f32, count: usize, palette: &Palette) {
        let first = self.band;
        let last = (first + count).min(SIDE);
        // The time: two slices of the lattice and how far between, by the high and low byte of the
        // step.
        let slice = u32::from(self.step >> 8);
        let (now, then) = (self.tables.order(slice), self.tables.order(slice + 1));
        let between = self.tables.fade[usize::from(self.step & 0xff)];
        for row in first..last {
            let mut column_before = 0.0;
            for column in 0..SIDE {
                let mut sum = 0.0;
                for (octave, step) in STEPS.iter().enumerate() {
                    let down = row as u32 * step;
                    let across = u32::from(self.step) + column as u32 * step;
                    let (cell_row, cell_column) = ((down >> 8) & 0xff, (across >> 8) & 0xff);
                    let (fade_row, fade_column) = (
                        self.tables.fade[(down & 0xff) as usize],
                        self.tables.fade[(across & 0xff) as usize],
                    );
                    let corner = |slice: u32, rows: u32| {
                        let start = self.tables.order(slice + rows) + cell_column;
                        let (left, right) = (self.tables.value(start), self.tables.value(start + 1));
                        left + fade_column * (right - left)
                    };
                    let at_slice = |slice: u32| {
                        let (upper, lower) = (corner(slice, cell_row), corner(slice, cell_row + 1));
                        upper + fade_row * (lower - upper)
                    };
                    let (a, b) = (at_slice(now), at_slice(then));
                    sum += (a + between * (b - a)) / (1 << octave) as f32;
                    if octave == SLOPED {
                        let at = row * SIDE + column;
                        self.slopes[at] = [column_before - sum, self.row_before[column] - sum];
                        column_before = sum;
                        self.row_before[column] = sum;
                    }
                }
                self.sums[row * SIDE + column] = sum;
            }
        }
        let threshold = ((1.0 - density.clamp(0.0, 1.0)) * 255.0) as i32;
        for at in first * SIDE..last * SIDE {
            let byte = (self.sums[at] * 64.0 + 128.0).floor().clamp(0.0, 255.0) as i32;
            let index = byte - threshold;
            self.cover[at] = if index >= 0 {
                self.tables.curve[index as usize]
            } else {
                0
            };
        }
        self.colour(first, last, palette);
        self.band = last;
        if self.band >= SIDE {
            self.band = 0;
            self.step = self.step.wrapping_add(1);
        }
    }

    /// The colour of the rows from `first` to `last`: a texel not covered keeps the colour of the one
    /// before, of no alpha; a covered one the colour added by how thin it is times its thinness,
    /// plus the clouds' colour, plus the glow of the body by the angle between its slope and the
    /// body, lifted by the storm and dimmed by it, bounded to 1; the cover its alpha (`0x6cfb00`).
    fn colour(&mut self, first: usize, last: usize, palette: &Palette) {
        let lift = 64.0 + 192.0 * palette.storm;
        let strength = palette.envelope * (1.0 - 0.75 * palette.storm);
        let (body, _) = on_dome(palette.towards);
        for row in first..last {
            for column in 0..SIDE {
                let at = row * SIDE + column;
                let cover = self.cover[at];
                if cover == 0 {
                    let before = if column > 0 {
                        self.texels[at - 1]
                    } else {
                        [255, 255, 255, 0]
                    };
                    self.texels[at] = [before[0], before[1], before[2], 0];
                    continue;
                }
                let thinness = f32::from(((255 - cover) >> 1) + 64) / 255.0;
                let mut colour: [f32; 3] =
                    std::array::from_fn(|channel| palette.thin[channel] * thinness + palette.body[channel]);
                let to_body = [body[0] - column as f32, body[1] - row as f32, lift];
                let [dx, dy] = self.slopes[at];
                let normal = [dx, dy, 1.0];
                let dot: f32 = (0..3).map(|axis| to_body[axis] * normal[axis]).sum();
                let lengths = (to_body.iter().map(|value| value * value).sum::<f32>()
                    * normal.iter().map(|value| value * value).sum::<f32>())
                .sqrt();
                let cosine = dot / lengths.max(1e-6);
                if cosine > 0.0 {
                    for (channel, value) in colour.iter_mut().enumerate() {
                        *value += palette.glow[channel] * cosine * strength;
                    }
                }
                let [red, green, blue] = colour.map(|value| (value.clamp(0.0, 1.0) * 255.0) as u8);
                self.texels[at] = [red, green, blue, cover];
            }
        }
    }

    #[cfg(test)]
    pub fn cover(&self) -> &[u8] {
        &self.cover
    }

    /// The curve shaping the cover, at `at` past the threshold.
    #[cfg(test)]
    pub fn curve(&self, at: usize) -> u8 {
        self.tables.curve[at]
    }
}
