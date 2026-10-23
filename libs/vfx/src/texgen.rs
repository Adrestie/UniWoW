//! The textures of the effects, drawn here: white (or the colour given) with the shape in the
//! alpha, so that a particle, a ribbon or a mesh tints one shape with its own colour. The kinds and
//! parameters of `tools/vfx/texgen.py`; the analytic shapes are its own to one level of alpha, the
//! random ones (noise, smoke, rune, lightning) come from a generator of their own here: the same
//! seed gives the same image, of the same look, not the same pixels.

use crate::json::Value;
use std::f64::consts::PI;

/// An RGBA image, 8 bits a channel, rows from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba[at..at + 4].try_into().expect("four channels")
    }
}

/// A plane of floats (alpha before it becomes bytes), rows from the top.
#[derive(Clone, Debug)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub v: Vec<f64>,
}

impl Plane {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            v: vec![0.0; width * height],
        }
    }

    /// Each texel from its column and row.
    fn from_fn(width: usize, height: usize, mut f: impl FnMut(usize, usize) -> f64) -> Self {
        let mut v = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                v.push(f(x, y));
            }
        }
        Self { width, height, v }
    }

    fn at(&self, x: usize, y: usize) -> f64 {
        self.v[y * self.width + x]
    }
}

/// The coordinates of a square texture: -1 to 1 across, from its centre, as texgen's `_grid`.
fn grid(size: usize, mut f: impl FnMut(f64, f64) -> f64) -> Plane {
    let c = (size as f64 - 1.0) / 2.0;
    let half = size as f64 / 2.0;
    Plane::from_fn(size, size, |x, y| f((x as f64 - c) / half, (y as f64 - c) / half))
}

fn smooth(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The image of an alpha plane: the colour everywhere, the shape in the alpha.
pub fn rgba(alpha: &Plane, color: [u8; 3]) -> Image {
    let mut image = Image::new(alpha.width as u32, alpha.height as u32);
    for (texel, a) in image.rgba.as_chunks_mut::<4>().0.iter_mut().zip(&alpha.v) {
        texel[..3].copy_from_slice(&color);
        texel[3] = (a.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
    }
    image
}

// --- a deterministic generator ------------------------------------------------------------------

/// SplitMix64: a small generator, the same numbers from the same seed on every machine.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: i64) -> Self {
        Self((seed as u64) ^ 0x5DEE_CE66_D1CE_4E5B)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// An integer in [lo, hi).
    pub fn integer(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo).max(1) as u64) as i64
    }

    /// Normal, by Box and Muller.
    pub fn normal(&mut self, mean: f64, deviation: f64) -> f64 {
        let u1 = 1.0 - self.unit();
        let u2 = self.unit();
        mean + deviation * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

/// Tileable fractal noise in [0, 1]: octaves of periodic gradient noise, from about `scale / 2`
/// waves across the tile, each finer octave weaker by the slope of texgen's spectrum.
pub fn tile_noise(size: usize, scale: f64, seed: i64, detail: f64) -> Plane {
    let mut rng = Rng::new(seed);
    let mut out = Plane::new(size, size);
    let base = (scale * 0.5).round().max(1.0) as usize;
    let cutoff = (scale * 2f64.powf(detail / 2.0)).min(size as f64 / 2.0);
    let exponent = 1.0 + 0.15 * detail;
    let mut period = base;
    while period as f64 <= cutoff.max(base as f64) {
        // A gradient at each lattice point, the lattice wrapped around the tile.
        let grads: Vec<(f64, f64)> = (0..period * period)
            .map(|_| {
                let a = rng.uniform(0.0, 2.0 * PI);
                (a.cos(), a.sin())
            })
            .collect();
        let gaussian = (-(period as f64 / (scale * 2f64.powf(detail / 2.0))).powi(2)).exp();
        let amp = (period as f64 / base as f64).powf(-(exponent - 1.0) - 0.5) * gaussian.max(0.05);
        let cell = size as f64 / period as f64;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f64 / cell, y as f64 / cell);
                let (x0, y0) = (fx.floor() as usize % period, fy.floor() as usize % period);
                let (x1, y1) = ((x0 + 1) % period, (y0 + 1) % period);
                let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
                let dot = |gx: usize, gy: usize, dx: f64, dy: f64| {
                    let (a, b) = grads[gy * period + gx];
                    a * dx + b * dy
                };
                let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
                let (u, v) = (fade(tx), fade(ty));
                let n00 = dot(x0, y0, tx, ty);
                let n10 = dot(x1, y0, tx - 1.0, ty);
                let n01 = dot(x0, y1, tx, ty - 1.0);
                let n11 = dot(x1, y1, tx - 1.0, ty - 1.0);
                let top = n00 + (n10 - n00) * u;
                let bottom = n01 + (n11 - n01) * u;
                out.v[y * size + x] += amp * (top + (bottom - top) * v);
            }
        }
        if period * 2 > size {
            break;
        }
        period *= 2;
    }
    let lo = out.v.iter().copied().fold(f64::INFINITY, f64::min);
    for v in &mut out.v {
        *v -= lo;
    }
    let hi = out.v.iter().copied().fold(0.0, f64::max).max(1e-9);
    for v in &mut out.v {
        *v /= hi;
    }
    out
}

