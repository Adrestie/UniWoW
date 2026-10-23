//! What a model draws at a moment, as textured triangles grouped by texture and blending: its
//! meshes skinned on the posed bones (colour, transparency and texture scroll of the moment), its
//! particles as quads facing the eye, its ribbons as strips. Built on the CPU, small enough for it:
//! the client's own limits (2,000 particles, 64 ribbon edges) hold the work down.

use crate::anim::{Clock, sample};
use crate::m2::{Model, Skin};
use crate::sim::{NO_BILLBOARD, Simulation};
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    /// Red, green, blue, alpha, 0 to 1, multiplying the texture.
    pub color: [f32; 4],
}

/// What a batch is: it sorts them, and the client draws particles and ribbons after the meshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Mesh(usize),
    Particles(usize),
    Ribbon(usize),
}

/// Triangles drawn with one texture and one blending.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub source: Source,
    /// The model's texture (its index), none for white.
    pub texture: Option<usize>,
    /// The blending of the M2 (0 opaque, 1 alpha key, 2 alpha, 3 additive without alpha, 4
    /// additive, 5 modulate, 6 modulate 2x).
    pub blend: u16,
    /// The material's flags (0x1 unlit, 0x4 two-sided, 0x10 no depth write...); none for particles.
    pub material_flags: u16,
    /// Laid on the ground by the client (batch flag 0x04).
    pub projected: bool,
    pub priority: i8,
    /// Its distance along the view, for drawing the blended ones far first.
    pub depth: f32,
    /// Three a triangle.
    pub vertices: Vec<Vertex>,
}

/// Where the model is seen from, in its own space.
#[derive(Clone, Copy, Debug)]
pub struct Eye {
    pub position: Vec3,
    /// The view's right and up, for quads facing it.
    pub right: Vec3,
    pub up: Vec3,
}

impl Eye {
    fn depth(&self, p: Vec3) -> f32 {
        let forward = self.up.cross(self.right);
        (p - self.position).dot(-forward)
    }
}

/// A vertex of the model as stored: place, weights, bones, normal, texture coordinates.
pub struct RawVertex {
    pub pos: Vec3,
    pub weights: [u8; 4],
    pub bones: [u8; 4],
    pub uv: [f32; 2],
}

pub fn raw(bytes: &[u8; 48]) -> RawVertex {
    let f = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    RawVertex {
        pos: Vec3::new(f(0), f(4), f(8)),
        weights: bytes[12..16].try_into().expect("four"),
        bones: bytes[16..20].try_into().expect("four"),
        uv: [f(32), f(36)],
    }
}

fn quad(out: &mut Vec<Vertex>, corners: [Vec3; 4], uvs: [[f32; 2]; 4], color: [f32; 4]) {
    for k in [0, 1, 2, 0, 2, 3] {
        out.push(Vertex {
            pos: corners[k].to_array(),
            uv: uvs[k],
            color,
        });
    }
}

