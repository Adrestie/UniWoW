//! A spell effect model (M2 264, its skin, its textures) made from its description, as
//! `tools/vfx/forge.py` of WoW-mods makes it: the same description, the same model to the byte,
//! the same notes, refused for the same reasons. Particle emitters, ribbons and small generated
//! meshes, on bones that spin, orbit, bob and scale.
//!
//! Distances in yards, times in milliseconds, angles in degrees; values changing over the
//! sequence take keys `[[t, value], ...]`, t from 0 to 1.

use crate::json::Value;
use crate::m2::{self, Batch, Bone, Color, FBlock, Material, Model, Particle, Ribbon, Sequence, Skin, Submesh};
use crate::m2::{Texture, TextureTransform, Track, Weight};
use crate::texgen::{self, Image};
use crate::{blp, py, validate};
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

pub const FIX16: i64 = 32767;
pub const BLENDS: [(&str, u16); 7] = [
    ("opaque", 0),
    ("key", 1),
    ("alpha", 2),
    ("add_opaque", 3),
    ("add", 4),
    ("mod", 5),
    ("mod2x", 6),
];
pub const PARTICLE_BLENDS: [(&str, u8); 5] = [("opaque", 0), ("key", 1), ("alpha", 2), ("add", 4), ("mod", 5)];
/// The client's own emitters (5,445 in 1,564 spell models): 0x20029 is their most common set of
/// flags; forge starts from it.
pub const BASE_PARTICLE_FLAGS: u32 = 0x20029;
/// Names for the bits of a 3.3.5 emitter.
pub const PARTICLE_FLAGS: [(&str, u32); 8] = [
    ("lit", 0x1),
    ("world_space", 0x8),
    ("no_trail", 0x10),
    ("model_space", 0x80),
    ("pinned", 0x400),
    ("no_billboard", 0x1000),
    ("random_cell", 0x10000),
    ("outward", 0x20000),
];
pub const MATERIAL_FLAGS: [(&str, u32); 5] = [
    ("unlit", 0x1),
    ("no_fog", 0x2),
    ("two_sided", 0x4),
    ("no_depth_test", 0x8),
    ("no_depth_write", 0x10),
];
/// 0x17, common in the client's spells.
pub const DEFAULT_MATERIAL: [&str; 4] = ["unlit", "no_fog", "two_sided", "no_depth_write"];
pub const SKIN_PALETTE: u32 = 21;
/// The palette sizes of the client's own skins: the smallest holding the largest submesh.
pub const BONE_PALETTES: [u32; 3] = [21, 53, 64];
/// Bones of one submesh at most (the client's largest skinned palette).
pub const SUBMESH_BONES: usize = 64;
/// The material of the client's environment-map layers (0x13).
pub const ENV_MATERIAL: [&str; 3] = ["unlit", "no_fog", "no_depth_write"];
/// What the client's own spell models stay under.
pub const PARTICLE_LOAD: f64 = 800.0;
pub const MODEL_LOAD: f64 = 2000.0;
pub const EMITTERS: usize = 24;
pub const RIBBON_EDGES: f64 = 64.0;
/// The joints of a bent mesh at most: 7 bones, well under the palette.
pub const BEND_JOINTS: usize = 6;
pub const PARTICLE_KEYS: usize = 8;
/// Ground guard: the steepest slope a ground effect is expected to stand on.
pub const SLOPE_DEG: f64 = 20.0;
/// The flags of a batch laid on the terrain by the client (Blizzard's ground circles).
pub const PROJECTED_BATCH: u8 = 0x1C;

fn slope() -> f64 {
    SLOPE_DEG.to_radians().tan()
}

/// Why a description makes no model.
#[derive(Clone, Debug, PartialEq)]
pub struct ForgeError(pub String);

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ForgeError {}

type Result<T> = std::result::Result<T, ForgeError>;

fn fail<T>(text: impl Into<String>) -> Result<T> {
    Err(ForgeError(text.into()))
}

// --- values of the description, read as Python reads them ---------------------------------------

/// `float(v)`.
pub fn num(v: &Value) -> Result<f64> {
    match v {
        Value::Int(x) => Ok(*x as f64),
        Value::Float(x) => Ok(*x),
        Value::Bool(x) => Ok(f64::from(u8::from(*x))),
        Value::Str(text) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| ForgeError(format!("could not convert string to float: '{text}'"))),
        other => fail(format!("a number expected, {} found", other.repr())),
    }
}

/// `int(v)`.
pub fn int(v: &Value) -> Result<i64> {
    match v {
        Value::Int(x) => Ok(*x),
        Value::Float(x) if x.is_finite() => Ok(x.trunc() as i64),
        Value::Bool(x) => Ok(i64::from(*x)),
        Value::Str(text) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| ForgeError(format!("invalid literal for int(): '{text}'"))),
        other => fail(format!("an integer expected, {} found", other.repr())),
    }
}

fn float_or(map: &Value, key: &str, default: f64) -> Result<f64> {
    map.get(key).map_or(Ok(default), num)
}

fn int_or(map: &Value, key: &str, default: i64) -> Result<i64> {
    map.get(key).map_or(Ok(default), int)
}

fn str_or<'a>(map: &'a Value, key: &str, default: &'a str) -> Result<&'a str> {
    match map.get(key) {
        None => Ok(default),
        Some(Value::Str(text)) => Ok(text),
        Some(other) => fail(format!("{key}: a name expected, {} found", other.repr())),
    }
}

fn list(v: &Value) -> Result<&[Value]> {
    v.as_list()
        .ok_or_else(|| ForgeError(format!("a list expected, {} found", v.repr())))
}

fn vec3(v: Option<&Value>) -> Result<[f64; 3]> {
    match v {
        None => Ok([0.0; 3]),
        Some(v) => {
            let items = list(v)?;
            if items.len() != 3 {
                return fail(format!("three numbers expected, {} found", v.repr()));
            }
            Ok([num(&items[0])?, num(&items[1])?, num(&items[2])?])
        }
    }
}

fn f32x3(v: [f64; 3]) -> [f32; 3] {
    v.map(|x| x as f32)
}