// --- drawing, for the random kinds --------------------------------------------------------------

/// The distance from (px, py) to the segment a-b.
fn segment_distance(px: f64, py: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + dx * t, a.1 + dy * t);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// A canvas of coverage: strokes drawn with soft edges (one texel), and a glow beside them.
struct Canvas {
    plane: Plane,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            plane: Plane::new(width, height),
        }
    }

    /// Over the texels near a box, the value of each from its texel centre.
    fn paint(&mut self, bounds: (f64, f64, f64, f64), value: impl Fn(f64, f64) -> f64) {
        let (w, h) = (self.plane.width as f64, self.plane.height as f64);
        let x0 = bounds.0.floor().clamp(0.0, w) as usize;
        let x1 = bounds.2.ceil().clamp(0.0, w) as usize;
        let y0 = bounds.1.floor().clamp(0.0, h) as usize;
        let y1 = bounds.3.ceil().clamp(0.0, h) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let v = value(x as f64 + 0.5, y as f64 + 0.5);
                let slot = &mut self.plane.v[y * self.plane.width + x];
                *slot = slot.max(v);
            }
        }
    }

    /// A line through the points, `width` texels wide, rounded at its joints.
    fn polyline(&mut self, points: &[(f64, f64)], width: f64) {
        let half = width / 2.0;
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let pad = half + 1.0;
            let bounds = (
                a.0.min(b.0) - pad,
                a.1.min(b.1) - pad,
                a.0.max(b.0) + pad,
                a.1.max(b.1) + pad,
            );
            self.paint(bounds, |x, y| {
                (half + 0.5 - segment_distance(x, y, a, b)).clamp(0.0, 1.0)
            });
        }
    }

    /// A glow along the line: a Gaussian across it, `sigma` texels, at most `peak`.
    fn glow(&mut self, points: &[(f64, f64)], sigma: f64, peak: f64) {
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let pad = sigma * 3.0;
            let bounds = (
                a.0.min(b.0) - pad,
                a.1.min(b.1) - pad,
                a.0.max(b.0) + pad,
                a.1.max(b.1) + pad,
            );
            self.paint(bounds, |x, y| {
                let d = segment_distance(x, y, a, b);
                peak * (-(d * d) / (2.0 * sigma * sigma)).exp()
            });
        }
    }

    /// A circle line about (cx, cy).
    fn circle(&mut self, cx: f64, cy: f64, radius: f64, width: f64) {
        let half = width / 2.0;
        let pad = radius + half + 1.0;
        let inner = (radius - half - 1.0).max(0.0);
        self.paint((cx - pad, cy - pad, cx + pad, cy + pad), |x, y| {
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            if d < inner {
                return 0.0;
            }
            (half + 0.5 - (d - radius).abs()).clamp(0.0, 1.0)
        });
    }
}

// --- shapes -------------------------------------------------------------------------------------

/// The parameters of a kind, read from the description of its texture.
struct Params<'a> {
    spec: &'a Value,
}

impl Params<'_> {
    fn f(&self, key: &str, default: f64) -> Result<f64, String> {
        match self.spec.get(key) {
            None => Ok(default),
            Some(v) => v
                .as_f64()
                .ok_or_else(|| format!("{key}: a number expected, {} found", v.repr())),
        }
    }

    fn i(&self, key: &str, default: i64) -> Result<i64, String> {
        match self.spec.get(key) {
            None => Ok(default),
            Some(crate::json::Value::Int(v)) => Ok(*v),
            Some(crate::json::Value::Float(v)) if v.fract() == 0.0 => Ok(*v as i64),
            Some(v) => Err(format!("{key}: an integer expected, {} found", v.repr())),
        }
    }

    fn size(&self, default: i64) -> Result<usize, String> {
        let size = self.i("size", default)?;
        if !(1..=4096).contains(&size) {
            return Err(format!("size {size}"));
        }
        Ok(size as usize)
    }
}

/// The default of a parameter of a kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Param {
    Num(f64),
    /// `#rrggbb`.
    Color(&'static str),
}

use Param::{Color as C, Num as N};

