//! The clouds of the sky, made as Wow.exe 12340 makes them (`0x7efd00` makes the tile, `0x7efae0`
//! colours it, `0x7f20e0` lays it on its dome, `0x7ef920` finds a body on it), as the client 1.12.1
//! that the public implementation benilla reads makes them but where 12340 differs: a tile of 128 ×
//! 128 texels (`SkyCloudLOD` 0, the default) of value noise of four octaves, which morphs and drifts
//! with the time: their sum thresholded by the density of the clouds of the light (its number 3) and
//! shaped by a fixed curve into how much each texel is covered; each covered texel coloured from the
//! bands 11 and 12 of the light by how thin it is, plus the glow of the sun or the moon (the band 10)
//! on the slopes facing it. Eight rows are made at each frame into a tile apart, shown once whole
//! and the time moved on; the tile is laid on a dome over the eye, its edge at the horizon, faded
//! there; the glare of the sun is dimmed by the cover at the sun.
//!
//! The tables of the noise are the editor's own (a permutation and values of its own drawing): the
//! client's are not copied, and any such tables look alike.

use std::f32::consts::{FRAC_PI_4, PI};

/// The side of the tile, in texels (`SkyCloudLOD` 0, the default).
pub const SIDE: usize = 128;
/// The rows of the tile made at each frame (`0x7f04b0`).
const ROWS: usize = 8;
/// The steps of the noise in time a second, read once the whole tile is made (`0xa4040c`).
const STEPS_A_SECOND: f32 = 2.0;
/// How far from the eye the cover at a body is read, in yards: where its quad stands.
const BODY_DISTANCE: f32 = 12.0;
/// The steps of the lattice of each octave along a texel, in 256ths of a cell: 8, 16, 32 and 64
/// cells across the tile; each octave half as strong as the one before.
const STEPS: [u32; 4] = [16, 32, 64, 128];
/// The octave whose sum with those before it gives the slopes of the clouds.
const SLOPED: usize = 2;

/// The co-latitudes of the rings of the dome of the clouds, from its top to its edge, in turns of
/// half a circle, the dome lowered by cos 45° so that its edge, 45° from its top, stands at the
/// horizon; the alpha of each ring: whole to the ninth, a half at the tenth, none at the edge; the
/// radius of the tile at the ring `i` is `i / 22` (`0x7f20e0`, `0xa41ce8`).
#[cfg(test)]
const RINGS: [f32; 12] = [
    0.0, 0.025, 0.05, 0.075, 0.1, 0.125, 0.15, 0.175, 0.205, 0.23, 0.245, 0.25,
];
#[cfg(test)]
const RING_ALPHAS: [f32; 12] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 128.0 / 255.0, 0.0, 0.0];
#[cfg(test)]
const RING_RADIUS: f32 = 1.0 / 22.0;

/// Where `towards`, a direction from the eye, meets the dome of the clouds: the place on the tile,
/// in texels along the north then the west, and how much of the clouds is drawn there (the alpha of
/// its rings); past the edge, the edge, of no alpha. As the shader of the heavens lays the tile,
/// written again for the tests.
#[cfg(test)]
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

/// Where a body seen at `at` from the eye stands on the tile, as 12340 finds it (`0x7ef920`): its
/// angle from the top over the eye lifted by cos 45°, taken to the tile linearly up to 45°, the edge
/// half the side away; in texels along the north then the west.
fn on_tile(at: [f32; 3]) -> [f32; 2] {
    let [x, y, z] = at;
    let up = z + FRAC_PI_4.cos();
    let across = (x * x + y * y).sqrt();
    let angle = (up / (across * across + up * up).sqrt().max(1e-6))
        .clamp(-1.0, 1.0)
        .acos();
    let radius = 2.0 / PI * angle.min(FRAC_PI_4);
    let (north, west) = if across > 1e-6 {
        (x / across, y / across)
    } else {
        (0.0, 0.0)
    };
    [
        (0.5 + north * radius) * SIDE as f32,
        (0.5 + west * radius) * SIDE as f32,
    ]
}