/// A colour: `#rrggbb` as 0..255 each, or a list of numbers.
pub fn hex_rgb(c: &Value) -> Result<[f64; 3]> {
    match c {
        Value::List(items) => {
            if items.len() != 3 {
                return fail(format!("a colour of three numbers expected, {} found", c.repr()));
            }
            Ok([num(&items[0])?, num(&items[1])?, num(&items[2])?])
        }
        Value::Str(text) => {
            let t = text.trim_start_matches('#');
            let part = |at: usize| {
                t.get(at..at + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .map(f64::from)
                    .ok_or_else(|| ForgeError(format!("colour {text}: #rrggbb expected")))
            };
            Ok([part(0)?, part(2)?, part(4)?])
        }
        other => fail(format!("a colour expected, {} found", other.repr())),
    }
}

/// The keys `[[t, v], ...]`, sorted by time (stable, as Python's `sorted`).
fn sorted_keys(keys: &[Value]) -> Result<Vec<(f64, &Value)>> {
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        match key.as_list() {
            Some([t, v, ..]) => out.push((num(t)?, v)),
            _ => return fail(format!("a key [t, value] expected, {} found", key.repr())),
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// The time of a key over the sequence, in milliseconds.
fn key_time(t: f64, duration: i64) -> u32 {
    py::round_int(t.clamp(0.0, 1.0) * duration as f64) as u32
}

/// The phases (sequences) of a state model, as the client's own (barkskin, frost nova: 231 of
/// its 1,564 spell models): Stand once as its birth, Hold (158) looping while the aura lasts,
/// Decay (159) once as it ends; those played once carry the flag 0x01.
pub const PHASES: [(&str, u16, u32); 3] = [("birth", 0, 0x21), ("hold", 158, 0x20), ("decay", 159, 0x21)];

/// The timing of the model being made: the loop, its phases, and the global sequence the tracks
/// that only loop run on (-1 with one phase).
struct Time {
    duration: i64,
    gs: i16,
    phases: Vec<(String, i64)>,
}

impl Time {
    /// One value all along.
    fn fixed<T>(&self, value: T) -> Track<T> {
        Track {
            global_sequence: self.gs,
            ..Track::fixed(value)
        }
    }

    /// Keys over the loop.
    fn keyed<T>(&self, interpolation: u16, times: Vec<u32>, values: Vec<T>) -> Track<T> {
        Track {
            global_sequence: self.gs,
            ..Track::keyed(interpolation, times, values)
        }
    }

    /// `{"birth": keys, "hold": keys, "decay": keys}`: one key set a sequence, t a fraction of that
    /// phase. A phase left out holds the last value before it (or the first after it).
    fn phased<T>(&self, v: &Value, conv: impl Fn(&Value) -> Result<T>) -> Result<Track<T>> {
        let entries = v.as_map().unwrap_or_default();
        let mut unknown: Vec<&str> = entries
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !PHASES.iter().any(|(p, _, _)| p == k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort();
            return fail(format!(
                "unknown phase {} (known: birth, hold, decay)",
                unknown.join(", ")
            ));
        }
        let mut missing: Vec<&str> = entries
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !self.phases.iter().any(|(p, _)| p == k))
            .collect();
        if !missing.is_empty() {
            missing.sort();
            return fail(format!(
                "keys for {}, a phase the model does not have (sequences)",
                missing.join(", ")
            ));
        }
        let mut sets: Vec<Option<Vec<(f64, Value)>>> = Vec::new();
        for (name, _) in &self.phases {
            let keys = match v.get(name) {
                None | Some(Value::Null) => None,
                Some(k) if k.is_keys() => Some(k.clone()),
                Some(k) => Some(Value::List(vec![Value::List(vec![Value::Int(0), k.clone()])])),
            };
            sets.push(match keys.filter(Value::truthy) {
                Some(k) => Some(
                    sorted_keys(list(&k)?)?
                        .into_iter()
                        .map(|(t, v)| (t, v.clone()))
                        .collect(),
                ),
                None => None,
            });
        }
        if sets.iter().all(Option::is_none) {
            return fail("phase keys without any key");
        }
        let mut held: Option<Vec<(f64, Value)>> = None;
        for i in 0..sets.len() {
            if sets[i].is_none() {
                let value = match &held {
                    Some(h) => h[h.len() - 1].1.clone(),
                    None => sets[i..].iter().flatten().next().expect("a phase with keys")[0]
                        .1
                        .clone(),
                };
                sets[i] = Some(vec![(0.0, value)]);
            }
            held = sets[i].clone();
        }
        let mut track = Track::empty();
        track.interpolation = 1;
        for ((_, duration), keys) in self.phases.iter().zip(sets.iter().flatten()) {
            track
                .timestamps
                .push(keys.iter().map(|(t, _)| key_time(*t, *duration)).collect());
            track
                .values
                .push(keys.iter().map(|(_, v)| conv(v)).collect::<Result<_>>()?);
        }
        Ok(track)
    }

    /// A value, keys over the loop (linear between them), or keys by phase.
    fn value_or_keys<T>(&self, v: &Value, conv: impl Fn(&Value) -> Result<T>) -> Result<Track<T>> {
        if let Value::Map(_) = v {
            return self.phased(v, conv);
        }
        if v.is_keys() {
            let keys = sorted_keys(list(v)?)?;
            let times = keys.iter().map(|(t, _)| key_time(*t, self.duration)).collect();
            let values = keys.iter().map(|(_, v)| conv(v)).collect::<Result<_>>()?;
            return Ok(self.keyed(1, times, values));
        }
        Ok(self.fixed(conv(v)?))
    }
}

/// A particle's value over its own life: keys `[[t, v], ...]`, or one value all along.
fn part_track<T>(keys: &Value, conv: impl Fn(&Value) -> Result<T>) -> Result<FBlock<T>> {
    let whole;
    let keys = if keys.is_keys() {
        keys
    } else {
        whole = Value::List(vec![
            Value::List(vec![Value::Float(0.0), keys.clone()]),
            Value::List(vec![Value::Float(1.0), keys.clone()]),
        ]);
        &whole
    };
    let keys = sorted_keys(list(keys)?)?;
    if keys.len() > PARTICLE_KEYS {
        return fail(format!("a particle track holds {PARTICLE_KEYS} keys at most"));
    }
    Ok(FBlock {
        timestamps: keys
            .iter()
            .map(|(t, _)| py::round_int(t.clamp(0.0, 1.0) * FIX16 as f64) as i16)
            .collect(),
        values: keys.iter().map(|(_, v)| conv(v)).collect::<Result<_>>()?,
    })
}

/// A float quaternion component as the int16 the client stores.
pub fn comp(q: f64) -> i16 {
    let v = py::round_int(q * 32767.0);
    let v = if q >= 0.0 { v - 32768 } else { v + 32767 };
    v.clamp(-32768, 32767) as i16
}

/// The float a stored quaternion component stands for (the client's reading of `comp`).
pub fn uncomp(v: i16) -> f64 {
    let v = f64::from(v);
    if v < 0.0 {
        (v + 32768.0) / 32767.0
    } else {
        (v - 32767.0) / 32767.0
    }
}

type Quat = [f64; 4];

fn quat(axis: [f64; 3], angle: f64) -> Quat {
    let s = (angle / 2.0).sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, (angle / 2.0).cos()]
}

fn qmul(a: Quat, b: Quat) -> Quat {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

/// [x, y, z] degrees as a quaternion: Z, then Y, then X applied last.
pub fn euler(deg: [f64; 3]) -> [f64; 4] {
    let [x, y, z] = deg.map(f64::to_radians);
    qmul(
        quat([1.0, 0.0, 0.0], x),
        qmul(quat([0.0, 1.0, 0.0], y), quat([0.0, 0.0, 1.0], z)),
    )
}

fn cquat(q: Quat) -> [i16; 4] {
    q.map(comp)
}

fn flags_of(names: Option<&Value>, table: &[(&str, u32)], what: &str) -> Result<u32> {
    let mut v = 0;
    let Some(names) = names.filter(|n| n.truthy()) else {
        return Ok(0);
    };
    for n in list(names)? {
        match n {
            Value::Int(bits) => v |= *bits as u32,
            Value::Str(name) => match table.iter().find(|(k, _)| k == name) {
                Some((_, bits)) => v |= bits,
                None => {
                    let known: Vec<&str> = table.iter().map(|(k, _)| *k).collect();
                    return fail(format!("{what}: unknown flag {name} (known: {})", known.join(", ")));
                }
            },
            other => return fail(format!("{what}: unknown flag {}", other.repr())),
        }
    }
    Ok(v)
}

/// The box of a sequence: -r..r on each axis, then r.
fn bounds(r: f64) -> Vec<u8> {
    let r = r as f32;
    [-r, -r, -r, r, r, r, r].iter().flat_map(|v| v.to_le_bytes()).collect()
}

// --- meshes -------------------------------------------------------------------------------------

/// A generated shape: positions, normals, texture coordinates, triangles; centred on 0, lying in
/// the XY plane (Z up) unless said otherwise.
pub struct Shape {
    pub positions: Vec<[f64; 3]>,
    pub normals: Vec<[f64; 3]>,
    pub uvs: Vec<[f64; 2]>,
    pub triangles: Vec<[u32; 3]>,
}

impl Shape {
    fn grid(&mut self, rows: i64, cols: i64, f: impl Fn(f64, f64) -> ([f64; 3], [f64; 3])) {
        let base = self.positions.len() as u32;
        for i in 0..=rows {
            for j in 0..=cols {
                let (p, n) = f(i as f64 / rows as f64, j as f64 / cols as f64);
                self.positions.push(p);
                self.normals.push(n);
                self.uvs.push([j as f64 / cols as f64, i as f64 / rows as f64]);
            }
        }
        let cols = cols as u32;
        for i in 0..rows as u32 {
            for j in 0..cols {
                let a = base + i * (cols + 1) + j;
                let (b, c, d) = (a + 1, a + cols + 1, a + cols + 2);
                self.triangles.push([a, c, b]);
                self.triangles.push([b, c, d]);
            }
        }
    }
}

pub const SHAPES: [&str; 9] = [
    "gltf", "petal", "tube", "ring", "disc", "plane", "sphere", "cone", "cylinder",
];

/// The shape a mesh description names; a glTF file named from `dir`, the description's folder.
pub fn mesh_shape(spec: &Value, dir: &Path) -> Result<Shape> {
    let kind = str_or(spec, "shape", "")?;
    let seg = int_or(spec, "segments", 48)?;
    let mut s = Shape {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        triangles: Vec::new(),
    };
    if spec.get("shape").is_none() {
        return fail("mesh: no shape");
    }
    let up = [0.0, 0.0, 1.0];
    match kind {
        "gltf" => {
            // a mesh modelled in Blender (File, Export, glTF 2.0: .glb or .gltf): positions,
            // normals, first UV set; Blender's axes kept (Z up, glTF's Y up turned back)
            let file = str_or(spec, "file", "")?;
            if file.is_empty() {
                return fail("mesh gltf: no file");
            }
            let path = dir.join(file);
            let mesh = spec.get("mesh").and_then(Value::as_str);
            let g = crate::gltf::read(&path, mesh).map_err(ForgeError)?;
            let k = float_or(spec, "size", 1.0)?;
            s.positions = g.positions.iter().map(|p| [p[0] * k, p[1] * k, p[2] * k]).collect();
            s.normals = g.normals;
            s.uvs = g.uvs;
            s.triangles = g.triangles;
        }
        "petal" => {
            // a petal from its base (at 0) along +X: pointed outline, cupped across (Z up at the
            // sides), curled along (the tip bends up, or down with a negative curl)
            let (l, w) = (float_or(spec, "length", 1.0)?, float_or(spec, "width", 0.4)?);
            let (cup, curl) = (float_or(spec, "cup", 0.15)?, float_or(spec, "curl", 0.1)?);
            let (rows, cols) = (int_or(spec, "rows", 10)?, int_or(spec, "cols", 6)?);
            // span [u0, u1]: only that stretch of the petal (0 base, 1 tip), so that a petal cut in
            // two (base and tip, each on its bone) bends at the cut; the texture runs on across it
            let (u0, u1) = match spec.get("span") {
                Some(span) => match list(span)? {
                    [a, b] => (num(a)?, num(b)?),
                    _ => return fail("petal: span [u0, u1] expected"),
                },
                None => (0.0, 1.0),
            };
            s.grid(rows, cols, |i, j| {
                let (u, v) = (u0 + (u1 - u0) * i, j * 2.0 - 1.0); // u along, v across (-1..1)
                let half = w / 2.0 * (PI * u.min(0.999)).sin().powf(0.6) * (1.0 - 0.25 * u) + 0.02 * w * (1.0 - u);
                let (x, y) = (l * u, half * v);
                let ratio = if w != 0.0 { half / (w / 2.0) } else { 0.0 };
                let z = cup * (v * v) * ratio + curl * u * u;
                // the normal of the cupped, curled sheet: tilted by the slopes across and along
                let dzdv = 2.0 * cup * v * ratio / half.max(1e-4);
                let dzdu = 2.0 * curl * u / l;
                let n = [-dzdu, -dzdv, 1.0];
                let k = py::sum(n.iter().map(|c| c * c)).sqrt();
                ([x, y, z], n.map(|c| c / k))
            });
            for uv in &mut s.uvs {
                uv[1] = u0 + (u1 - u0) * uv[1];
            }
        }
        "tube" => {
            // a tube from its base (at 0) along +X: radius0 at the base to radius1, closed by a
            // cone; bent a little (bend_y, bend_z: the axis's offset at the far end, growing as the
            // square of the distance). u around, v along, repeating every turn of the base
            let l = float_or(spec, "length", 1.0)?;
            let (r0, r1) = (float_or(spec, "radius0", 0.1)?, float_or(spec, "radius1", 0.05)?);
            let (sides, rows) = (int_or(spec, "sides", 8)?, int_or(spec, "rows", 12)?);
            let (by, bz) = (float_or(spec, "bend_y", 0.0)?, float_or(spec, "bend_z", 0.0)?);
            let per = if 2.0 * PI * r0 != 0.0 { 2.0 * PI * r0 } else { 1.0 };
            s.grid(rows, sides, |u, j| {
                let a = 2.0 * PI * j;
                let r = r0 + (r1 - r0) * u;
                let n = [0.0, a.cos(), a.sin()];
                ([l * u, by * u * u + r * n[1], bz * u * u + r * n[2]], n)
            });
            for uv in &mut s.uvs {
                uv[1] = uv[1] * l / per;
            }
            // the cone closing the far end
            let tip = s.positions.len() as u32;
            s.positions.push([l + 2.0 * r1, by, bz]);
            s.normals.push([1.0, 0.0, 0.0]);
            s.uvs.push([0.5, (l + 2.0 * r1) / per]);
            let last = (rows * (sides + 1)) as u32;
            for j in 0..sides as u32 {
                s.triangles.push([last + j, last + j + 1, tip]);
            }
        }
        "ring" => {
            let (ri, ro) = (float_or(spec, "inner", 0.8)?, float_or(spec, "outer", 1.0)?);
            s.grid(1, seg, |i, j| {
                let r = ri + (ro - ri) * i;
                ([(2.0 * PI * j).cos() * r, (2.0 * PI * j).sin() * r, 0.0], up)
            });
        }
        "disc" => {
            let r = float_or(spec, "radius", 1.0)?;
            // rim > 0 lifts the edge into a shallow bowl, so that a ground circle stays above a slope
            let rim = float_or(spec, "rim", 0.0)?;
            let rows = int_or(spec, "rows", if rim != 0.0 { 8 } else { 1 })?;
            s.grid(rows, seg, |i, j| {
                (
                    [(2.0 * PI * j).cos() * r * i, (2.0 * PI * j).sin() * r * i, rim * i * i],
                    up,
                )
            });
            // the texture of a disc is laid flat, not wrapped: planar coordinates
            s.uvs = s
                .positions
                .iter()
                .map(|p| [0.5 + p[0] / (2.0 * r), 0.5 - p[1] / (2.0 * r)])
                .collect();
        }
        "plane" => {
            let (w, h) = (float_or(spec, "width", 2.0)?, float_or(spec, "length", 2.0)?);
            s.grid(1, 1, |i, j| ([(j - 0.5) * w, (0.5 - i) * h, 0.0], up));
        }
        "sphere" => {
            let r = float_or(spec, "radius", 1.0)?;
            let rings = int_or(spec, "rings", seg.div_euclid(2))?;
            s.grid(rings, seg, |i, j| {
                let (th, ph) = (PI * i, 2.0 * PI * j);
                let n = [th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos()];
                ([n[0] * r, n[1] * r, n[2] * r], n)
            });
        }
        "cone" | "cylinder" => {
            let r = float_or(spec, "radius", 1.0)?;
            let h = float_or(spec, "height", 2.0)?;
            let top = float_or(spec, "top", if kind == "cone" { 0.0 } else { r })?;
            let rows = int_or(spec, "rows", 4)?;
            s.grid(rows, seg, |i, j| {
                let rr = r + (top - r) * i;
                let ph = 2.0 * PI * j;
                (
                    [ph.cos() * rr, ph.sin() * rr, h * i],
                    [ph.cos(), ph.sin(), (r - top) / h.max(1e-6)],
                )
            });
        }
        _ => return fail(format!("mesh shape {kind} (known: {})", SHAPES.join(", "))),
    }
    if let Some(tile) = spec.get("uv_tile") {
        let tile = list(tile)?;
        if tile.len() < 2 {
            return fail("uv_tile: two numbers expected");
        }
        let (tu, tv) = (num(&tile[0])?, num(&tile[1])?);
        for uv in &mut s.uvs {
            *uv = [uv[0] * tu, uv[1] * tv];
        }
    }
    Ok(s)
}

// --- the model ----------------------------------------------------------------------------------

/// Reads the image of a file.
pub type ReadImage = dyn Fn(&Path) -> std::result::Result<Image, String>;

/// The triangles in groups of at most `SUBMESH_BONES` bones (one group when the mesh has that
/// few): a submesh draws with one palette of bones.
fn split(triangles: &[[u32; 3]], influences: &[Influence], bones: usize) -> Vec<Vec<[u32; 3]>> {
    if bones <= SUBMESH_BONES {
        return vec![triangles.to_vec()];
    }
    let bones_of = |tri: &[u32; 3]| -> std::collections::BTreeSet<u8> {
        tri.iter()
            .flat_map(|v| {
                let (w, b) = influences[*v as usize];
                (0..4).filter(move |k| w[*k] != 0).map(move |k| b[k])
            })
            .collect()
    };
    let mut order: Vec<[u32; 3]> = triangles.to_vec();
    order.sort_by_key(|tri| bones_of(tri).iter().next().copied().unwrap_or(u8::MAX));
    let mut groups = Vec::new();
    let (mut current, mut used): (Vec<[u32; 3]>, std::collections::BTreeSet<u8>) = (Vec::new(), Default::default());
    for tri in order {
        let bones = bones_of(&tri);
        if used.union(&bones).count() > SUBMESH_BONES {
            groups.push(std::mem::take(&mut current));
            used.clear();
        }
        current.push(tri);
        used.extend(bones);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// A mesh skinned on a skeleton: its texture coordinates, its palette, each vertex's influences.
type Skinned = (Vec<[f64; 2]>, Vec<usize>, Vec<Influence>);

/// A vertex's four weights (bytes summing to 255) and its four places in its submesh's palette.
type Influence = ([u8; 4], [u8; 4]);

/// How a model is made.
pub struct Options<'a> {
    /// Where its files sit in the game: the model names its textures by it.
    pub game_dir: String,
    /// The folder of the description: the files it names (images, glTF meshes) are found from it.
    pub dir: PathBuf,
    /// The image a `{"file": ...}` texture names (its path from `dir`), read by the caller.
    pub file: Option<&'a ReadImage>,
    /// Draws the textures and writes their BLP; without it the model alone is made.
    pub textures: bool,
    /// Refuses the model when the checks of `validate` find a fault.
    pub check: bool,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            game_dir: "Spells\\Vfx".into(),
            dir: PathBuf::from("."),
            file: None,
            textures: true,
            check: true,
        }
    }
}

/// A texture the model brings with it.
pub struct TextureFile {
    pub key: String,
    /// Its file name, beside the model.
    pub file_name: String,
    pub image: Image,
    pub blp: Vec<u8>,
}

/// What a description makes.
pub struct Forged {
    pub name: String,
    pub model: Model,
    pub skin: Skin,
    /// `<name>.m2`.
    pub m2: Vec<u8>,
    /// `<name>00.skin`.
    pub skin_file: Vec<u8>,
    pub textures: Vec<TextureFile>,
    pub notes: Vec<String>,
}

#[derive(Default)]
struct Lookups {
    texture: Vec<i16>,
    unit: Vec<i16>,
    weight: Vec<i16>,
    transform: Vec<i16>,
    bone: Vec<i16>,
}

struct Forge<'a> {
    spec: &'a Value,
    options: &'a Options<'a>,
    game_dir: String,
    name: String,
    time: Time,
    radius: f64,
    notes: Vec<String>,
    bones: Vec<Bone>,
    pivots: Vec<[f64; 3]>,
    bone_ids: Vec<(String, usize)>,
    textures: Vec<Texture>,
    tex_ids: Vec<(String, usize)>,
    files: Vec<TextureFile>,
    materials: Vec<Material>,
    colors: Vec<Color>,
    weights: Vec<Weight>,
    transforms: Vec<TextureTransform>,
    vertices: Vec<[u8; 48]>,
    skin: Skin,
    lookups: Lookups,
    load: f64,
}