/// Every kind, with its parameters and their defaults.
pub const KINDS: [(&str, &[(&str, Param)]); 18] = [
    (
        "petal",
        &[
            ("size", N(256.0)),
            ("base", C("#fff4f8")),
            ("tip", C("#ff4f9a")),
            ("veins", N(7.0)),
            ("edge", N(0.12)),
            ("glow", N(0.35)),
        ],
    ),
    ("glow", &[("size", N(64.0)), ("falloff", N(2.0))]),
    ("spark", &[("size", N(64.0)), ("core", N(0.15))]),
    (
        "star",
        &[
            ("size", N(128.0)),
            ("rays", N(4.0)),
            ("width", N(0.06)),
            ("halo", N(0.35)),
        ],
    ),
    (
        "ring",
        &[
            ("size", N(256.0)),
            ("radius", N(0.8)),
            ("width", N(0.08)),
            ("soft", N(0.05)),
        ],
    ),
    (
        "shockwave",
        &[("size", N(256.0)), ("radius", N(0.85)), ("width", N(0.35))],
    ),
    ("streak", &[("size", N(128.0)), ("height", N(32.0)), ("taper", N(1.0))]),
    (
        "trail",
        &[
            ("size", N(128.0)),
            ("height", N(32.0)),
            ("fade", N(1.0)),
            ("head", N(0.08)),
        ],
    ),
    (
        "smoke",
        &[
            ("size", N(256.0)),
            ("rows", N(4.0)),
            ("cols", N(4.0)),
            ("seed", N(1.0)),
            ("detail", N(5.0)),
        ],
    ),
    (
        "noise",
        &[
            ("size", N(256.0)),
            ("scale", N(4.0)),
            ("seed", N(1.0)),
            ("contrast", N(1.5)),
            ("vfade", N(0.0)),
        ],
    ),
    (
        "swirl",
        &[
            ("size", N(256.0)),
            ("arms", N(3.0)),
            ("twist", N(2.5)),
            ("width", N(0.35)),
            ("hole", N(0.2)),
        ],
    ),
    (
        "rune",
        &[
            ("size", N(512.0)),
            ("seed", N(1.0)),
            ("rings", N(3.0)),
            ("glyphs", N(12.0)),
        ],
    ),
    (
        "lightning",
        &[
            ("size", N(256.0)),
            ("rows", N(2.0)),
            ("cols", N(2.0)),
            ("seed", N(1.0)),
            ("branches", N(2.0)),
        ],
    ),
    ("solid", &[("size", N(8.0))]),
    (
        "leaf",
        &[
            ("size", N(128.0)),
            ("base", C("#b6e27a")),
            ("tip", C("#4f9a32")),
            ("veins", N(6.0)),
        ],
    ),
    (
        "leaves",
        &[
            ("size", N(256.0)),
            ("rows", N(4.0)),
            ("cols", N(4.0)),
            ("veins", N(5.0)),
        ],
    ),
    (
        "bark",
        &[
            ("size", N(256.0)),
            ("dark", C("#3a2616")),
            ("light", C("#7d5a3a")),
            ("seed", N(3.0)),
            ("streaks", N(10.0)),
        ],
    ),
    (
        "soil",
        &[
            ("size", N(256.0)),
            ("color", C("#2e1c10")),
            ("seed", N(5.0)),
            ("cracks", N(0.5)),
        ],
    ),
];

impl Params<'_> {
    fn color(&self, key: &str, default: &str) -> Result<[f64; 3], String> {
        let text = match self.spec.get(key) {
            None => default,
            Some(v) => v.as_str().ok_or_else(|| format!("{key}: #rrggbb expected"))?,
        };
        parse_color(text)
            .map(|c| c.map(f64::from))
            .ok_or_else(|| format!("{key}: #rrggbb expected"))
    }
}

/// Bark, coloured and opaque: grooves running along v (the length of a `tube`), tileable both ways.
fn bark(p: &Params) -> Result<(Plane, Vec<[f64; 3]>), String> {
    let size = p.size(256)?;
    let (d, l) = (p.color("dark", "#3a2616")?, p.color("light", "#7d5a3a")?);
    let (seed, streaks) = (p.i("seed", 3)?, p.f("streaks", 10.0)?);
    let n = tile_noise(size, 4.0, seed, 4.0);
    // the noise averaged over rows: grooves that run lengthwise
    let k = (size / 12).max(1) as i64;
    let mut stretched = Plane::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let sum: f64 = (-k..=k)
                .map(|dy| n.at(x, (y as i64 + dy).rem_euclid(size as i64) as usize))
                .sum();
            stretched.v[y * size + x] = sum / (2 * k + 1) as f64;
        }
    }
    let lo = stretched.v.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = stretched.v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1e-6);
    let mut rgb = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            let v = stretched.v[y * size + x];
            let grooves = 0.5 + 0.5 * (2.0 * PI * (x as f64 / size as f64 * streaks + v * 1.5)).sin();
            let t = ((v - lo) / span * 0.6 + grooves * 0.5 - 0.1).clamp(0.0, 1.0);
            rgb.push(std::array::from_fn(|c| d[c] + (l[c] - d[c]) * t));
        }
    }
    Ok((Plane::from_fn(size, size, |_, _| 1.0), rgb))
}

