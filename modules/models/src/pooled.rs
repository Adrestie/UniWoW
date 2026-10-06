//! A look made ready to draw from the pool, in a job: its model's vertices and the indices of its
//! skins in the arenas, read once whoever asks; its batches as records of draws, their materials in
//! the table and their textures in the arrays. A texture or a model finding no room sends the look
//! back to the path of step 9.4c, its own buffers and textures.

use std::ops::Range;
use std::sync::Arc;

use uniwow_api::bytemuck;
use uniwow_api::formats::{FileRef, Formats, Model};
use uniwow_api::glam::Vec3;
use uniwow_api::models::Look;
use uniwow_api::texture_arrays::{NONE, Placed, Refused};

use crate::gpu::{State, Vertex, moving_radius};
use crate::loading::{Caches, key, plan};
use crate::pool::{MaterialGpu, Pool};

/// The vertices and the indices of a model in the arenas, given back when no look holds it, and
/// what of the model its looks read: its vertices left out.
pub struct ArenaModel {
    pub model: Model,
    /// The bounds of its vertices at rest; those of the model hold its animations too.
    pub rest: [Vec3; 2],
    /// Its first vertex in the arena of the vertices, and the first index of each skin in that of
    /// the indices.
    pub base_vertex: u32,
    pub skins: Vec<u32>,
    vertices: Range<u64>,
    indices: Range<u64>,
    pool: Arc<Pool>,
    pub bytes: u64,
}

impl Drop for ArenaModel {
    fn drop(&mut self) {
        self.pool.vertices.give(self.vertices.clone());
        self.pool.indices.give(self.indices.clone());
    }
}

/// A batch drawn from the pool: its indices in the arena, its first vertex there, its material in
/// the table, and the state of its pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Record {
    pub first_index: u32,
    pub count: u32,
    pub base_vertex: i32,
    pub material: u32,
    pub state: State,
}

/// A look drawn from the pool: its model, the textures it holds, and its records of each skin, in
/// the order they are drawn; its materials given back with it.
pub struct PooledLook {
    pub model: Arc<ArenaModel>,
    pub textures: Vec<Arc<Placed>>,
    pub skins: Vec<Vec<Record>>,
    materials: Range<u64>,
    pool: Arc<Pool>,
    /// The bytes of its materials.
    pub bytes: u64,
}

impl Drop for PooledLook {
    fn drop(&mut self) {
        self.pool.materials.give(self.materials.clone());
    }
}

/// The model `file` in the arenas.
pub fn model(pool: &Arc<Pool>, formats: &dyn Formats, file: &FileRef) -> Result<ArenaModel, String> {
    let mut model = formats.model(file)?;
    model.radius = moving_radius(&model);
    let bones = model.animation.bones.len();
    let vertices: Vec<Vertex> = model.vertices.iter().map(|vertex| Vertex::of(vertex, bones)).collect();
    let rest = if model.vertices.is_empty() {
        [Vec3::ZERO; 2]
    } else {
        model
            .vertices
            .iter()
            .fold([Vec3::INFINITY, Vec3::NEG_INFINITY], |[low, high], vertex| {
                let position = Vec3::from(vertex.position);
                [low.min(position), high.max(position)]
            })
    };
    let mut indices: Vec<u32> = Vec::new();
    let mut starts = Vec::with_capacity(model.skins.len());
    for skin in &model.skins {
        starts.push(indices.len() as u32);
        indices.extend_from_slice(&skin.triangles);
    }
    let vertex_range = pool.vertices.put(bytemuck::cast_slice(&vertices))?;
    let index_range = match pool.indices.put(bytemuck::cast_slice(&indices)) {
        Ok(range) => range,
        Err(why) => {
            pool.vertices.give(vertex_range);
            return Err(why);
        }
    };
    model.vertices = Vec::new();
    Ok(ArenaModel {
        model,
        rest,
        base_vertex: vertex_range.start as u32,
        skins: starts.iter().map(|start| index_range.start as u32 + start).collect(),
        bytes: (vertices.len() * size_of::<Vertex>() + indices.len() * 4) as u64,
        vertices: vertex_range,
        indices: index_range,
        pool: pool.clone(),
    })
}

/// `look` ready to draw from the pool; the textures that could not be read drawn white, said once
/// each in `refused`. Refused, and why, when its model or a texture finds no room there.
pub fn look(
    pool: &Arc<Pool>,
    formats: &dyn Formats,
    caches: &Caches,
    look: &Look,
    refused: &mut Vec<String>,
) -> Result<PooledLook, String> {
    let model = caches
        .pooled
        .get(&key(&look.model), || self::model(pool, formats, &look.model))?;
    let mut held: Vec<Arc<Placed>> = Vec::new();
    let mut materials: Vec<MaterialGpu> = Vec::new();
    let mut planned_skins = Vec::new();
    for batches in plan(&model.model, look) {
        let mut planned_skin = Vec::with_capacity(batches.len());
        for planned in batches {
            let mut codes = [NONE; 2];
            let mut sizes = [1.0; 4];
            let mut wraps = 0;
            for (slot, texture) in planned.textures.iter().enumerate() {
                let Some((file, wrap)) = texture else {
                    continue;
                };
                match pool.arrays.fetch(formats, file) {
                    Ok(placed) => {
                        codes[slot] = placed.code();
                        sizes[2 * slot] = placed.width as f32;
                        sizes[2 * slot + 1] = placed.height as f32;
                        wraps |= wrap << (2 * slot);
                        if !held.iter().any(|kept| Arc::ptr_eq(kept, &placed)) {
                            held.push(placed);
                        }
                    }
                    Err(Refused::Unreadable(why)) => {
                        let why = format!("{file:?}: {why}");
                        if !refused.contains(&why) {
                            refused.push(why);
                        }
                    }
                    Err(Refused::NoRoom(why)) => return Err(why),
                }
            }
            let params = planned.params;
            materials.push(MaterialGpu {
                colour: params.colour,
                flags: params.flags,
                model: params.model,
                combine: params.combine,
                textures: [codes[0], codes[1], wraps, 0],
                sizes,
            });
            // Its pipeline made here, in the job, rather than by the first frame drawing it.
            pool.pipeline(planned.state);
            planned_skin.push((planned.indices, planned.state));
        }
        planned_skins.push(planned_skin);
    }
    let range = pool.materials.put(bytemuck::cast_slice(&materials))?;
    let mut material = range.start as u32;
    let skins = planned_skins
        .into_iter()
        .enumerate()
        .map(|(level, batches)| {
            batches
                .into_iter()
                .map(|(indices, state)| {
                    let record = Record {
                        first_index: model.skins[level] + indices.start,
                        count: indices.len() as u32,
                        base_vertex: model.base_vertex as i32,
                        material,
                        state,
                    };
                    material += 1;
                    record
                })
                .collect()
        })
        .collect();
    Ok(PooledLook {
        model,
        textures: held,
        skins,
        bytes: (materials.len() * size_of::<MaterialGpu>()) as u64,
        materials: range,
        pool: pool.clone(),
    })
}