impl<'a> Forge<'a> {
    fn bone(&self, name: &str) -> Result<usize> {
        match self.bone_ids.iter().find(|(n, _)| n == name) {
            Some((_, id)) => Ok(*id),
            None => fail(format!("bone {name} not described")),
        }
    }

    /// Whole turns in one sequence, so that the loop has no seam.
    fn turns(&mut self, per_second: &Value, what: &str) -> Result<i64> {
        let exact = num(per_second)? * self.time.duration as f64 / 1000.0;
        let mut t = py::round_int(exact);
        if t == 0 {
            t = if exact >= 0.0 { 1 } else { -1 };
        }
        if (t as f64 - exact).abs() > 1e-6 {
            self.notes.push(format!(
                "{what}: {} turn/s gives {exact:.2} turns in {} ms, {t} kept (no seam at the loop)",
                per_second.repr(),
                self.time.duration
            ));
        }
        Ok(t)
    }

    fn add_bone(&mut self, name: &str, b: &Value, parent: i16) -> Result<usize> {
        let d = self.time.duration;
        let pos = vec3(b.get("pos"))?;
        let mut rec = Bone {
            key_bone_id: -1,
            flags: 0,
            parent,
            submesh_id: 0,
            crc: 0,
            translation: Track::empty(),
            rotation: Track::empty(),
            scale: Track::empty(),
            pivot: f32x3(pos),
        };
        if b.get("billboard").is_some_and(Value::truthy) {
            rec.flags |= 0x8;
        }
        // translation: orbit (a circle about the pivot) and bob (up and down)
        let orbit = b.get("orbit").filter(|v| v.truthy());
        let bob = b.get("bob").filter(|v| v.truthy());
        if orbit.is_some() || bob.is_some() {
            let (o_r, o_turns, o_phase) = match orbit {
                Some(o) => (
                    float_or(o, "radius", 1.0)?,
                    self.turns(o.get("speed").unwrap_or(&Value::Float(0.5)), "orbit")?,
                    2.0 * PI * float_or(o, "phase", 0.0)?,
                ),
                None => (0.0, 0, 0.0),
            };
            let (b_h, b_turns) = match bob {
                Some(o) => (
                    float_or(o, "height", 0.25)?,
                    self.turns(o.get("speed").unwrap_or(&Value::Float(1.0)), "bob")?,
                ),
                None => (0.0, 0),
            };
            // linear keys: 32 per turn, so that a ribbon on the bone draws a circle, not a polygon
            let n = 16.max(32 * o_turns.abs().max(b_turns.abs()));
            let (mut ts, mut vs) = (Vec::new(), Vec::new());
            for k in 0..=n {
                let t = k as f64 / n as f64;
                let a = 2.0 * PI * o_turns as f64 * t + o_phase;
                ts.push(py::round_int(t * d as f64) as u32);
                vs.push(f32x3([
                    o_r * a.cos(),
                    o_r * a.sin(),
                    b_h * (2.0 * PI * b_turns as f64 * t).sin(),
                ]));
            }
            rec.translation = self.time.keyed(1, ts, vs);
        }
        // rotation: a fixed orientation, then a spin about the bone's Z; or keys by phase
        // ({"birth": [[t, [x, y, z]], ...], ...}: a petal opening, a wing folding)
        let spin = b.get("spin").filter(|v| v.truthy());
        let base = match b.get("rotation") {
            Some(rot @ Value::Map(_)) => {
                if spin.is_some() {
                    return fail(format!("bone {name}: rotation keyed by phase and spin together"));
                }
                rec.rotation = self.time.phased(rot, |r| Ok(cquat(euler(vec3(Some(r))?))))?;
                euler([0.0; 3])
            }
            rot => euler(vec3(rot)?),
        };
        if let Some(spin) = spin {
            let turns = self.turns(spin, "spin")?;
            let steps = 8.max(8 * turns.abs());
            let (mut ts, mut vs) = (Vec::new(), Vec::new());
            for k in 0..=steps {
                let t = k as f64 / steps as f64;
                ts.push(py::round_int(t * d as f64) as u32);
                vs.push(cquat(qmul(base, quat([0.0, 0.0, 1.0], 2.0 * PI * turns as f64 * t))));
            }
            rec.rotation = self.time.keyed(1, ts, vs);
        } else if b.has("rotation") && rec.rotation.timestamps.is_empty() {
            rec.rotation = self.time.fixed(cquat(base));
        }
        if let Some(translation) = b.get("translation") {
            // an offset from the rest place, in the parent's frame: a value, keys or keys by phase
            if orbit.is_some() || bob.is_some() {
                return fail(format!("bone {name}: translation keys and orbit/bob together"));
            }
            rec.translation = self.time.value_or_keys(translation, |v| vec3(Some(v)).map(f32x3))?;
        }
        if let Some(scale) = b.get("scale") {
            rec.scale = self.time.value_or_keys(scale, |s| match s {
                Value::List(_) => vec3(Some(s)).map(f32x3),
                _ => num(s).map(|v| [v as f32; 3]),
            })?;
        }
        if !rec.translation.timestamps.is_empty()
            || !rec.rotation.timestamps.is_empty()
            || !rec.scale.timestamps.is_empty()
        {
            rec.flags |= 0x200; // transformed
        }
        let id = self.bones.len();
        self.bone_ids.push((name.to_owned(), id));
        self.bones.push(rec);
        self.pivots.push(pos);
        Ok(id)
    }