/// The alpha and the vein shade of a leaf: u along (0 stem, 1 tip), v across (-1..1).
fn leaf_shape(u: f64, v: f64, veins: f64) -> (f64, f64) {
    let half = 0.92 * (PI * u.clamp(0.0, 1.0)).sin().clamp(0.0, 1.0).powf(0.75) * (1.0 - 0.15 * u);
    let mut a = 1.0 - smooth(half - 0.06, half, v.abs());
    if !(0.0..=1.0).contains(&u) {
        a = 0.0;
    }
    let rib = (-(v / 0.05).powi(2)).exp();
    let side = ((u * veins - v.abs() * 1.2) * PI).sin().abs();
    let inside = if v.abs() < half { 1.0 } else { 0.0 };
    let vein = 1.0 - 0.25 * rib - 0.12 * (1.0 - smooth(0.0, 0.15, side)) * inside;
    (a.clamp(0.0, 1.0), vein)
}

/// A leaf, coloured: x across, y from the stem (top row) to the tip; a midrib and side veins; the
/// alpha draws its outline.
fn leaf(p: &Params) -> Result<(Plane, Vec<[f64; 3]>), String> {
    let size = p.size(128)?;
    let (b, t) = (p.color("base", "#b6e27a")?, p.color("tip", "#4f9a32")?);
    let veins = p.f("veins", 6.0)?;
    let mut rgb = Vec::with_capacity(size * size);
    let alpha = grid(size, |x, y| {
        let u = (y + 1.0) / 2.0;
        let (a, vein) = leaf_shape(u, x, veins);
        let mix = smooth(0.0, 1.0, u);
        rgb.push(std::array::from_fn(|c| {
            ((b[c] + (t[c] - b[c]) * mix) * vein).clamp(0.0, 255.0)
        }));
        a
    });
    Ok((alpha, rgb))
}

/// A flipbook of a leaf tumbling (rows x cols cells): turning in the plane, seen edge on and back,
/// white (a particle's colour tints it).
fn leaves(p: &Params) -> Result<Plane, String> {
    let size = p.size(256)?;
    let (rows, cols) = (p.i("rows", 4)?.max(1) as usize, p.i("cols", 4)?.max(1) as usize);
    let veins = p.f("veins", 5.0)?;
    let cell = (size / rows.max(cols)).max(1);
    let mut out = Plane::new(cell * cols, cell * rows);
    let n = rows * cols;
    let c0 = (cell as f64 - 1.0) / 2.0;
    let half = cell as f64 / 2.0;
    for k in 0..n {
        let th = 2.0 * PI * k as f64 / n as f64;
        let squash = (2.0 * th).cos().abs().max(0.2); // edge on twice a turn
        let (c, s) = (th.cos(), th.sin());
        let (rr, cc) = (k / cols, k % cols);
        for y in 0..cell {
            for x in 0..cell {
                let (gx, gy) = ((x as f64 - c0) / half, (y as f64 - c0) / half);
                let (xr, yr) = (c * gx + s * gy, -s * gx + c * gy);
                let (a, vein) = leaf_shape((yr / 0.9 + 1.0) / 2.0, xr / (0.55 * squash), veins);
                out.v[(rr * cell + y) * out.width + cc * cell + x] = a * (0.75 + 0.25 * vein);
            }
        }
    }
    Ok(out)
}

/// Dark earth, round and fading at its rim, broken by cracks: a ground decal under roots.
fn soil(p: &Params) -> Result<(Plane, Vec<[f64; 3]>), String> {
    let size = p.size(256)?;
    let colour = p.color("color", "#2e1c10")?;
    let (seed, cracks) = (p.i("seed", 5)?, p.f("cracks", 0.5)?);
    let n = tile_noise(size, 5.0, seed, 4.0);
    let m = tile_noise(size, 10.0, seed + 1, 3.0);
    let mut rgb = Vec::with_capacity(size * size);
    let c = (size as f64 - 1.0) / 2.0;
    let half = size as f64 / 2.0;
    let alpha = Plane::from_fn(size, size, |x, y| {
        let r = ((x as f64 - c) / half).hypot((y as f64 - c) / half);
        let (nv, mv) = (n.at(x, y), m.at(x, y));
        let crack = 1.0 - (-((mv - 0.5).abs() / 0.03).powi(2)).exp() * cracks;
        let a = (1.0 - smooth(0.45, 0.95, r + (nv - 0.5) * 0.35)) * (0.75 + 0.25 * nv);
        rgb.push(colour.map(|k| (k * (0.7 + 0.6 * nv) * crack).clamp(0.0, 255.0)));
        (a * (0.85 + 0.15 * crack)).clamp(0.0, 1.0)
    });
    Ok((alpha, rgb))
}

