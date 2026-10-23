//! The particles and ribbons of a model over time, made the same each time from the same start:
//! a preview can be scrubbed back and forth (it plays again from the start, in fixed steps).
//!
//! What the client does is followed where it is known (wowdev's notes on the M2 emitters, WoW
//! Model Viewer); where it is not yet measured in the game, the choice is said in a comment, to be
//! checked against the client:
//! - a particle's size is its quad's whole side (as forge's ground guard and preview.py read it);
//! - plane emitters spread their particles over length (X) and width (Y) about their place;
//!   sphere emitters between the width (inner radius) and the length (outer radius);
//! - the direction leans from up (Z) by a random angle within the vertical range, turned about Z
//!   within the horizontal range;
//! - drag slows a particle by that fraction of its speed each second.

use crate::anim::{self, Clock, sample, sample_life};
use crate::m2::{Model, Particle as Emitter, Ribbon};
use crate::texgen::Rng;
use glam::{Mat4, Vec3};

/// The emitter flag keeping its particles on it as it moves ("do not trail").
pub const NO_TRAIL: u32 = 0x10;
/// The emitter flag starting each particle on a random cell of its flipbook.
pub const RANDOM_CELL: u32 = 0x10000;
/// The emitter flag drawing its quads flat (not facing the camera).
pub const NO_BILLBOARD: u32 = 0x1000;

/// The step of the simulation, in seconds.
pub const STEP: f64 = 1.0 / 60.0;

#[derive(Clone, Debug)]
pub struct Particle {
    /// In the model's space; on the emitter's bone for an emitter that does not trail.
    pub pos: Vec3,
    pub vel: Vec3,
    pub age: f32,
    pub life: f32,
    pub spin: f32,
    pub spin_rate: f32,
    pub scale: f32,
    pub cell: u16,
}

/// A particle as it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub emitter: usize,
    /// Its centre, in the model's space.
    pub pos: Vec3,
    /// Half its sides.
    pub size: [f32; 2],
    pub rotation: f32,
    /// Red, green, blue (0 to 1), alpha (0 to 1).
    pub color: [f32; 4],
    /// The cell of its texture.
    pub cell: u16,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub pos: Vec3,
    /// The bone's up at that moment (the ribbon's height across).
    pub up: Vec3,
    pub age: f32,
}

#[derive(Clone, Debug)]
struct EmitterState {
    owed: f64,
    particles: Vec<Particle>,
}

#[derive(Clone, Debug)]
struct RibbonState {
    owed: f64,
    edges: Vec<Edge>,
}

/// A ribbon as it is drawn: its edges, head first, each a top and a bottom point and its texture
/// coordinate along.
#[derive(Clone, Debug, PartialEq)]
pub struct Strip {
    pub ribbon: usize,
    pub points: Vec<(Vec3, Vec3, f32)>,
    pub color: [f32; 4],
}

/// The particles and ribbons of a model at a time.
#[derive(Clone, Debug)]
pub struct Simulation {
    seed: i64,
    rng: Rng,
    /// Seconds simulated.
    pub time: f64,
    emitters: Vec<EmitterState>,
    ribbons: Vec<RibbonState>,
}

fn emitter_matrix(pose: &[Mat4], e: &Emitter) -> Mat4 {
    let bone = pose.get(e.bone as usize).copied().unwrap_or(Mat4::IDENTITY);
    bone * Mat4::from_translation(Vec3::from(e.position))
}

impl Simulation {
    pub fn new(model: &Model, seed: i64) -> Self {
        Self {
            seed,
            rng: Rng::new(seed),
            time: 0.0,
            emitters: model
                .particles
                .iter()
                .map(|_| EmitterState {
                    owed: 0.0,
                    particles: Vec::new(),
                })
                .collect(),
            ribbons: model
                .ribbons
                .iter()
                .map(|_| RibbonState {
                    owed: 0.0,
                    edges: Vec::new(),
                })
                .collect(),
        }
    }

    /// Brought to `time` (seconds) of the playback: forward step by step, or again from the start
    /// when the time is before it.
    pub fn seek(&mut self, model: &Model, playback: &anim::Playback, time: f64) {
        if time < self.time || self.emitters.len() != model.particles.len() || self.ribbons.len() != model.ribbons.len()
        {
            *self = Self::new(model, self.seed);
        }
        while self.time + STEP <= time + 1e-9 {
            let clock = playback.clock((self.time + STEP) * 1000.0);
            let pose = anim::pose(model, &clock, None);
            self.step(model, &clock, &pose, STEP as f32);
        }
    }