    fn texture_spec(&self, key: &str) -> Result<&'a Value> {
        match self.spec.get("textures").and_then(|t| t.get(key)) {
            Some(tex) => Ok(tex),
            None => fail(format!("texture {key} not described")),
        }
    }

    /// Textures laid on the ground by a projected mesh: the client stretches their border pixels
    /// beyond the quad (clamped), so their edge must be transparent.
    fn projected(&self, key: &str) -> bool {
        self.spec
            .get("meshes")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .any(|m| {
                m.get("projected").is_some_and(Value::truthy) && m.get("texture").and_then(Value::as_str) == Some(key)
            })
    }

    fn texture(&mut self, key: &str) -> Result<u16> {
        if let Some((_, id)) = self.tex_ids.iter().find(|(k, _)| k == key) {
            return Ok(*id as u16);
        }
        let tex = self.texture_spec(key)?;
        let path = if let Some(game) = tex.get("game") {
            game.as_str()
                .ok_or_else(|| ForgeError(format!("texture {key}: a game path expected")))?
                .to_owned()
        } else {
            let file_name = format!("{}_{key}.blp", self.name).to_lowercase();
            if self.options.textures {
                let mut image = if let Some(file) = tex.get("file") {
                    let file = file
                        .as_str()
                        .ok_or_else(|| ForgeError(format!("texture {key}: a file name expected")))?;
                    match self.options.file {
                        Some(read) => {
                            read(&self.options.dir.join(file)).map_err(|e| ForgeError(format!("texture {key}: {e}")))?
                        }
                        None => return fail(format!("texture {key}: no reader for the file {file}")),
                    }
                } else {
                    let kind = str_or(tex, "kind", "")?;
                    let color = tex.get("color").and_then(Value::as_str);
                    texgen::make(kind, color, tex).map_err(|e| ForgeError(format!("texture {key}: {e}")))?
                };
                if self.projected(key) {
                    image = texgen::edge_fade(&image, 0.06);
                }
                let encoding = blp::Encoding::from_name(str_or(tex, "encoding", "auto")?)
                    .ok_or_else(|| ForgeError(format!("texture {key}: encoding (known: palette, dxt5, auto)")))?;
                let data = blp::encode(&image, encoding).map_err(|e| ForgeError(format!("texture {key}: {e}")))?;
                self.files.push(TextureFile {
                    key: key.to_owned(),
                    file_name: file_name.clone(),
                    image,
                    blp: data,
                });
            }
            format!("{}\\{file_name}", self.game_dir)
        };
        if !path.is_ascii() {
            return fail(format!("texture {key}: its path {path} is not ASCII"));
        }
        // repeat only the tileable noise: a clamped edge keeps a glow or a trail from bleeding into
        // the opposite border (a hard line on the quad's edge, or at the ribbon's tail)
        let wrap = match tex.get("wrap") {
            Some(w) => w.truthy(),
            None => tex.get("kind").and_then(Value::as_str) == Some("noise"),
        };
        let mut filename = path.into_bytes();
        filename.push(0);
        self.textures.push(Texture {
            kind: 0,
            flags: if wrap { 3 } else { 0 },
            filename,
        });
        let id = self.textures.len() - 1;
        self.tex_ids.push((key.to_owned(), id));
        Ok(id as u16)
    }

    fn cells_of(&self, key: &str) -> Result<(u16, u16)> {
        let tex = self.texture_spec(key)?;
        if let Some(cells) = tex.get("cells") {
            let cells = list(cells)?;
            if cells.len() != 2 {
                return fail(format!("texture {key}: cells [rows, cols] expected"));
            }
            return Ok((int(&cells[0])? as u16, int(&cells[1])? as u16));
        }
        if let Some(kind) = tex.get("kind").and_then(Value::as_str) {
            let (rows, cols) = texgen::cells(kind, tex).map_err(ForgeError)?;
            return Ok((rows as u16, cols as u16));
        }
        Ok((1, 1))
    }

    fn material(&mut self, blend: &str, flags: Option<&Value>) -> Result<u16> {
        let Some((_, mode)) = BLENDS.iter().find(|(k, _)| *k == blend) else {
            let known: Vec<&str> = BLENDS.iter().map(|(k, _)| *k).collect();
            return fail(format!("blend {blend} (known: {})", known.join(", ")));
        };
        let flags = match flags {
            Some(f) => flags_of(Some(f), &MATERIAL_FLAGS, "material")?,
            None => flags_of(
                Some(&Value::List(DEFAULT_MATERIAL.iter().map(|n| Value::from(*n)).collect())),
                &MATERIAL_FLAGS,
                "material",
            )?,
        };
        self.materials.push(Material {
            flags: flags as u16,
            blend: *mode,
        });
        Ok((self.materials.len() - 1) as u16)
    }

    fn emitter(&mut self, e: &Value, i: usize) -> Result<Particle> {
        let d = self.time.duration;
        let what = format!("emitter {i}");
        let key = e
            .get("texture")
            .and_then(Value::as_str)
            .ok_or_else(|| ForgeError(format!("{what}: no texture")))?;
        let tex = self.texture(key)?;
        let (rows, cols) = self.cells_of(key)?;
        let blend = str_or(e, "blend", "add")?;
        let Some((_, blend)) = PARTICLE_BLENDS.iter().find(|(k, _)| *k == blend) else {
            let known: Vec<&str> = PARTICLE_BLENDS.iter().map(|(k, _)| *k).collect();
            return fail(format!("{what}: blend {blend} (particles: {})", known.join(", ")));
        };
        let shape = str_or(e, "shape", "point")?;
        let (etype, length, width) = match shape {
            "sphere" => {
                let radius = float_or(e, "radius", 0.5)?;
                (
                    2,
                    radius,
                    if e.has("radius_min") {
                        float_or(e, "radius_min", 0.0)?
                    } else {
                        radius
                    },
                )
            }
            "plane" | "point" => {
                let size = match e.get("area") {
                    Some(area) => {
                        let area = list(area)?;
                        if area.len() < 2 {
                            return fail(format!("{what}: area [length, width] expected"));
                        }
                        [num(&area[0])?, num(&area[1])?]
                    }
                    None if shape == "point" => [0.0, 0.0],
                    None => [1.0, 1.0],
                };
                (1, size[0], size[1])
            }
            _ => return fail(format!("{what}: shape {shape} (known: point, plane, sphere)")),
        };

        // emission over the sequence: steady, a window, or bursts
        let life = float_or(e, "life", 1.0)?;
        let (rate_track, peak, alive) = if let Some(bursts) = e.get("bursts") {
            let mut keys: Vec<(i64, f64)> = Vec::new();
            let mut counts = Vec::new();
            for b in list(bursts)? {
                let at = float_or(b, "at", 0.0)?;
                let count = num(b
                    .get("count")
                    .ok_or_else(|| ForgeError(format!("{what}: a burst without its count")))?)?;
                // the window is widened until rate x life stays under the client's load
                let window = float_or(b, "window", 50.0)?.max((count * life * 1000.0 / PARTICLE_LOAD).ceil() + 1.0);
                let rate = count / (window / 1000.0);
                let t0 = py::round_int(at * d as f64);
                keys.push((t0, rate));
                keys.push(((t0 + window.trunc() as i64).min(d), 0.0));
                counts.push(count);
            }
            keys.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            });
            match keys.first() {
                None => return fail(format!("{what}: bursts without a burst")),
                Some((t, _)) if *t > 0 => keys.insert(0, (0, 0.0)),
                Some(_) => {}
            }
            let peak = keys.iter().map(|k| k.1).fold(f64::NEG_INFINITY, f64::max);
            let track = self.time.keyed(
                0,
                keys.iter().map(|k| k.0 as u32).collect(),
                keys.iter().map(|k| k.1 as f32).collect(),
            );
            (track, peak, Some(py::sum(counts)))
        } else {
            let rate = e.get("rate").cloned().unwrap_or(Value::Int(20));
            let track: Track<f64> = if e.has("until") {
                let until = float_or(e, "until", 0.0)?;
                self.time.keyed(
                    0,
                    vec![0, py::round_int(until * d as f64) as u32],
                    vec![num(&rate)?, 0.0],
                )
            } else {
                self.time.value_or_keys(&rate, num)?
            };
            let peak = track.values.iter().flatten().copied().fold(f64::NEG_INFINITY, f64::max);
            let track = Track {
                interpolation: track.interpolation,
                global_sequence: track.global_sequence,
                timestamps: track.timestamps,
                values: track
                    .values
                    .iter()
                    .map(|vs| vs.iter().map(|v| *v as f32).collect())
                    .collect(),
            };
            (track, peak, None)
        };
        let load = match alive {
            Some(alive) => alive,
            None => peak * life * (1.0 + float_or(e, "life_vary", 0.0)?),
        };
        if load > PARTICLE_LOAD {
            return fail(format!(
                "{what}: about {load:.0} particles alive (at most {PARTICLE_LOAD})"
            ));
        }
        self.load += load;

        let white = || Value::List(vec![Value::Int(0), Value::from("#ffffff")]);
        let colors = match e.get("colors") {
            None => Value::List(vec![white(), Value::List(vec![Value::Int(1), Value::from("#ffffff")])]),
            Some(Value::Str(c)) => Value::List(vec![
                Value::List(vec![Value::Int(0), Value::from(c.as_str())]),
                Value::List(vec![Value::Int(1), Value::from(c.as_str())]),
            ]),
            Some(c) => c.clone(),
        };
        let base_flags = match e.get("base_flags") {
            Some(Value::Str(text)) => {
                py::int_auto(text).ok_or_else(|| ForgeError(format!("{what}: base_flags {text}")))? as u32
            }
            Some(v) => int(v)? as u32,
            None => BASE_PARTICLE_FLAGS,
        };
        let float_track = |key: &str, default: f64| -> Result<Track<f32>> {
            self.time
                .value_or_keys(e.get(key).unwrap_or(&Value::Float(default)), |v| {
                    num(v).map(|x| x as f32)
                })
        };
        let fixed = |v: f64| self.time.fixed(v as f32);
        let default_alpha = Value::List(vec![
            Value::List(vec![Value::Int(0), Value::Int(0)]),
            Value::List(vec![Value::Float(0.15), Value::Int(1)]),
            Value::List(vec![Value::Int(1), Value::Int(0)]),
        ]);
        let mut rec = Particle {
            id: -1,
            flags: base_flags | flags_of(e.get("flags"), &PARTICLE_FLAGS, &what)?,
            position: f32x3(vec3(e.get("pos"))?),
            bone: self.bone(str_or(e, "bone", "root")?)? as u16,
            texture: tex,
            geometry_model: Vec::new(),
            recursion_model: Vec::new(),
            blend: *blend,
            emitter_type: etype,
            color_index: 0,
            particle_type: 0,
            head_or_tail: 0,
            tile_rotation: 0,
            rows,
            cols,
            emission_speed: float_track("speed", 1.0)?,
            speed_variation: fixed(float_or(e, "speed_vary", 0.0)?),
            vertical_range: fixed(float_or(e, "spread", 0.0)?.to_radians()),
            horizontal_range: fixed(float_or(e, "around", 360.0)?.to_radians()),
            gravity: float_track("gravity", 0.0)?,
            lifespan: fixed(life),
            lifespan_vary: float_or(e, "life_vary", 0.0)? as f32,
            emission_rate: rate_track,
            emission_rate_vary: float_or(e, "rate_vary", 0.0)? as f32,
            area_length: self.time.fixed(length as f32),
            area_width: self.time.fixed(width as f32),
            z_source: fixed(float_or(e, "z_source", 0.0)?),
            color_track: part_track(&colors, |c| hex_rgb(c).map(f32x3))?,
            alpha_track: part_track(e.get("alpha").unwrap_or(&default_alpha), |a| {
                Ok(py::round_int(num(a)?.clamp(0.0, 1.0) * FIX16 as f64) as i16)
            })?,
            scale_track: part_track(e.get("size").unwrap_or(&Value::Float(0.5)), |s| match s {
                Value::List(items) if items.len() >= 2 => Ok([num(&items[0])? as f32, num(&items[1])? as f32]),
                _ => num(s).map(|v| [v as f32; 2]),
            })?,
            scale_vary: [float_or(e, "size_vary", 0.0)? as f32; 2],
            head_cell: FBlock::default(),
            tail_cell: FBlock::default(),
            // tail length, twinkle and follow values: the most common ones of the client's emitters
            tail_length: float_or(e, "tail", 0.1)? as f32,
            twinkle_speed: 10.0,
            twinkle_percent: 1.0,
            twinkle_scale: [1.0, 1.0],
            burst_multiplier: 1.0,
            drag: float_or(e, "drag", 0.0)? as f32,
            base_spin: float_or(e, "base_spin", 0.0)?.to_radians() as f32,
            base_spin_vary: float_or(e, "base_spin_vary", 0.0)?.to_radians() as f32,
            spin: float_or(e, "spin", 0.0)?.to_radians() as f32,
            spin_vary: float_or(e, "spin_vary", 0.0)?.to_radians() as f32,
            tumble: [0; 24],
            wind: [0.0; 3],
            wind_time: 0.0,
            follow_speed1: 2.5,
            follow_scale1: 0.7,
            follow_speed2: 7.0,
            follow_scale2: 0.9,
            spline_points: Vec::new(),
            enabled: self.time.fixed(1),
        };
        if e.get("flipbook").is_some_and(Value::truthy) && rows * cols > 1 {
            rec.head_cell = FBlock {
                timestamps: vec![0, FIX16 as i16],
                values: vec![0, rows * cols - 1],
            };
        }
        Ok(rec)
    }

    fn ribbon(&mut self, r: &Value, i: usize) -> Result<Ribbon> {
        let key = r
            .get("texture")
            .and_then(Value::as_str)
            .ok_or_else(|| ForgeError(format!("ribbon {i}: no texture")))?;
        let tex = self.texture(key)?;
        let mat = self.material(str_or(r, "blend", "add")?, r.get("material_flags"))?;
        let life = float_or(r, "life", 0.5)?;
        let edges = float_or(r, "edges", 30.0)?;
        if edges * life > RIBBON_EDGES {
            return fail(format!("ribbon {i}: {:.0} edges alive (at most 64)", edges * life));
        }
        let float_track = |key: &str, default: f64| -> Result<Track<f32>> {
            self.time
                .value_or_keys(r.get(key).unwrap_or(&Value::Float(default)), |v| {
                    num(v).map(|x| x as f32)
                })
        };
        Ok(Ribbon {
            id: -1,
            bone: self.bone(str_or(r, "bone", "root")?)? as u32,
            position: f32x3(vec3(r.get("pos"))?),
            textures: vec![tex],
            materials: vec![mat],
            color: self
                .time
                .value_or_keys(r.get("color").unwrap_or(&Value::from("#ffffff")), |c| {
                    hex_rgb(c).map(|rgb| rgb.map(|x| (x / 255.0) as f32))
                })?,
            alpha: self
                .time
                .value_or_keys(r.get("alpha").unwrap_or(&Value::Float(1.0)), |a| {
                    Ok(py::round_int(num(a)? * FIX16 as f64) as i16)
                })?,
            height_above: float_track("above", 0.2)?,
            height_below: float_track("below", 0.2)?,
            edges_per_second: edges as f32,
            edge_lifetime: life as f32,
            gravity: float_or(r, "gravity", 0.0)? as f32,
            texture_rows: 1,
            texture_cols: 1,
            tex_slot: self.time.fixed(0),
            visibility: self.time.fixed(1),
            priority_plane: int_or(r, "priority", 0)? as i16,
            color_index: 0,
            transform_lookup: 0,
        })
    }

    /// The bones of a mesh (its palette, global ids) and, for each vertex, its 4 weights (bytes
    /// summing to 255) and 4 places in the palette. Alone, the mesh's own bone. With
    /// `"bend": [{"at": share of the length along +X, "rotation": value or keys by phase}, ...]`, one
    /// whole mesh bent by a chain of joints (a petal, a leaf, a tail): each joint a bone, child of
    /// the one before, the first the mesh's own; a vertex within `bend_soft` (0.08 of the length)
    /// of a joint is shared between the bones on either side, so that the surface bends without
    /// parting.
    fn bend_chain(
        &mut self,
        s: &Value,
        positions: &[[f64; 3]],
        bone: usize,
        pivot: [f64; 3],
        i: usize,
    ) -> Result<(Vec<usize>, Vec<Influence>)> {
        let mut bones = vec![bone];
        let mut joints = vec![0.0];
        let tube = s.get("shape").and_then(Value::as_str) == Some("tube");
        let most = positions.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let reach = if tube {
            float_or(s, "length", 1.0)?
        } else if most == 0.0 || positions.is_empty() {
            1.0
        } else {
            most
        };
        let joints_given = s.get("bend").map(list).transpose()?.unwrap_or_default();
        if joints_given.len() > BEND_JOINTS {
            return fail(format!("mesh {i}: {BEND_JOINTS} bend joints at most"));
        }
        for j in joints_given {
            let at = num(j
                .get("at")
                .ok_or_else(|| ForgeError(format!("mesh {i}: a bend joint without at")))?)?;
            if at <= joints[joints.len() - 1] {
                return fail(format!(
                    "mesh {i}: bend joints must go outward (at {})",
                    crate::json::float_repr(at)
                ));
            }
            let (y, z) = if tube {
                // on the axis
                (
                    float_or(s, "bend_y", 0.0)? * at * at,
                    float_or(s, "bend_z", 0.0)? * at * at,
                )
            } else {
                // the first point nearest the joint along the middle of the mesh
                let key = |p: &[f64; 3]| (p[0] - at * reach).abs() + p[1].abs();
                let near = positions
                    .iter()
                    .fold(None::<&[f64; 3]>, |best, p| match best {
                        Some(b) if key(b) <= key(p) => Some(b),
                        _ => Some(p),
                    })
                    .copied()
                    .unwrap_or([0.0; 3]);
                (0.0, near[2])
            };
            let name = match j.get("name") {
                Some(Value::Str(name)) => name.clone(),
                Some(other) => return fail(format!("mesh {i}: a bend joint's name {}", other.repr())),
                None => format!("_mesh{i}_j{}", bones.len()),
            };
            if self.bone(&name).is_ok() {
                return fail(format!("mesh {i}: bend joint {name}: a bone of that name exists"));
            }
            let mut joint = Value::Map(vec![
                (
                    "pos".into(),
                    Value::List(vec![
                        Value::Float(pivot[0] + at * reach),
                        Value::Float(pivot[1] + y),
                        Value::Float(pivot[2] + z),
                    ]),
                ),
                (
                    "rotation".into(),
                    j.get("rotation").cloned().unwrap_or(Value::List(vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(0),
                    ])),
                ),
            ]);
            if let Some(translation) = j.get("translation") {
                joint.set("translation", translation.clone());
            }
            let parent = bones[bones.len() - 1] as i16;
            bones.push(self.add_bone(&name, &joint, parent)?);
            joints.push(at);
        }
        let soft = float_or(s, "bend_soft", 0.08)?;
        let weights = |x: f64| -> Vec<(u8, u8)> {
            for (j, joint) in joints.iter().enumerate().skip(1) {
                if (x - joint).abs() < soft {
                    let f = (x - joint + soft) / (2.0 * soft);
                    let w = py::round_int(255.0 * f * f * (3.0 - 2.0 * f)) as u8;
                    return [((j - 1) as u8, 255 - w), (j as u8, w)]
                        .into_iter()
                        .filter(|b| b.1 != 0)
                        .collect();
                }
            }
            vec![(joints[1..].iter().filter(|a| **a <= x).count() as u8, 255)]
        };
        let influences = positions
            .iter()
            .map(|p| {
                let mut ws = if bones.len() > 1 {
                    weights(p[0] / reach)
                } else {
                    vec![(0, 255)]
                };
                ws.resize(4, (0, 0));
                (
                    std::array::from_fn(|k| ws[k].1),
                    std::array::from_fn(|k| if ws[k].1 != 0 { ws[k].0 } else { 0 }),
                )
            })
            .collect();
        Ok((bones, influences))
    }

    /// `"parts": [{"bone", "pos", "rotation", "scale", ...}, ...]`: the mesh's shape once a part,
    /// each copy on a bone of its own (child of the part's `bone`, or the mesh's), placed and
    /// animated by the part's keys: many small things in one batch (the leaves of a twig).
    fn parts(&mut self, s: &Value, i: usize, mesh_bone: usize) -> Result<(Shape, Vec<usize>, Vec<Influence>)> {
        let parts = list(s.get("parts").expect("tested"))?;
        if parts.is_empty() || parts.len() > SKIN_PALETTE as usize {
            return fail(format!("mesh {i}: 1 to {SKIN_PALETTE} parts"));
        }
        let one = mesh_shape(s, &self.options.dir)?;
        let mut shape = Shape {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            triangles: Vec::new(),
        };
        let (mut palette, mut influences) = (Vec::new(), Vec::new());
        for (k, part) in parts.iter().enumerate() {
            let spec = Value::Map(
                ["pos", "rotation", "spin", "scale", "translation", "orbit", "bob"]
                    .iter()
                    .filter_map(|key| part.get(key).map(|v| ((*key).to_owned(), v.clone())))
                    .collect(),
            );
            let parent = match part.get("bone") {
                Some(_) => self.bone(str_or(part, "bone", "root")?)?,
                None => mesh_bone,
            };
            let bone = self.add_bone(&format!("_mesh{i}_p{k}"), &spec, parent as i16)?;
            let [px, py, pz] = self.pivots[bone];
            let base = shape.positions.len() as u32;
            shape
                .positions
                .extend(one.positions.iter().map(|p| [p[0] + px, p[1] + py, p[2] + pz]));
            shape.normals.extend_from_slice(&one.normals);
            shape.uvs.extend_from_slice(&one.uvs);
            shape
                .triangles
                .extend(one.triangles.iter().map(|t| t.map(|v| v + base)));
            influences.extend(std::iter::repeat_n(
                ([255, 0, 0, 0], [k as u8, 0, 0, 0]),
                one.positions.len(),
            ));
            palette.push(bone);
        }
        Ok((shape, palette, influences))
    }

    fn mesh(&mut self, s: &Value, i: usize) -> Result<()> {
        let parent = self.bone(str_or(s, "bone", "root")?)?;
        let (mut shape, pivot, palette, influences) = if s.has("parts") {
            // copies of the shape in one mesh, each on a bone of its own: the leaves of a twig
            let (shape, palette, influences) = self.parts(s, i, parent)?;
            (shape, [0.0; 3], palette, influences)
        } else if s.has("skeleton") {
            // one whole mesh (modelled in place) skinned on a skeleton of bones: a tree's wood
            let mut shape = mesh_shape(s, &self.options.dir)?;
            let (uvs, palette, influences) = self.skeleton_skin(s, &shape.positions, &shape.uvs, i)?;
            shape.uvs = uvs;
            (shape, [0.0; 3], palette, influences)
        } else {
            // each mesh on a bone of its own (its scale, spin and place), child of the one named
            let bone_spec = Value::Map(
                ["pos", "rotation", "spin", "scale", "orbit", "bob", "billboard"]
                    .iter()
                    .filter_map(|k| s.get(k).map(|v| ((*k).to_owned(), v.clone())))
                    .collect(),
            );
            let bone = self.add_bone(&format!("_mesh{i}"), &bone_spec, parent as i16)?;
            let pivot = self.pivots[bone];
            let shape = mesh_shape(s, &self.options.dir)?;
            let (palette, influences) = self.bend_chain(s, &shape.positions, bone, pivot, i)?;
            (shape, pivot, palette, influences)
        };
        for p in &mut shape.positions {
            *p = [p[0] + pivot[0], p[1] + pivot[1], p[2] + pivot[2]];
        }
        let mut subs = Vec::new();
        for group in split(&shape.triangles, &influences, palette.len()) {
            subs.push(self.submesh(&shape, &group, &palette, &influences)?);
        }
        // its layers: one batch each on every submesh, drawn in order over the first
        let layers: Vec<Value> = match s.get("layers").filter(|l| l.truthy()) {
            Some(layers) => list(layers)?.to_vec(),
            None => vec![Value::Map(
                [
                    "texture",
                    "blend",
                    "material_flags",
                    "color",
                    "alpha",
                    "uv_scroll",
                    "priority",
                ]
                .iter()
                .filter_map(|k| s.get(k).map(|v| ((*k).to_owned(), v.clone())))
                .collect(),
            )],
        };
        for (li, ly) in layers.iter().enumerate() {
            self.layer(s, ly, i, li, &subs)?;
        }
        Ok(())
    }

    /// One layer of a mesh: its texture, material, colour and scroll; a batch on each submesh.
    fn layer(&mut self, s: &Value, ly: &Value, i: usize, li: usize, subs: &[u16]) -> Result<()> {
        let d = self.time.duration;
        let what = format!("mesh {i} layer {li}");
        let key = ly
            .get("texture")
            .and_then(Value::as_str)
            .ok_or_else(|| ForgeError(format!("{what}: no texture")))?;
        let tex = self.texture(key)?;
        let texture_combo = self.lookups.texture.len();
        self.lookups.texture.push(tex as i16);
        let env = ly.get("env").is_some_and(Value::truthy);
        let flags = match ly.get("material_flags") {
            Some(f) => f.clone(),
            None => Value::List(
                (if env { &ENV_MATERIAL[..] } else { &DEFAULT_MATERIAL[..] })
                    .iter()
                    .map(|n| Value::from(*n))
                    .collect(),
            ),
        };
        let mat = self.material(str_or(ly, "blend", "add")?, Some(&flags))?;
        // colour and alpha over the sequence
        let mut color = -1;
        if ly.has("color") || ly.has("alpha") {
            self.colors.push(Color {
                color: self
                    .time
                    .value_or_keys(ly.get("color").unwrap_or(&Value::from("#ffffff")), |c| {
                        hex_rgb(c).map(|rgb| rgb.map(|x| (x / 255.0) as f32))
                    })?,
                alpha: self
                    .time
                    .value_or_keys(ly.get("alpha").unwrap_or(&Value::Float(1.0)), |a| {
                        Ok(py::round_int(num(a)? * FIX16 as f64) as i16)
                    })?,
            });
            color = (self.colors.len() - 1) as i16;
        }
        let weight_combo = self.lookups.weight.len();
        self.weights.push(Weight {
            weight: self.time.fixed(FIX16 as i16),
        });
        self.lookups.weight.push((self.weights.len() - 1) as i16);
        let transform_combo = self.lookups.transform.len();
        if let Some(scroll) = ly.get("uv_scroll") {
            let scroll = list(scroll)?;
            if scroll.len() < 2 {
                return fail(format!("{what}: uv_scroll [u, v] expected"));
            }
            let du = num(&scroll[0])? * d as f64 / 1000.0;
            let dv = num(&scroll[1])? * d as f64 / 1000.0;
            let (mut ru, mut rv) = (py::round(du), py::round(dv));
            if ru == 0.0 && rv == 0.0 {
                let unit = |x: f64| {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                };
                (ru, rv) = (unit(du), unit(dv));
            }
            if (ru - du).abs() > 1e-6 || (rv - dv).abs() > 1e-6 {
                self.notes.push(format!(
                    "{what}: UV scroll {du:.2},{dv:.2} per loop rounded to {},{} (no seam)",
                    ru as i64, rv as i64
                ));
            }
            self.transforms.push(TextureTransform {
                translation: self
                    .time
                    .keyed(1, vec![0, d as u32], vec![[0.0; 3], [ru as f32, rv as f32, 0.0]]),
                rotation: Track::empty(),
                scaling: Track::empty(),
            });
            self.lookups.transform.push((self.transforms.len() - 1) as i16);
        } else {
            self.lookups.transform.push(-1);
        }
        // an environment map, as the client's own spell models lay one over a mesh: coordinates
        // from the normal as the camera sees it (a sphere map), a texture unit of -1, batch flag 0x10
        let coord = if env {
            if !self.lookups.unit.contains(&-1) {
                self.lookups.unit.push(-1);
            }
            self.lookups.unit.iter().position(|u| *u == -1).expect("pushed") as u16
        } else {
            0
        };
        // 0x04: projected texture, the client lays the quad on the terrain under it (Blizzard's
        // ground circles: consecration, death and decay...); 0x18 as on those batches
        let projected = s.get("projected").is_some_and(Value::truthy);
        let flags = if projected {
            PROJECTED_BATCH
        } else if env {
            0x10
        } else {
            0
        };
        let priority = match ly.get("priority") {
            Some(p) => int(p)?,
            None => int_or(s, "priority", 0)?,
        } as i8;
        for &sub in subs {
            self.skin.batches.push(Batch {
                flags,
                priority,
                shader: 0,
                submesh: sub,
                geoset: sub,
                color,
                material: mat,
                layer: li as u16,
                texture_count: 1,
                texture_combo: texture_combo as u16,
                coord_combo: coord,
                weight_combo: weight_combo as u16,
                transform_combo: transform_combo as u16,
            });
        }
        Ok(())
    }

    /// Writes the vertices of some triangles (those they use, again when another submesh uses them
    /// too: the same place, normal and weights) and their submesh; its index.
    fn submesh(
        &mut self,
        shape: &Shape,
        tris: &[[u32; 3]],
        palette: &[usize],
        influences: &[Influence],
    ) -> Result<u16> {
        let mut used: Vec<u32> = tris.iter().flatten().copied().collect();
        used.sort_unstable();
        used.dedup();
        let mut bones: Vec<u8> = used
            .iter()
            .flat_map(|v| {
                let (w, b) = influences[*v as usize];
                (0..4).filter(move |k| w[*k] != 0).map(move |k| b[k])
            })
            .collect();
        bones.sort_unstable();
        bones.dedup();
        if bones.is_empty() {
            bones.push(0);
        }
        let local = |b: u8| bones.iter().position(|x| *x == b).expect("among them") as u8;
        let (v0, sv0, si0) = (self.vertices.len(), self.skin.vertices.len(), self.skin.indices.len());
        if v0 + used.len() > 65536 || sv0 + used.len() > 65536 {
            return fail("too many vertices: a skin addresses 65,535 at most");
        }
        let mut place = std::collections::HashMap::new();
        for (k, v) in used.iter().enumerate() {
            let (w, b) = influences[*v as usize];
            let p = shape.positions[*v as usize];
            let n = shape.normals[*v as usize];
            let uv = shape.uvs[*v as usize];
            let mut raw = Vec::with_capacity(48);
            for x in p {
                raw.extend_from_slice(&(x as f32).to_le_bytes());
            }
            raw.extend_from_slice(&w);
            for k in 0..4 {
                let id = if w[k] != 0 { palette[b[k] as usize] } else { 0 };
                raw.push(u8::try_from(id).map_err(|_| ForgeError(format!("bone {id} past 255")))?);
            }
            for x in [n[0], n[1], n[2], uv[0], uv[1], 0.0, 0.0] {
                raw.extend_from_slice(&(x as f32).to_le_bytes());
            }
            self.vertices.push(raw.try_into().expect("48 bytes"));
            self.skin.vertices.push((v0 + k) as u16);
            self.skin
                .bones
                .push(std::array::from_fn(|k| if w[k] != 0 { local(b[k]) } else { 0 }));
            place.insert(*v, (sv0 + k) as u16);
        }
        for tri in tris {
            self.skin.indices.extend(tri.iter().map(|v| place[v]));
        }
        if self.skin.indices.len() > 65535 {
            return fail("too many triangles: a skin addresses 65,535 indices at most");
        }
        let bone_combo = self.lookups.bone.len();
        self.lookups
            .bone
            .extend(bones.iter().map(|b| palette[*b as usize] as i16));
        let n = used.len() as f64;
        let mean = |axis: usize| py::sum(used.iter().map(|v| shape.positions[*v as usize][axis])) / n;
        let center = [mean(0), mean(1), mean(2)];
        let radius = used
            .iter()
            .map(|v| py::dist(&center, &shape.positions[*v as usize]))
            .fold(f64::NEG_INFINITY, f64::max);
        let most = used
            .iter()
            .map(|v| influences[*v as usize].0.iter().filter(|w| **w != 0).count())
            .max()
            .unwrap_or(0);
        self.skin.submeshes.push(Submesh {
            id: 0,
            level: 0,
            vertex_start: sv0 as u16,
            vertex_count: used.len() as u16,
            index_start: si0 as u16,
            index_count: (tris.len() * 3) as u16,
            bone_count: bones.len() as u16,
            bone_combo: bone_combo as u16,
            bone_influences: most as u16,
            center_bone: palette[bones[0] as usize] as u16,
            center: f32x3(center),
            sort_center: f32x3(center),
            sort_radius: radius as f32,
        });
        Ok((self.skin.submeshes.len() - 1) as u16)
    }

    /// `"skeleton": [[bone a, bone b(, [radius a, radius b])], ...]`: segments between bones'
    /// pivots (the mesh modelled around them, in place). Each vertex goes to its nearest segment
    /// (nearest for its thickness, when given), shared between its two bones as far along as it
    /// lies. With `"skeleton_uv": [around, along]`: coordinates around each segment and along the
    /// skeleton (repeats a turn and a yard), for a bark that runs along every branch.
    fn skeleton_skin(&mut self, s: &Value, positions: &[[f64; 3]], uvs: &[[f64; 2]], i: usize) -> Result<Skinned> {
        let segs = list(s.get("skeleton").expect("tested"))?;
        let mut parsed = Vec::with_capacity(segs.len());
        for g in segs {
            let g = list(g)?;
            let name = |k: usize| -> Result<String> {
                g.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| {
                    ForgeError(format!(
                        "mesh {i}: a skeleton segment [bone a, bone b(, radii)] expected"
                    ))
                })
            };
            let radii = match g.get(2) {
                Some(r) => {
                    let r = list(r)?;
                    [num(&r[0])?, num(&r[1])?]
                }
                None => [1.0, 1.0],
            };
            parsed.push((name(0)?, name(1)?, radii));
        }
        let mut names: Vec<String> = parsed.iter().flat_map(|(a, b, _)| [a.clone(), b.clone()]).collect();
        names.sort();
        names.dedup();
        let mut ids = Vec::with_capacity(names.len());
        for n in &names {
            ids.push((self.bone(n)?, n.clone()));
        }
        ids.sort_by_key(|(id, _)| *id);
        let palette: Vec<usize> = ids.iter().map(|(id, _)| *id).collect();
        let place = |n: &str| ids.iter().position(|(_, m)| m == n).expect("named") as u8;
        let pivot = |n: &str| -> Result<[f64; 3]> { Ok(self.pivots[self.bone(n)?]) };
        let mut a_of = Vec::new();
        let mut ab_of = Vec::new();
        let mut ll_of = Vec::new();
        for (a, b, _) in &parsed {
            let (pa, pb) = (pivot(a)?, pivot(b)?);
            let ab = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            a_of.push(pa);
            ab_of.push(ab);
            ll_of.push((ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2]).max(1e-9));
        }
        // the length along the skeleton at each bone, for the bark running along
        let mut arc: Vec<(String, f64)> = Vec::new();
        for ((a, b, _), (pa, ab)) in parsed.iter().zip(a_of.iter().zip(&ab_of)) {
            let _ = pa;
            if !arc.iter().any(|(n, _)| n == a) {
                arc.push((a.clone(), 0.0));
            }
            let from = arc.iter().find(|(n, _)| n == a).map(|(_, v)| *v).expect("set");
            let length = (ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2]).sqrt();
            match arc.iter_mut().find(|(n, _)| n == b) {
                Some((_, v)) => *v = from + length,
                None => arc.push((b.clone(), from + length)),
            }
        }
        let tile = match s.get("skeleton_uv") {
            Some(t) => {
                let t = list(t)?;
                Some([num(&t[0])?, num(&t[1])?])
            }
            None => None,
        };
        let mut influences = Vec::with_capacity(positions.len());
        let mut out_uv = Vec::with_capacity(positions.len());
        for p in positions {
            // the nearest segment, for its thickness
            let mut best = (f64::INFINITY, 0usize, 0.0f64);
            for (k, (pa, ab)) in a_of.iter().zip(&ab_of).enumerate() {
                let q = [p[0] - pa[0], p[1] - pa[1], p[2] - pa[2]];
                let t = ((q[0] * ab[0] + q[1] * ab[1] + q[2] * ab[2]) / ll_of[k]).clamp(0.0, 1.0);
                let near = [pa[0] + ab[0] * t, pa[1] + ab[1] * t, pa[2] + ab[2] * t];
                let [r0, r1] = parsed[k].2;
                let r = r0 + (r1 - r0) * t;
                let dd = [p[0] - near[0], p[1] - near[1], p[2] - near[2]];
                let d = (dd[0] * dd[0] + dd[1] * dd[1] + dd[2] * dd[2]).sqrt() / r.max(1e-4);
                if d < best.0 {
                    best = (d, k, t);
                }
            }
            let (_, k, tv) = best;
            let wb = py::round_int(255.0 * tv) as u8;
            let (ia, ib) = (place(&parsed[k].0), place(&parsed[k].1));
            influences.push(match wb {
                0 => ([255, 0, 0, 0], [ia, 0, 0, 0]),
                255 => ([255, 0, 0, 0], [ib, 0, 0, 0]),
                _ => ([255 - wb, wb, 0, 0], [ia, ib, 0, 0]),
            });
            if let Some(tile) = tile {
                let ab = ab_of[k];
                let n = ll_of[k].sqrt();
                let dd = [ab[0] / n, ab[1] / n, ab[2] / n];
                let reference = if dd[2].abs() < 0.9 {
                    [0.0, 0.0, 1.0]
                } else {
                    [1.0, 0.0, 0.0]
                };
                let cross = |a: [f64; 3], b: [f64; 3]| {
                    [
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ]
                };
                let mut e1 = cross(reference, dd);
                let norm = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
                let norm = if norm == 0.0 { 1.0 } else { norm };
                e1 = e1.map(|v| v / norm);
                let e2 = cross(dd, e1);
                let pa = a_of[k];
                let q = [p[0] - pa[0], p[1] - pa[1], p[2] - pa[2]];
                let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                let angle = dot(q, e2).atan2(dot(q, e1));
                let along = arc.iter().find(|(m, _)| *m == parsed[k].0).map_or(0.0, |(_, v)| *v);
                out_uv.push([angle / (2.0 * PI) * tile[0], (along + tv * n) * tile[1]]);
            }
        }
        Ok((if tile.is_some() { out_uv } else { uvs.to_vec() }, palette, influences))
    }
    /// Notes billboards and ribbons that dip into a sloped ground: only a projected mesh follows
    /// the ground, which rises about tan(SLOPE_DEG) times the distance from the centre.
    fn ground_check(&mut self) -> Result<()> {
        let spec = self.spec;
        // bone: horizontal radius, bob height
        let mut reach: Vec<(String, (f64, f64))> = vec![("root".into(), (0.0, 0.0))];
        for (name, b) in spec.get("bones").and_then(Value::as_map).unwrap_or_default() {
            let orbit = b.get("orbit").filter(|v| v.truthy());
            let bob = b.get("bob").filter(|v| v.truthy());
            let r = match orbit {
                Some(o) => float_or(o, "radius", 1.0)?,
                None => 0.0,
            };
            let h = match bob {
                Some(o) => float_or(o, "height", 0.25)?,
                None => 0.0,
            };
            match reach.iter_mut().find(|(n, _)| n == name) {
                Some((_, slot)) => *slot = (r, h),
                None => reach.push((name.clone(), (r, h))),
            }
        }
        let reach_of = |bone: &str| reach.iter().find(|(n, _)| n == bone).map_or((0.0, 0.0), |(_, v)| *v);
        let mut notes = Vec::new();
        let low = |notes: &mut Vec<String>, z: f64, dist: f64, what: &str| {
            if z - dist * slope() < 0.05 {
                notes.push(format!(
                    "{what}: lowest point {z:.2} at {dist:.2} from the centre, cuts into a slope of {SLOPE_DEG:.0} degrees"
                ));
            }
        };
        for (i, e) in spec
            .get("emitters")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let rising = match e.get("alpha") {
                Some(Value::List(keys)) if !keys.is_empty() => {
                    let first = keys[0]
                        .as_list()
                        .and_then(|k| k.get(1))
                        .ok_or_else(|| ForgeError(format!("emitter {i}: alpha keys [t, value] expected")))?;
                    num(first)? == 0.0 && float_or(e, "speed", 0.0)? > 0.0
                }
                _ => false,
            };
            if float_or(e, "gravity", 0.0)? > 0.0 && float_or(e, "spread", 0.0)? > 90.0 {
                notes.push(format!(
                    "emitter {i}: particles thrown downwards with gravity, they reach the ground"
                ));
            }
            if rising {
                // appears while rising: leaves the ground unseen
                continue;
            }
            let size = e.get("size").cloned().unwrap_or(Value::Int(0));
            let values: Vec<Value> = if size.is_keys() {
                list(&size)?
                    .iter()
                    .map(|k| k.as_list().and_then(|k| k.get(1)).cloned().unwrap_or(Value::Int(0)))
                    .collect()
            } else {
                vec![size]
            };
            let mut half = f64::NEG_INFINITY;
            for v in &values {
                let most = match v {
                    Value::List(items) => items
                        .iter()
                        .map(num)
                        .collect::<Result<Vec<f64>>>()?
                        .into_iter()
                        .fold(f64::NEG_INFINITY, f64::max),
                    _ => num(v)?,
                };
                half = half.max(most);
            }
            let half = half / 2.0;
            let (radius, bob) = reach_of(str_or(e, "bone", "root")?);
            let z = vec3(e.get("pos"))?[2];
            low(
                &mut notes,
                z - bob - half,
                radius + half,
                &format!("emitter {i} (billboard)"),
            );
        }
        for (i, r) in spec
            .get("ribbons")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let (radius, bob) = reach_of(str_or(r, "bone", "root")?);
            let z = vec3(r.get("pos"))?[2];
            low(
                &mut notes,
                z - bob - float_or(r, "below", 0.2)?,
                radius,
                &format!("ribbon {i}"),
            );
        }
        self.notes.extend(notes);
        Ok(())
    }

    /// A point light on a bone: colour, intensity (a value, keys or keys by phase), and the
    /// distance over which it fades (start, end).
    fn light(&self, l: &Value) -> Result<m2::Light> {
        let t = &self.time;
        let col = |c: &Value| hex_rgb(c).map(|rgb| rgb.map(|x| (x / 255.0) as f32));
        let float = |key: &str, default: f64| -> Result<Track<f32>> {
            t.value_or_keys(l.get(key).unwrap_or(&Value::Float(default)), |v| {
                num(v).map(|x| x as f32)
            })
        };
        Ok(m2::Light {
            kind: 1,
            bone: self.bone(str_or(l, "bone", "root")?)? as i16,
            position: f32x3(vec3(l.get("pos"))?),
            ambient_color: t.value_or_keys(l.get("ambient").unwrap_or(&Value::from("#000000")), col)?,
            ambient_intensity: float("ambient_intensity", 0.0)?,
            diffuse_color: t.value_or_keys(l.get("color").unwrap_or(&Value::from("#ffffff")), col)?,
            diffuse_intensity: float("intensity", 1.0)?,
            attenuation_start: float("start", 0.5)?,
            attenuation_end: float("end", 4.0)?,
            visibility: t.value_or_keys(l.get("visible").unwrap_or(&Value::Int(1)), |v| int(v).map(|x| x as u8))?,
        })
    }

    fn build(mut self) -> Result<Forged> {
        self.load = 0.0;
        self.add_bone("root", &Value::Map(Vec::new()), -1)?;
        let bones = self.spec.get("bones").and_then(Value::as_map).unwrap_or_default();
        if bones.iter().any(|(name, _)| name == "root") {
            return fail("bone \"root\" is made by forge");
        }
        // bones in an order where each parent comes first
        let mut pending: Vec<&(String, Value)> = bones.iter().collect();
        while !pending.is_empty() {
            let mut done = false;
            let mut index = 0;
            while index < pending.len() {
                let (name, b) = pending[index];
                let parent = str_or(b, "parent", "root")?;
                if let Ok(parent) = self.bone(parent) {
                    self.add_bone(name, b, parent as i16)?;
                    pending.remove(index);
                    done = true;
                } else {
                    index += 1;
                }
            }
            if !done {
                let names: Vec<&str> = pending.iter().map(|(n, _)| n.as_str()).collect();
                return fail(format!("bones without a parent described: {}", names.join(", ")));
            }
        }
        let spec = self.spec;
        // meshes first: their bend joints may carry a name that an emitter or a ribbon hangs on
        for (i, s) in spec
            .get("meshes")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            self.mesh(s, i)?;
        }
        if self.bones.len() > 256 {
            // a vertex names its bones in bytes: past 256, a mesh could not reach them
            return fail(format!(
                "{} bones (at most 256: a vertex names its bones in bytes)",
                self.bones.len()
            ));
        }
        let emitters = spec.get("emitters").and_then(Value::as_list).unwrap_or_default();
        let particles = emitters
            .iter()
            .enumerate()
            .map(|(i, e)| self.emitter(e, i))
            .collect::<Result<Vec<_>>>()?;
        if self.load > MODEL_LOAD {
            return fail(format!(
                "about {:.0} particles alive in the model (at most {MODEL_LOAD})",
                self.load
            ));
        }
        let ribbons = spec
            .get("ribbons")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, r)| self.ribbon(r, i))
            .collect::<Result<Vec<_>>>()?;
        if particles.len() > EMITTERS {
            return fail(format!("{} emitters (at most {EMITTERS})", particles.len()));
        }
        self.ground_check()?;

        let d = self.time.duration;
        let single = self.time.phases.len() == 1;
        // as the client's state models: no blend between phases, each its own alias, no lookups
        let sequences = self
            .time
            .phases
            .iter()
            .enumerate()
            .map(|(i, (phase, duration))| {
                let (_, id, flags) = PHASES
                    .iter()
                    .find(|(p, _, _)| p == phase)
                    .copied()
                    .unwrap_or(("stand", 0, 0x20));
                Sequence {
                    id: if single { 0 } else { id },
                    variation: 0,
                    duration: *duration as u32,
                    move_speed: 0.0,
                    flags: if single { m2::EMBEDDED } else { flags },
                    frequency: 32767,
                    padding: 0,
                    replay_min: 0,
                    replay_max: 0,
                    blend_time: if single { 150 } else { 0 },
                    bounds: bounds(self.radius).try_into().expect("28 bytes"),
                    variation_next: -1,
                    alias_next: if single { 0 } else { i as u16 },
                }
            })
            .collect();
        let lights = spec
            .get("lights")
            .and_then(Value::as_list)
            .unwrap_or_default()
            .iter()
            .map(|l| self.light(l))
            .collect::<Result<Vec<_>>>()?;
        if !self.name.is_ascii() {
            return fail(format!("name {}: not ASCII", self.name));
        }
        let mut name = self.name.clone().into_bytes();
        name.push(0);
        let mut boxes = bounds(self.radius)[..24].to_vec();
        boxes.extend_from_slice(&(self.radius as f32).to_le_bytes());
        boxes.resize(56, 0);
        let has_batches = !self.skin.batches.is_empty();
        let lk = std::mem::take(&mut self.lookups);
        let model = Model {
            version: 264,
            name,
            flags: 0,
            num_skin: 1,
            boxes,
            global_loops: if self.time.gs == 0 { vec![d as u32] } else { Vec::new() },
            sequences,
            lights,
            bones: std::mem::take(&mut self.bones),
            key_bone_lookup: vec![-1],
            vertices: std::mem::take(&mut self.vertices),
            colors: std::mem::take(&mut self.colors),
            textures: std::mem::take(&mut self.textures),
            texture_weights: std::mem::take(&mut self.weights),
            texture_transforms: std::mem::take(&mut self.transforms),
            replaceable_texture_lookup: vec![-1],
            materials: std::mem::take(&mut self.materials),
            bone_lookup: lk.bone,
            texture_lookup: lk.texture,
            texture_unit_lookup: if has_batches { lk.unit.clone() } else { Vec::new() },
            transparency_lookup: lk.weight,
            texture_transform_lookup: lk.transform,
            ribbons,
            particles,
            ..Model::default()
        };
        let _ = lk.unit;
        let mut skin = std::mem::take(&mut self.skin);
        // the smallest palette of the client's own that holds the submesh with most bones
        let most = skin
            .submeshes
            .iter()
            .map(|s| u32::from(s.bone_count))
            .max()
            .unwrap_or(0);
        skin.bone_count_max = BONE_PALETTES.iter().copied().find(|v| *v >= most).unwrap_or(256);
        let m2 = m2::write(&model);
        let skin_file = m2::write_skin(&skin);
        Ok(Forged {
            name: self.name,
            model,
            skin,
            m2,
            skin_file,
            textures: self.files,
            notes: self.notes,
        })
    }
}