/// A petal skin, coloured: x across the petal, y from the base (top row) to the tip. Light at the
/// base, the tip colour toward the tip, darker veins fanning out, a bright rim; the alpha denser
/// at the rim and the base, clearer in the middle. For a `petal` mesh, whose outline is the shape.
fn petal(p: &Params) -> Result<(Plane, Vec<[f64; 3]>), String> {
    let size = p.size(256)?;
    let color = |key: &str, default: &str| -> Result<[f64; 3], String> {
        let text = match p.spec.get(key) {
            None => default,
            Some(v) => v.as_str().ok_or_else(|| format!("{key}: #rrggbb expected"))?,
        };
        parse_color(text)
            .map(|c| c.map(f64::from))
            .ok_or_else(|| format!("{key}: #rrggbb expected"))
    };
    let (b, t) = (color("base", "#fff4f8")?, color("tip", "#ff4f9a")?);
    let (veins, edge, glow) = (p.f("veins", 7.0)?, p.f("edge", 0.12)?, p.f("glow", 0.35)?);
    let mut rgb = Vec::with_capacity(size * size);
    let alpha = grid(size, |x, y| {
        let (u, v) = ((y + 1.0) / 2.0, x); // along: 0 base, 1 tip; across: -1..1
        let mix = smooth(0.05, 0.95, u);
        let mut c: [f64; 3] = std::array::from_fn(|k| b[k] * (1.0 - mix) + t[k] * mix);
        let ang = v / u.max(0.05) * 0.5;
        let vein = (ang * PI * veins / 2.0).sin().abs();
        let vein = 1.0 - 0.15 * (1.0 - smooth(0.0, 0.12, vein)) * smooth(0.05, 0.3, u);
        let rim = smooth(1.0 - edge, 1.0, v.abs());
        for k in &mut c {
            *k *= vein;
            *k += (255.0 - *k) * (rim * 0.5);
        }
        rgb.push(c.map(|k| k.clamp(0.0, 255.0)));
        let a = 0.55 + 0.35 * rim + glow * (1.0 - smooth(0.0, 0.35, u));
        a.clamp(0.0, 1.0) * (1.0 - smooth(0.97, 1.0, v.abs()))
    });
    Ok((alpha, rgb))
}

/// Keys of a texture's description that are not parameters of its kind.
const NOT_PARAMETERS: [&str; 5] = ["kind", "color", "encoding", "wrap", "cells"];

fn glow(p: &Params) -> Result<Plane, String> {
    let falloff = p.f("falloff", 2.0)?;
    Ok(grid(p.size(64)?, |x, y| {
        (1.0 - x.hypot(y)).clamp(0.0, 1.0).powf(falloff)
    }))
}

fn spark(p: &Params) -> Result<Plane, String> {
    let core = p.f("core", 0.15)?;
    Ok(grid(p.size(64)?, |x, y| {
        let r = x.hypot(y);
        ((-(r / core).powi(2)).exp() + 0.5 * (1.0 - r).clamp(0.0, 1.0).powi(3)).clamp(0.0, 1.0)
    }))
}

fn star(p: &Params) -> Result<Plane, String> {
    let rays = p.f("rays", 4.0)?;
    let (width, halo) = (p.f("width", 0.06)?, p.f("halo", 0.35)?);
    Ok(grid(p.size(128)?, |x, y| {
        let r = x.hypot(y);
        let th = y.atan2(x);
        let mut a: f64 = 0.0;
        for k in 0..rays as i64 {
            let ang = PI * k as f64 / rays;
            let d = (th - ang).sin().abs() * r;
            a = a.max((-(d / width).powi(2)).exp() * (1.0 - r).clamp(0.0, 1.0).powf(1.5));
        }
        a = a.max((-(r / halo).powi(2)).exp());
        a * smooth(1.0, 0.9, r)
    }))
}

fn ring(p: &Params) -> Result<Plane, String> {
    let (radius, width, soft) = (p.f("radius", 0.8)?, p.f("width", 0.08)?, p.f("soft", 0.05)?);
    Ok(grid(p.size(256)?, |x, y| {
        let d = (x.hypot(y) - radius).abs() - width / 2.0;
        1.0 - smooth(0.0, soft.max(1e-3), d)
    }))
}