/// The batches of the model at `clock`, posed as `pose`, its particles and ribbons as `sim` holds
/// them, seen from `eye`.
pub fn build(
    model: &Model,
    skin: Option<&Skin>,
    sim: &Simulation,
    clock: &Clock,
    pose: &[Mat4],
    eye: &Eye,
) -> Vec<Batch> {
    let loops = &model.global_loops;
    let mut out = Vec::new();
    // meshes
    if let Some(skin) = skin {
        let vertices: Vec<RawVertex> = model.vertices.iter().map(raw).collect();
        for (index, b) in skin.batches.iter().enumerate() {
            let (Some(sm), Some(material)) = (
                skin.submeshes.get(b.submesh as usize),
                model.materials.get(b.material as usize),
            ) else {
                continue;
            };
            let mut color = [1.0f32; 4];
            if let Some(c) = usize::try_from(b.color).ok().and_then(|c| model.colors.get(c)) {
                let rgb = sample(&c.color, clock, loops).unwrap_or([1.0; 3]);
                let alpha = sample(&c.alpha, clock, loops).map_or(1.0, |a| f32::from(a) / 32767.0);
                color = [rgb[0], rgb[1], rgb[2], alpha];
            }
            let weight = model
                .transparency_lookup
                .get(b.weight_combo as usize)
                .and_then(|w| usize::try_from(*w).ok())
                .and_then(|w| model.texture_weights.get(w))
                .and_then(|w| sample(&w.weight, clock, loops))
                .map_or(1.0, |w| f32::from(w) / 32767.0);
            color[3] *= weight;
            let scroll = model
                .texture_transform_lookup
                .get(b.transform_combo as usize)
                .and_then(|t| usize::try_from(*t).ok())
                .and_then(|t| model.texture_transforms.get(t))
                .and_then(|t| sample(&t.translation, clock, loops))
                .unwrap_or([0.0; 3]);
            let texture = model
                .texture_lookup
                .get(b.texture_combo as usize)
                .and_then(|t| usize::try_from(*t).ok());
            let start = sm.level as usize * 65536 + sm.index_start as usize;
            let end = (start + sm.index_count as usize).min(skin.indices.len());
            let mut tris = Vec::with_capacity(end.saturating_sub(start));
            let mut centre = Vec3::ZERO;
            for &k in skin.indices.get(start.min(end)..end).unwrap_or_default() {
                let k = k as usize;
                let (Some(&mv), Some(bones)) = (skin.vertices.get(k), skin.bones.get(k)) else {
                    continue;
                };
                let Some(v) = vertices.get(mv as usize) else {
                    continue;
                };
                let mut pos = Vec3::ZERO;
                let mut total = 0.0;
                for (w, slot) in v.weights.iter().zip(bones) {
                    if *w == 0 {
                        continue;
                    }
                    let bone = model
                        .bone_lookup
                        .get(sm.bone_combo as usize + *slot as usize)
                        .and_then(|b| usize::try_from(*b).ok())
                        .unwrap_or(0);
                    let m = pose.get(bone).copied().unwrap_or(Mat4::IDENTITY);
                    pos += m.transform_point3(v.pos) * (f32::from(*w) / 255.0);
                    total += f32::from(*w) / 255.0;
                }
                if total == 0.0 {
                    pos = v.pos;
                }
                centre += pos;
                tris.push(Vertex {
                    pos: pos.to_array(),
                    uv: [v.uv[0] + scroll[0], v.uv[1] + scroll[1]],
                    color,
                });
            }
            if tris.is_empty() || color[3] <= 0.002 {
                continue;
            }
            centre /= tris.len() as f32;
            out.push(Batch {
                source: Source::Mesh(index),
                texture,
                blend: material.blend,
                material_flags: material.flags,
                projected: b.flags & 0x04 != 0,
                priority: b.priority,
                depth: eye.depth(centre),
                vertices: tris,
            });
        }
    }
    // ribbons
    for strip in sim.strips(model, clock, pose) {
        let r = &model.ribbons[strip.ribbon];
        let n = strip.points.len();
        let mut tris = Vec::with_capacity((n - 1) * 6);
        for k in 0..n - 1 {
            let (a, b) = (&strip.points[k], &strip.points[k + 1]);
            let (u0, u1) = (k as f32 / (n - 1) as f32, (k + 1) as f32 / (n - 1) as f32);
            quad(
                &mut tris,
                [a.0, a.1, b.1, b.0],
                [[u0, 0.0], [u0, 1.0], [u1, 1.0], [u1, 0.0]],
                strip.color,
            );
        }
        let material = r.materials.first().and_then(|m| model.materials.get(*m as usize));
        out.push(Batch {
            source: Source::Ribbon(strip.ribbon),
            texture: r.textures.first().map(|t| *t as usize),
            blend: material.map_or(4, |m| m.blend),
            material_flags: material.map_or(0, |m| m.flags),
            projected: false,
            priority: r.priority_plane.clamp(-128, 127) as i8,
            depth: eye.depth(strip.points[0].0),
            vertices: tris,
        });
    }
    // particles, far first within each emitter
    let mut sprites = sim.sprites(model, pose);
    sprites.sort_by(|a, b| eye.depth(b.pos).total_cmp(&eye.depth(a.pos)));
    for (index, e) in model.particles.iter().enumerate() {
        let mine: Vec<_> = sprites.iter().filter(|s| s.emitter == index).collect();
        if mine.is_empty() {
            continue;
        }
        let (rows, cols) = (e.rows.max(1), e.cols.max(1));
        let flat = e.flags & NO_BILLBOARD != 0;
        let mut tris = Vec::with_capacity(mine.len() * 6);
        let mut far = f32::MIN;
        for s in &mine {
            let (right, up) = if flat { (Vec3::X, Vec3::Y) } else { (eye.right, eye.up) };
            let (sin, cos) = s.rotation.sin_cos();
            let r = (right * cos + up * sin) * s.size[0];
            let u = (up * cos - right * sin) * s.size[1];
            let (row, col) = (s.cell / cols % rows, s.cell % cols);
            let (cw, ch) = (1.0 / f32::from(cols), 1.0 / f32::from(rows));
            let (u0, v0) = (f32::from(col) * cw, f32::from(row) * ch);
            quad(
                &mut tris,
                [s.pos - r + u, s.pos - r - u, s.pos + r - u, s.pos + r + u],
                [[u0, v0], [u0, v0 + ch], [u0 + cw, v0 + ch], [u0 + cw, v0]],
                s.color,
            );
            far = far.max(eye.depth(s.pos));
        }
        out.push(Batch {
            source: Source::Particles(index),
            texture: Some(e.texture as usize),
            blend: u16::from(e.blend),
            material_flags: 0x1 | 0x4 | 0x10,
            projected: false,
            priority: 0,
            depth: far,
            vertices: tris,
        });
    }
    out
}