/// Where the body towards `towards` glows from on the tile: where it meets the dome of the clouds
/// (`0x7eda90`), found on the tile (`on_tile`).
pub fn glowing_from(towards: [f32; 3]) -> [f32; 2] {
    let length = towards.iter().map(|value| value * value).sum::<f32>().sqrt().max(1e-6);
    let [x, y, z] = towards.map(|value| value / length);
    let lowered = FRAC_PI_4.cos();
    let along = -lowered * z + (lowered * lowered * z * z + 1.0 - lowered * lowered).sqrt();
    on_tile([along * x, along * y, along * z])
}

/// Where the cover at the body towards `towards` is read on the tile: at its quad, `BODY_DISTANCE`
/// yards from the eye, found on the tile without the dome (`0x7efa30`).
pub fn covered_at(towards: [f32; 3]) -> [f32; 2] {
    let length = towards.iter().map(|value| value * value).sum::<f32>().sqrt().max(1e-6);
    on_tile(towards.map(|value| value / length * BODY_DISTANCE))
}

/// The byte of the noise summed to `sum`: 64 times it over 128, rounded to the nearest as an x87
/// store rounds, then its low byte (`0x7efd00`).
pub fn noise_byte(sum: f32) -> i32 {
    (sum * 64.0 + 128.0).round() as i32 & 0xff
}

/// The inverse of the square root of `value`, as 12340 takes it for the glow: from the bits of the
/// float, without a step of Newton (`0x5f3997bb`).
pub fn rough_inverse_root(value: f32) -> f32 {
    f32::from_bits(0x5f39_97bb_u32.wrapping_sub(value.to_bits() >> 1))
}

/// What colours the clouds: the glow of the body on them (the band 10 of the light), the colour
/// added by how thin they are (11) and their colour (12), in gamma; the weight of the storm, which
/// dims the glow and lifts it; and towards the body that glows (the sun by day, the moon by night),
/// whose glow is always whole in 12340 (`0xaf4be0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub glow: [f32; 3],
    pub thin: [f32; 3],
    pub body: [f32; 3],
    pub storm: f32,
    pub towards: [f32; 3],
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

/// The clouds: of the tile shown (`shown`) and of the one being made, how much each texel is
/// covered, a byte, and its colour, in gamma, the cover its alpha; the sums of the noise, their
/// slopes, the band made next, the step of the noise in time and the seconds gone by.
pub struct Field {
    cover: [Vec<u8>; 2],
    texels: [Vec<[u8; 4]>; 2],
    shown: usize,
    sums: Vec<f32>,
    slopes: Vec<[f32; 2]>,
    row_before: Vec<f32>,
    band: usize,
    step: u16,
    seconds: f32,
    started: bool,
    tables: Tables,
}

impl Default for Field {
    fn default() -> Self {
        Self {
            cover: [vec![0; SIDE * SIDE], vec![0; SIDE * SIDE]],
            texels: [
                vec![[255, 255, 255, 0]; SIDE * SIDE],
                vec![[255, 255, 255, 0]; SIDE * SIDE],
            ],
            shown: 0,
            sums: vec![0.0; SIDE * SIDE],
            slopes: vec![[0.0; 2]; SIDE * SIDE],
            row_before: vec![0.0; SIDE],
            band: 0,
            step: 0,
            seconds: 0.0,
            started: false,
            tables: Tables::new(),
        }
    }
}

impl Field {
    /// A frame `seconds` after the one before, as 12340 makes its clouds at each frame: the whole
    /// tile made and shown at once the first time, then `ROWS` rows into the tile apart, of the
    /// density `density` and coloured by `palette`; once it is whole, the step of the noise read
    /// from the seconds gone by, two a second, and the tile shown when that step moved on. Whether
    /// the tile shown changed.
    pub fn tick(&mut self, seconds: f32, density: f32, palette: &Palette) -> bool {
        self.seconds += seconds.max(0.0);
        if !self.started {
            self.started = true;
            self.band = 0;
            self.make(density, SIDE, palette);
            self.shown = 1 - self.shown;
            return true;
        }
        if !self.make(density, ROWS, palette) {
            return false;
        }
        let step = (STEPS_A_SECOND * self.seconds) as u32 as u16;
        if step == self.step {
            return false;
        }
        self.step = step;
        self.shown = 1 - self.shown;
        true
    }