fn shockwave(p: &Params) -> Result<Plane, String> {
    let (radius, width) = (p.f("radius", 0.85)?, p.f("width", 0.35)?);
    Ok(grid(p.size(256)?, |x, y| {
        let r = x.hypot(y);
        let inner = smooth(radius - width, radius, r);
        let outer = 1.0 - smooth(radius, (radius + 0.06).min(1.0), r);
        inner * inner * outer
    }))
}

fn streak(p: &Params) -> Result<Plane, String> {
    let size = p.size(128)?;
    let height = p.i("height", 32)?.max(1) as usize;
    let taper = p.f("taper", 1.0)?;
    Ok(Plane::from_fn(size, height, |x, y| {
        let u = x as f64 / (size as f64 - 1.0).max(1.0);
        let v = (y as f64 - (height as f64 - 1.0) / 2.0) / (height as f64 / 2.0);
        let along = (PI * u).sin().powf(taper);
        (along * (1.0 - v * v).powi(2)).clamp(0.0, 1.0)
    }))
}

fn trail(p: &Params) -> Result<Plane, String> {
    let size = p.size(128)?;
    let height = p.i("height", 32)?.max(1) as usize;
    let (fade, head) = (p.f("fade", 1.0)?, p.f("head", 0.08)?);
    Ok(Plane::from_fn(size, height, |x, y| {
        let u = x as f64 / (size as f64 - 1.0).max(1.0);
        let v = (y as f64 - (height as f64 - 1.0) / 2.0) / (height as f64 / 2.0);
        let ends = (1.0 - u).powf(fade) * if head != 0.0 { smooth(0.0, head, u) } else { 1.0 };
        (ends * (1.0 - v.abs().powi(2)).powf(1.5)).clamp(0.0, 1.0)
    }))
}

fn smoke(p: &Params) -> Result<Plane, String> {
    let size = p.size(256)?;
    let (rows, cols) = (p.i("rows", 4)?.max(1) as usize, p.i("cols", 4)?.max(1) as usize);
    let (seed, detail) = (p.i("seed", 1)?, p.f("detail", 5.0)?);
    let cell = (size / rows.max(cols)).max(1);
    let mut out = Plane::new(cell * cols, cell * rows);
    let base = tile_noise(cell * 2, 3.0, seed, detail);
    let n = rows * cols;
    for k in 0..n {
        let t = k as f64 / (n.max(2) - 1) as f64;
        let (ox, oy) = ((k * 37) % cell, (k * 61) % cell);
        let (rr, cc) = (k / cols, k % cols);
        let c = (cell as f64 - 1.0) / 2.0;
        let half = cell as f64 / 2.0;
        for y in 0..cell {
            for x in 0..cell {
                let (gx, gy) = ((x as f64 - c) / half, (y as f64 - c) / half);
                // the noise rolled by (ox, oy), as np.roll
                let nz = base.at((x + base.width - ox) % base.width, (y + base.height - oy) % base.height);
                let r = gx.hypot(gy) / (0.7 + 0.25 * t);
                let mask = (1.0 - r).clamp(0.0, 1.0).powf(0.8);
                let a = (mask * (0.6 + 1.4 * (nz - 0.3)) * 1.6 * (1.0 - 0.5 * t)).clamp(0.0, 1.0);
                out.v[(rr * cell + y) * out.width + cc * cell + x] = a;
            }
        }
    }
    Ok(out)
}