/// Heights of the ground about the effect, in its space: a square of `count` × `count` samples
/// `step` apart, its first at `low`; none where there is no ground.
#[derive(Clone, Debug, PartialEq)]
pub struct Ground {
    pub low: [f32; 2],
    pub step: f32,
    pub count: usize,
    pub heights: Vec<Option<f32>>,
}

impl Ground {
    /// The height at (x, y), read between the samples; none outside them or where one is missing.
    pub fn height(&self, x: f32, y: f32) -> Option<f32> {
        let fx = (x - self.low[0]) / self.step;
        let fy = (y - self.low[1]) / self.step;
        if fx < 0.0 || fy < 0.0 {
            return None;
        }
        let (ix, iy) = (fx.floor() as usize, fy.floor() as usize);
        if ix + 1 >= self.count || iy + 1 >= self.count {
            return None;
        }
        let at = |i: usize, j: usize| self.heights[j * self.count + i];
        let (h00, h10, h01, h11) = (at(ix, iy)?, at(ix + 1, iy)?, at(ix, iy + 1)?, at(ix + 1, iy + 1)?);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let top = h00 + (h10 - h00) * tx;
        let bottom = h01 + (h11 - h01) * tx;
        Some(top + (bottom - top) * ty)
    }
}

/// A projected batch laid on the ground, as the client lays the quads of its ground circles: each
/// triangle cut in `cuts` × `cuts`, each corner put on the ground under it, a little above it; where
/// there is no ground, left where it is.
pub fn drape(batch: &mut Batch, ground: &Ground, cuts: usize) {
    let cuts = cuts.max(1);
    let lift = 0.03;
    let mut out = Vec::with_capacity(batch.vertices.len() * cuts * cuts);
    let mix = |a: &Vertex, b: &Vertex, c: &Vertex, u: f32, v: f32| {
        let w = 1.0 - u - v;
        let pos: [f32; 3] = std::array::from_fn(|k| a.pos[k] * w + b.pos[k] * u + c.pos[k] * v);
        let height = ground.height(pos[0], pos[1]);
        Vertex {
            pos: [pos[0], pos[1], height.map_or(pos[2], |h| h + lift)],
            uv: std::array::from_fn(|k| a.uv[k] * w + b.uv[k] * u + c.uv[k] * v),
            color: a.color,
        }
    };
    for tri in batch.vertices.as_chunks::<3>().0 {
        let (a, b, c) = (&tri[0], &tri[1], &tri[2]);
        let n = cuts as f32;
        for i in 0..cuts {
            for j in 0..cuts - i {
                let (u, v) = (i as f32 / n, j as f32 / n);
                let d = 1.0 / n;
                out.push(mix(a, b, c, u, v));
                out.push(mix(a, b, c, u + d, v));
                out.push(mix(a, b, c, u, v + d));
                if i + j + 1 < cuts {
                    out.push(mix(a, b, c, u + d, v));
                    out.push(mix(a, b, c, u + d, v + d));
                    out.push(mix(a, b, c, u, v + d));
                }
            }
        }
    }
    batch.vertices = out;
}