/// The phases of the model from its `"sequences": {"birth": ms, "hold": ms, "decay": ms}`; a
/// looping Stand alone, as long as `"duration"`, without them.
fn timing(spec: &Value) -> Result<Time> {
    let duration = int_or(spec, "duration", 2000)?;
    let seqs = spec.get("sequences").filter(|s| s.truthy());
    let Some(seqs) = seqs else {
        return Ok(Time {
            duration,
            gs: -1,
            phases: vec![("stand".into(), duration)],
        });
    };
    let entries = seqs
        .as_map()
        .ok_or_else(|| ForgeError("sequences: {\"birth\": ms, \"hold\": ms, \"decay\": ms} expected".into()))?;
    let unknown = entries.iter().any(|(k, _)| !PHASES.iter().any(|(p, _, _)| p == k));
    if unknown || !seqs.has("hold") {
        let got: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        return fail(format!(
            "sequences: birth, hold, decay, with hold (got {})",
            got.join(", ")
        ));
    }
    if int(seqs.get("hold").expect("hold"))? != duration {
        return fail("sequences: hold must last \"duration\" (the loop of the looping tracks)");
    }
    let mut phases = Vec::new();
    for (phase, _, _) in PHASES {
        if let Some(ms) = seqs.get(phase) {
            phases.push((phase.to_owned(), int(ms)?));
        }
    }
    Ok(Time {
        duration,
        gs: if phases.len() == 1 { -1 } else { 0 },
        phases,
    })
}