fn noise(p: &Params) -> Result<Plane, String> {
    let size = p.size(256)?;
    let (scale, seed) = (p.f("scale", 4.0)?, p.i("seed", 1)?);
    let (contrast, vfade) = (p.f("contrast", 1.5)?, p.f("vfade", 0.0)?);
    let mut n = tile_noise(size, scale, seed, 5.0);
    for (at, v) in n.v.iter_mut().enumerate() {
        *v = ((*v - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
        if vfade != 0.0 {
            let row = (at / size) as f64;
            *v *= (PI * (row + 0.5) / size as f64).sin().powf(vfade);
        }
    }
    Ok(n)
}

fn swirl(p: &Params) -> Result<Plane, String> {
    let (arms, twist) = (p.f("arms", 3.0)?, p.f("twist", 2.5)?);
    let (width, hole) = (p.f("width", 0.35)?, p.f("hole", 0.2)?);
    Ok(grid(p.size(256)?, |x, y| {
        let r = x.hypot(y);
        let th = y.atan2(x);
        let phase = (th * arms + twist * 2.0 * PI * r) / (2.0 * PI);
        let band = (phase - phase.round_ties_even()).abs();
        let a = (1.0 - band / (width / 2.0)).clamp(0.0, 1.0);
        a * smooth(1.0, 0.75, r) * smooth(hole * 0.5, hole, r)
    }))
}

/// A magic circle: concentric rings, ticks between the outer two, a band of glyphs of random
/// strokes, an inner star.
fn rune(p: &Params) -> Result<Plane, String> {
    let size = p.size(512)?;
    let (seed, rings, glyphs) = (
        p.i("seed", 1)?,
        p.i("rings", 3)?.max(1) as usize,
        p.i("glyphs", 12)?.max(0),
    );
    let mut rng = Rng::new(seed);
    let mut canvas = Canvas::new(size, size);
    let c = size as f64 / 2.0;
    // texgen's strokes, drawn at four times the size and blurred: a little wider here, soft-edged
    let lw = (size as f64 / 160.0).max(0.5) * 1.25;
    let radii: Vec<f64> = (0..rings)
        .map(|k| {
            if rings == 1 {
                0.95
            } else {
                0.95 - 0.4 * k as f64 / (rings - 1) as f64
            }
        })
        .collect();
    for r in &radii {
        canvas.circle(c, c, r * c - lw / 2.0, lw);
    }
    let second = radii[1.min(radii.len() - 1)];
    let (r0, r1) = (radii[0] * c, second * c);
    for k in 0..72 {
        let a = 2.0 * PI * k as f64 / 72.0;
        let rr = r0 - (r0 - r1) * if k % 6 != 0 { 0.35 } else { 0.7 };
        canvas.polyline(
            &[
                (c + r0 * a.cos(), c + r0 * a.sin()),
                (c + rr * a.cos(), c + rr * a.sin()),
            ],
            lw,
        );
    }
    let last = radii[radii.len() - 1];
    let band = (second + last) / 2.0 * c;
    let gs = (second - last) * c * 0.7;
    for k in 0..glyphs {
        let a = 2.0 * PI * k as f64 / glyphs as f64;
        let (gx, gy) = (c + band * a.cos(), c + band * a.sin());
        let count = rng.integer(3, 6);
        let points: Vec<(f64, f64)> = (0..count)
            .map(|_| {
                let (px, py) = (rng.uniform(-0.5, 0.5) * gs * 1.3, rng.uniform(-0.5, 0.5) * gs * 1.3);
                (gx + px * a.cos() - py * a.sin(), gy + px * a.sin() + py * a.cos())
            })
            .collect();
        canvas.polyline(&points, lw);
    }
    let n = if rng.integer(0, 2) == 0 { 5 } else { 7 };
    let big = last * c * 0.95;
    let points: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let a = 2.0 * PI * (k * 2) as f64 / n as f64 - PI / 2.0;
            (c + big * a.cos(), c + big * a.sin())
        })
        .collect();
    canvas.polyline(&points, lw);
    Ok(canvas.plane)
}

/// Bolts of lightning, one in each cell, each with its branches: a core and a glow about it.
fn lightning(p: &Params) -> Result<Plane, String> {
    let size = p.size(256)?;
    let (rows, cols) = (p.i("rows", 2)?.max(1) as usize, p.i("cols", 2)?.max(1) as usize);
    let (seed, branches) = (p.i("seed", 1)?, p.i("branches", 2)?.max(0));
    let mut rng = Rng::new(seed);
    let cell = (size / rows.max(cols)).max(1);
    let mut core = Canvas::new(cell * cols, cell * rows);
    let mut glow = Canvas::new(cell * cols, cell * rows);
    let c = cell as f64;
    let sigma = c / 40.0;
    struct Bolt {
        from: (f64, f64),
        to: (f64, f64),
        width: f64,
        depth: u32,
    }
    for k in 0..rows * cols {
        let (r, cc) = (k / cols, k % cols);
        let (ox, oy) = (cc as f64 * c, r as f64 * c);
        let mut stack = vec![Bolt {
            from: (ox + c * 0.5 + rng.normal(0.0, c * 0.05), oy + c * 0.03),
            to: (ox + c * 0.5 + rng.normal(0.0, c * 0.1), oy + c * 0.97),
            width: (c / 50.0).max(0.5),
            depth: 1,
        }];
        while let Some(bolt) = stack.pop() {
            let n = 12;
            let mut points = vec![bolt.from];
            for s in 1..n {
                let t = s as f64 / n as f64;
                points.push((
                    bolt.from.0 + (bolt.to.0 - bolt.from.0) * t + rng.normal(0.0, c * 0.05),
                    bolt.from.1 + (bolt.to.1 - bolt.from.1) * t + rng.normal(0.0, c * 0.02),
                ));
            }
            points.push(bolt.to);
            core.polyline(&points, bolt.width);
            // the glow of a stroke blurred: its width over the spread, as a Gaussian blur keeps it
            let peak = (2.5 * bolt.width / (sigma * (2.0 * PI).sqrt())).min(1.0);
            glow.glow(&points, sigma, peak);
            if bolt.depth > 0 {
                for _ in 0..branches {
                    let i = rng.integer(2, n as i64 - 2) as usize;
                    let (bx, by) = points[i];
                    stack.push(Bolt {
                        from: (bx, by),
                        to: (bx + rng.normal(0.0, c * 0.25), by + rng.normal(c * 0.25, c * 0.1).abs()),
                        width: (bolt.width / 2.0).max(0.5),
                        depth: bolt.depth - 1,
                    });
                }
            }
        }
    }
    let mut out = core.plane;
    for (v, g) in out.v.iter_mut().zip(&glow.plane.v) {
        *v = (*v + g).clamp(0.0, 1.0);
    }
    Ok(out)
}