    fn spawn(rng: &mut Rng, e: &Emitter, clock: &Clock, loops: &[u32], m: Mat4) -> Option<Particle> {
        let get = |t: &crate::m2::Track<f32>, default: f32| sample(t, clock, loops).unwrap_or(default);
        let length = get(&e.area_length, 0.0);
        let width = get(&e.area_width, 0.0);
        let vertical = get(&e.vertical_range, 0.0);
        let horizontal = get(&e.horizontal_range, std::f32::consts::TAU);
        let tilt = rng.uniform(-f64::from(vertical), f64::from(vertical)) as f32;
        let turn = rng.uniform(-f64::from(horizontal) / 2.0, f64::from(horizontal) / 2.0) as f32;
        let mut dir = Vec3::new(tilt.sin() * turn.cos(), tilt.sin() * turn.sin(), tilt.cos());
        let local = match e.emitter_type {
            2 => {
                // a sphere: between the inner (width) and the outer (length) radius
                let r = rng.uniform(f64::from(width.min(length)), f64::from(length.max(width))) as f32;
                let z = rng.uniform(-1.0, 1.0) as f32;
                let a = rng.uniform(0.0, std::f64::consts::TAU) as f32;
                let s = (1.0 - z * z).max(0.0).sqrt();
                let out = Vec3::new(s * a.cos(), s * a.sin(), z);
                if vertical >= std::f32::consts::PI - 1e-3 {
                    dir = out;
                }
                out * r
            }
            _ => Vec3::new(
                rng.uniform(-0.5, 0.5) as f32 * length,
                rng.uniform(-0.5, 0.5) as f32 * width,
                0.0,
            ),
        };
        let z_source = get(&e.z_source, 0.0);
        if z_source > 0.0 {
            dir = (local - Vec3::new(0.0, 0.0, -z_source)).normalize_or(dir);
        }
        let speed = get(&e.emission_speed, 0.0);
        let vary = get(&e.speed_variation, 0.0);
        let speed = speed * (1.0 + rng.uniform(-f64::from(vary), f64::from(vary)) as f32);
        let life =
            get(&e.lifespan, 1.0) * (1.0 + rng.uniform(-f64::from(e.lifespan_vary), f64::from(e.lifespan_vary)) as f32);
        if life <= 0.0 {
            return None;
        }
        let cells = (e.rows.max(1) * e.cols.max(1)) as i64;
        let cell = if e.flags & RANDOM_CELL != 0 {
            rng.integer(0, cells) as u16
        } else {
            0
        };
        let mut pm = |v: f32| rng_signed(rng) * v;
        let (spin, spin_rate, scale) = (
            e.base_spin + pm(e.base_spin_vary),
            e.spin + pm(e.spin_vary),
            1.0 + pm(e.scale_vary[0]),
        );
        let trails = e.flags & NO_TRAIL == 0;
        let (pos, vel) = if trails {
            (m.transform_point3(local), m.transform_vector3(dir * speed))
        } else {
            (local, dir * speed)
        };
        Some(Particle {
            pos,
            vel,
            age: 0.0,
            life,
            spin,
            spin_rate,
            scale,
            cell,
        })
    }

    /// One step of `dt` seconds, the model at `clock` posed as `pose`.
    pub fn step(&mut self, model: &Model, clock: &Clock, pose: &[Mat4], dt: f32) {
        let loops = &model.global_loops;
        for (e, state) in model.particles.iter().zip(&mut self.emitters) {
            let gravity = sample(&e.gravity, clock, loops).unwrap_or(0.0);
            let drag = e.drag;
            state.particles.retain_mut(|p| {
                p.age += dt;
                if p.age >= p.life {
                    return false;
                }
                p.vel.z -= gravity * dt;
                if drag > 0.0 {
                    p.vel *= (1.0 - drag * dt).max(0.0);
                }
                p.pos += p.vel * dt;
                true
            });
            let enabled = sample(&e.enabled, clock, loops).unwrap_or(1) != 0;
            let rate = sample(&e.emission_rate, clock, loops).unwrap_or(0.0).max(0.0);
            if enabled {
                state.owed += f64::from(rate) * f64::from(dt);
            }
            let m = emitter_matrix(pose, e);
            while state.owed >= 1.0 {
                state.owed -= 1.0;
                if let Some(p) = Self::spawn(&mut self.rng, e, clock, loops, m) {
                    state.particles.push(p);
                }
            }
        }
        for (r, state) in model.ribbons.iter().zip(&mut self.ribbons) {
            state.edges.retain_mut(|edge| {
                edge.age += dt;
                edge.pos.z -= r.gravity * edge.age * dt;
                edge.age < r.edge_lifetime
            });
            let bone = pose.get(r.bone as usize).copied().unwrap_or(Mat4::IDENTITY);
            state.owed += f64::from(r.edges_per_second) * f64::from(dt);
            while state.owed >= 1.0 {
                state.owed -= 1.0;
                state.edges.push(Edge {
                    pos: bone.transform_point3(Vec3::from(r.position)),
                    up: bone.transform_vector3(Vec3::Z),
                    age: 0.0,
                });
            }
        }
        self.time += f64::from(dt);
    }