    /// How much the clouds shown cover at the body towards `towards`, from 0 to 1 (`covered_at`).
    pub fn cover_towards(&self, towards: [f32; 3]) -> f32 {
        let [u, v] = covered_at(towards);
        let at = |value: f32| (value.max(0.0) as usize).min(SIDE - 1);
        f32::from(self.cover[self.shown][at(v) * SIDE + at(u)]) / 255.0
    }

    /// The texels of the tile shown, row by row, in gamma, their alpha the cover.
    pub fn texels(&self) -> &[[u8; 4]] {
        &self.texels[self.shown]
    }

    /// The step of the noise in time.
    #[cfg(test)]
    pub fn step(&self) -> u16 {
        self.step
    }

    /// The rows `count` from the band made next into the tile apart: the noise summed, thresholded
    /// and shaped, then coloured; the band after. Whether the tile is whole.
    fn make(&mut self, density: f32, count: usize, palette: &Palette) -> bool {
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
        let made = 1 - self.shown;
        for at in first * SIDE..last * SIDE {
            let byte = noise_byte(self.sums[at]);
            let index = byte - threshold;
            self.cover[made][at] = if index >= 0 {
                self.tables.curve[index as usize]
            } else {
                0
            };
        }
        self.colour(first, last, palette);
        self.band = last;
        let whole = self.band >= SIDE;
        if whole {
            self.band = 0;
        }
        whole
    }

    /// The colour of the rows from `first` to `last` of the tile apart: a texel not covered keeps
    /// the colour of the one before, of no alpha, the first of a row left as it was; a covered one
    /// the colour added by how thin it is times its thinness, plus the clouds' colour, plus the glow
    /// of the body by the angle between its slope and the body, its lengths taken as 12340 takes
    /// them (`rough_inverse_root`), lifted by the storm and dimmed by it, bounded to 1; the cover its
    /// alpha (`0x7efae0`).
    fn colour(&mut self, first: usize, last: usize, palette: &Palette) {
        let lift = 64.0 + 192.0 * palette.storm;
        let strength = 1.0 - 0.75 * palette.storm;
        let body = glowing_from(palette.towards);
        let made = 1 - self.shown;
        for row in first..last {
            for column in 0..SIDE {
                let at = row * SIDE + column;
                let cover = self.cover[made][at];
                if cover == 0 {
                    if column > 0 {
                        let before = self.texels[made][at - 1];
                        self.texels[made][at] = [before[0], before[1], before[2], 0];
                    }
                    continue;
                }
                let thinness = f32::from(((255 - cover) >> 1) + 64) / 255.0;
                let mut colour: [f32; 3] =
                    std::array::from_fn(|channel| palette.thin[channel] * thinness + palette.body[channel]);
                let to_body = [body[0] - column as f32, body[1] - row as f32, lift];
                let [dx, dy] = self.slopes[at];
                let normal = [dx, dy, 1.0];
                let dot: f32 = (0..3).map(|axis| to_body[axis] * normal[axis]).sum();
                let squared = |vector: [f32; 3]| vector.iter().map(|value| value * value).sum::<f32>();
                let cosine = dot * rough_inverse_root(squared(normal)) * rough_inverse_root(squared(to_body));
                if cosine > 0.0 {
                    for (channel, value) in colour.iter_mut().enumerate() {
                        *value += palette.glow[channel] * cosine * strength;
                    }
                }
                let [red, green, blue] = colour.map(|value| (value.min(1.0) * 255.0).round().max(0.0) as u8);
                self.texels[made][at] = [red, green, blue, cover];
            }
        }
    }

    /// How much each texel of the tile shown is covered.
    #[cfg(test)]
    pub fn cover(&self) -> &[u8] {
        &self.cover[self.shown]
    }

    /// The curve shaping the cover, at `at` past the threshold.
    #[cfg(test)]
    pub fn curve(&self, at: usize) -> u8 {
        self.tables.curve[at]
    }
}