fn solid(p: &Params) -> Result<Plane, String> {
    let size = p.size(8)?;
    Ok(Plane::from_fn(size, size, |_, _| 1.0))
}

/// The image with its border faded to transparent black over `width` of the side (the outermost
/// 2 texels clear), so that a clamped projection repeats nothing.
pub fn edge_fade(image: &Image, width: f64) -> Image {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut out = image.clone();
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64, y as f64);
            let d = (fx.min(w as f64 - 1.0 - fx) / w as f64).min(fy.min(h as f64 - 1.0 - fy) / h as f64);
            let m = smooth(2.0 / w.min(h) as f64, width, d);
            let at = (y * w + x) * 4;
            for channel in &mut out.rgba[at..at + 4] {
                *channel = (f64::from(*channel) * m).round_ties_even() as u8;
            }
        }
    }
    out
}

/// The colours of a kind that draws them, a texel each.
pub type Colours = Vec<[f64; 3]>;

/// The alpha of a kind (and its colours, for a kind that draws them), its parameters read from `spec` (other keys of a texture's description
/// left aside).
pub fn draw(kind: &str, spec: &Value) -> Result<(Plane, Option<Colours>), String> {
    let Some((_, known)) = KINDS.iter().find(|(k, _)| *k == kind) else {
        let names: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
        return Err(format!("unknown texture kind {kind} (known: {})", names.join(", ")));
    };
    for (key, _) in spec.as_map().unwrap_or_default() {
        if !NOT_PARAMETERS.contains(&key.as_str()) && !known.iter().any(|(k, _)| k == key) {
            return Err(format!("{kind}: unknown parameter {key}"));
        }
    }
    let p = Params { spec };
    let coloured = match kind {
        "petal" => Some(petal(&p)?),
        "leaf" => Some(leaf(&p)?),
        "bark" => Some(bark(&p)?),
        "soil" => Some(soil(&p)?),
        _ => None,
    };
    if let Some((alpha, rgb)) = coloured {
        return Ok((alpha, Some(rgb)));
    }
    let alpha = match kind {
        "glow" => glow(&p),
        "spark" => spark(&p),
        "star" => star(&p),
        "ring" => ring(&p),
        "shockwave" => shockwave(&p),
        "streak" => streak(&p),
        "trail" => trail(&p),
        "smoke" => smoke(&p),
        "noise" => noise(&p),
        "swirl" => swirl(&p),
        "rune" => rune(&p),
        "lightning" => lightning(&p),
        "leaves" => leaves(&p),
        _ => solid(&p),
    }?;
    Ok((alpha, None))
}

/// `#rrggbb` as bytes.
pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let t = text.trim_start_matches('#');
    let part = |at: usize| t.get(at..at + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
    Some([part(0)?, part(2)?, part(4)?])
}

/// The texture as an image: white, or `color` baked in.
pub fn make(kind: &str, color: Option<&str>, spec: &Value) -> Result<Image, String> {
    let (plane, colours) = draw(kind, spec)?;
    if let Some(colours) = colours {
        // a kind that draws its own colours
        let mut image = rgba(&plane, [0; 3]);
        for (texel, c) in image.rgba.as_chunks_mut::<4>().0.iter_mut().zip(&colours) {
            for k in 0..3 {
                texel[k] = c[k].round_ties_even() as u8;
            }
        }
        return Ok(image);
    }
    let rgb = match color.filter(|c| !c.is_empty()) {
        Some(c) => parse_color(c).ok_or_else(|| format!("colour {c}: #rrggbb expected"))?,
        None => [255; 3],
    };
    Ok(rgba(&plane, rgb))
}

/// The rows and columns of a flipbook kind (1, 1 for the others).
pub fn cells(kind: &str, spec: &Value) -> Result<(i64, i64), String> {
    let p = Params { spec };
    match kind {
        "smoke" => Ok((p.i("rows", 4)?, p.i("cols", 4)?)),
        "lightning" => Ok((p.i("rows", 2)?, p.i("cols", 2)?)),
        "leaves" => Ok((p.i("rows", 4)?, p.i("cols", 4)?)),
        _ => Ok((1, 1)),
    }
}