    /// The particles as drawn at `clock`, the model posed as `pose`.
    pub fn sprites(&self, model: &Model, pose: &[Mat4]) -> Vec<Sprite> {
        let mut out = Vec::new();
        for (index, (e, state)) in model.particles.iter().zip(&self.emitters).enumerate() {
            let m = emitter_matrix(pose, e);
            let cells = (e.rows.max(1) * e.cols.max(1)).max(1);
            for p in &state.particles {
                let t = p.age / p.life;
                let rgb = sample_life(&e.color_track, t).unwrap_or([255.0; 3]);
                let alpha = sample_life(&e.alpha_track, t).map_or(1.0, |a| f32::from(a) / 32767.0);
                let size = sample_life(&e.scale_track, t).unwrap_or([1.0, 1.0]);
                let head = sample_life(&e.head_cell, t).unwrap_or(0);
                let pos = if e.flags & NO_TRAIL == 0 {
                    p.pos
                } else {
                    m.transform_point3(p.pos)
                };
                out.push(Sprite {
                    emitter: index,
                    pos,
                    size: [size[0] * p.scale / 2.0, size[1] * p.scale / 2.0],
                    rotation: p.spin + p.spin_rate * p.age,
                    color: [rgb[0] / 255.0, rgb[1] / 255.0, rgb[2] / 255.0, alpha.clamp(0.0, 1.0)],
                    cell: (head + p.cell) % cells,
                });
            }
        }
        out
    }

    /// The ribbons as drawn at `clock`, their head (the bone's place now) first.
    pub fn strips(&self, model: &Model, clock: &Clock, pose: &[Mat4]) -> Vec<Strip> {
        let loops = &model.global_loops;
        let mut out = Vec::new();
        for (index, (r, state)) in model.ribbons.iter().zip(&self.ribbons).enumerate() {
            let visible = sample(&r.visibility, clock, loops).unwrap_or(1) != 0;
            if !visible {
                continue;
            }
            let above = sample(&r.height_above, clock, loops).unwrap_or(0.0);
            let below = sample(&r.height_below, clock, loops).unwrap_or(0.0);
            let rgb = sample(&r.color, clock, loops).unwrap_or([1.0; 3]);
            let alpha = sample(&r.alpha, clock, loops).map_or(1.0, |a| f32::from(a) / 32767.0);
            let bone = pose.get(r.bone as usize).copied().unwrap_or(Mat4::IDENTITY);
            let head = Edge {
                pos: bone.transform_point3(Vec3::from(r.position)),
                up: bone.transform_vector3(Vec3::Z),
                age: 0.0,
            };
            let edges: Vec<&Edge> = std::iter::once(&head).chain(state.edges.iter().rev()).collect();
            if edges.len() < 2 {
                continue;
            }
            let span = r.edge_lifetime.max(1e-3);
            let points = edges
                .iter()
                .map(|e| {
                    let up = e.up.normalize_or(Vec3::Z);
                    (e.pos + up * above, e.pos - up * below, (e.age / span).min(1.0))
                })
                .collect();
            out.push(Strip {
                ribbon: index,
                points,
                color: [rgb[0], rgb[1], rgb[2], alpha.clamp(0.0, 1.0)],
            });
        }
        out
    }

    /// How many particles are alive.
    pub fn alive(&self) -> usize {
        self.emitters.iter().map(|e| e.particles.len()).sum()
    }
}

fn rng_signed(rng: &mut Rng) -> f32 {
    rng.uniform(-1.0, 1.0) as f32
}

/// The ribbon emitters' edges alive at most, for the client's limit (64).
pub fn ribbon_edges(r: &Ribbon) -> f32 {
    r.edges_per_second * r.edge_lifetime
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{self, Options};
    use crate::json;

    #[test]
    fn the_same_time_gives_the_same_particles() {
        let spec = json::parse(
            r##"{"name": "t", "duration": 1000, "textures": {"g": {"kind": "glow", "size": 8}},
            "bones": {"o": {"orbit": {"radius": 1, "speed": 1}}},
            "emitters": [{"texture": "g", "rate": 50, "life": 0.5, "speed": 2, "spread": 30, "gravity": 1}],
            "ribbons": [{"texture": "g", "bone": "o", "edges": 30, "life": 0.5}]}"##,
        )
        .unwrap();
        let options = Options {
            textures: false,
            ..Options::default()
        };
        let model = forge::forge(&spec, &options).unwrap().model;
        let playback = anim::Playback::of(&model, 3);
        let mut a = Simulation::new(&model, 7);
        a.seek(&model, &playback, 0.8);
        let alive = a.alive();
        assert!(
            (15..=30).contains(&alive),
            "{alive} particles after 0.8 s at 50/s for 0.5 s"
        );
        let mut b = Simulation::new(&model, 7);
        b.seek(&model, &playback, 0.3);
        b.seek(&model, &playback, 0.8);
        let clock = playback.clock(800.0);
        let pose = anim::pose(&model, &clock, None);
        assert_eq!(a.sprites(&model, &pose), b.sprites(&model, &pose));
        // scrubbed back, then forward again: the same
        b.seek(&model, &playback, 0.1);
        b.seek(&model, &playback, 0.8);
        assert_eq!(a.sprites(&model, &pose), b.sprites(&model, &pose));
        let strips = a.strips(&model, &clock, &pose);
        assert_eq!(strips.len(), 1);
        assert!(strips[0].points.len() > 10);
    }
}