/// The model a description makes; refused when the checks of `validate` find a fault (unless
/// `options.check` is off).
pub fn forge(spec: &Value, options: &Options) -> Result<Forged> {
    let name = match spec.get("name") {
        Some(Value::Str(name)) => name.clone(),
        Some(other) => return fail(format!("name: a text expected, {} found", other.repr())),
        None => return fail("the description has no name"),
    };
    let forge = Forge {
        spec,
        options,
        game_dir: options.game_dir.trim_end_matches('\\').to_owned(),
        name,
        time: timing(spec)?,
        radius: float_or(spec, "radius", 3.0)?,
        notes: Vec::new(),
        bones: Vec::new(),
        pivots: Vec::new(),
        bone_ids: Vec::new(),
        textures: Vec::new(),
        tex_ids: Vec::new(),
        files: Vec::new(),
        materials: Vec::new(),
        colors: Vec::new(),
        weights: Vec::new(),
        transforms: Vec::new(),
        vertices: Vec::new(),
        skin: Skin {
            bone_count_max: SKIN_PALETTE,
            ..Skin::default()
        },
        lookups: Lookups {
            unit: vec![0],
            ..Lookups::default()
        },
        load: 0.0,
    };
    let forged = forge.build()?;
    if options.check {
        let faults = validate::validate(&forged.model, Some(&forged.skin));
        if !faults.is_empty() {
            return fail(format!("validate refuses the model:\n  {}", faults.join("\n  ")));
        }
    }
    Ok(forged)
}