/// The order the batches are drawn in: the opaque ones first, then the blended ones far first.
pub fn sort(batches: &mut [Batch]) {
    batches.sort_by(|a, b| {
        let blended = |x: &Batch| x.blend >= 2;
        blended(a)
            .cmp(&blended(b))
            .then(a.priority.cmp(&b.priority))
            .then(if blended(a) {
                b.depth.total_cmp(&a.depth)
            } else {
                std::cmp::Ordering::Equal
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{self, Playback};
    use crate::forge::{self, Options};
    use crate::json;

    #[test]
    fn a_model_draws_its_meshes_ribbons_and_particles() {
        let spec = json::parse(
            r##"{"name": "t", "duration": 1000, "textures": {"g": {"kind": "glow", "size": 8}},
            "bones": {"o": {"orbit": {"radius": 1, "speed": 1}}},
            "emitters": [{"texture": "g", "rate": 50, "life": 0.5, "speed": 2}],
            "ribbons": [{"texture": "g", "bone": "o", "edges": 30, "life": 0.5}],
            "meshes": [{"shape": "plane", "texture": "g", "alpha": 0.5, "uv_scroll": [1, 0]}]}"##,
        )
        .unwrap();
        let forged = forge::forge(
            &spec,
            &Options {
                textures: false,
                ..Options::default()
            },
        )
        .unwrap();
        let playback = Playback::of(&forged.model, 1);
        let mut sim = Simulation::new(&forged.model, 1);
        sim.seek(&forged.model, &playback, 0.5);
        let clock = playback.clock(500.0);
        let pose = anim::pose(&forged.model, &clock, None);
        let eye = Eye {
            position: Vec3::new(10.0, 0.0, 2.0),
            right: Vec3::Y,
            up: Vec3::Z,
        };
        let mut batches = build(&forged.model, Some(&forged.skin), &sim, &clock, &pose, &eye);
        sort(&mut batches);
        let kinds: Vec<&str> = batches
            .iter()
            .map(|b| match b.source {
                Source::Mesh(_) => "mesh",
                Source::Ribbon(_) => "ribbon",
                Source::Particles(_) => "particles",
            })
            .collect();
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        let mesh = batches.iter().find(|b| b.source == Source::Mesh(0)).unwrap();
        assert_eq!(mesh.vertices.len(), 6);
        assert!((mesh.vertices[0].color[3] - 0.5).abs() < 1e-3);
        // halfway through the loop, the texture has scrolled half a turn
        let scroll = mesh.vertices.iter().map(|v| v.uv[0]).fold(f32::MAX, f32::min);
        assert!((scroll - 0.5).abs() < 1e-3, "{scroll}");
    }

    #[test]
    fn a_projected_quad_follows_the_ground() {
        // a slope rising along x: height = x / 2
        let count = 21;
        let ground = Ground {
            low: [-5.0, -5.0],
            step: 0.5,
            count,
            heights: (0..count * count)
                .map(|k| Some((-5.0 + (k % count) as f32 * 0.5) / 2.0))
                .collect(),
        };
        assert_eq!(ground.height(2.0, 1.0), Some(1.0));
        let v = |x: f32, y: f32| Vertex {
            pos: [x, y, 0.0],
            uv: [x, y],
            color: [1.0; 4],
        };
        let mut batch = Batch {
            source: Source::Mesh(0),
            texture: None,
            blend: 2,
            material_flags: 0,
            projected: true,
            priority: 0,
            depth: 0.0,
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(-1.0, 1.0)],
        };
        drape(&mut batch, &ground, 4);
        assert_eq!(batch.vertices.len(), 16 * 3, "cut in 16");
        for vertex in &batch.vertices {
            assert!(
                (vertex.pos[2] - (vertex.pos[0] / 2.0 + 0.03)).abs() < 1e-4,
                "{vertex:?}"
            );
            assert_eq!(vertex.uv, [vertex.pos[0], vertex.pos[1]], "the texture laid flat");
        }
    }
}
