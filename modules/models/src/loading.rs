//! A look made ready to draw, in a job. Its batches are planned once whichever way it is drawn:
//! those of the submeshes it shows and seen at rest, but the layers merged into their first, each
//! with the state of its pipeline, the shader WotLK chooses for it (`shaders`), its one or two
//! textures and its colour. Drawn from the pool (`pooled`), or with buffers and textures of its own,
//! the path of step 9.4c: its model from the cache of models, read once whoever asks, its vertices
//! and the indices of each skin in buffers, each batch with its pipeline and its textures from the
//! cache of textures with their samplers; all created and filled from the job.

use std::ops::Range;
use std::sync::Arc;

use uniwow_api::formats::{
    self, Animation, Batch, Bone, FacialHair, FileRef, Formats, HairGeoset, Keys, Model, ModelTextureSource, Sequence,
    Submesh, TextureFormat, Track,
};
use uniwow_api::glam::Vec3;
use uniwow_api::models::{Extent, Geosets, Look};
use uniwow_api::{bytemuck, wgpu};

use crate::cache::Cache;
use crate::dress::{self, Moving};
use crate::gpu::{BatchParams, Shared, State, TextureGpu, Vertex, flags, moving_radius};
use crate::pooled::{ArenaModel, PooledLook};
use crate::shaders;

/// The vertices and skins of a model on the GPU, and what of the model its looks read: its
/// vertices left out.
pub struct ModelGpu {
    pub model: Model,
    /// The bounds of its vertices at rest; those of the model hold its animations too.
    pub rest: [Vec3; 2],
    pub vertices: wgpu::Buffer,
    pub skins: Vec<SkinGpu>,
    pub bytes: u64,
    /// What it keeps on the CPU (`cpu_bytes`).
    pub cpu: [u64; 2],
}

/// The bytes of a track: its keys of each sequence.
fn track_bytes<T>(track: &Track<T>) -> u64 {
    track
        .keys
        .iter()
        .map(|keys| {
            size_of::<Keys<T>>() + keys.times.len() * 4 + (keys.values.len() + keys.tangents.len() * 2) * size_of::<T>()
        })
        .sum::<usize>() as u64
}

/// What `model`, its vertices left out, keeps on the CPU, as far as its lists count: its skins and
/// the rest, then its animation.
pub fn cpu_bytes(model: &Model) -> [u64; 2] {
    let skins: usize = model
        .skins
        .iter()
        .map(|skin| {
            skin.triangles.len() * 4
                + skin.submeshes.len() * size_of::<Submesh>()
                + skin.batches.len() * size_of::<Batch>()
        })
        .sum();
    let rest = size_of::<Model>()
        + model.textures.len() * size_of::<formats::ModelTexture>()
        + model.materials.len() * size_of::<formats::Material>()
        + (model.texture_combos.len()
            + model.uv_combos.len()
            + model.weight_combos.len()
            + model.transform_combos.len()
            + model.combiner_combos.len())
            * 2
        + model.colours.len() * 16
        + model.weights.len() * 4;
    let animation = &model.animation;
    let mut moving = (animation.sequences.len() * size_of::<Sequence>()
        + animation.globals.len() * 4
        + animation.order.len() * 2
        + animation.bones.len() * size_of::<Bone>()) as u64;
    for bone in &animation.bones {
        moving += track_bytes(&bone.translation) + track_bytes(&bone.rotation) + track_bytes(&bone.scale);
    }
    for (colour, alpha) in &animation.colours {
        moving += track_bytes(colour) + track_bytes(alpha);
    }
    moving += animation.weights.iter().map(track_bytes).sum::<u64>();
    for transform in &animation.transforms {
        moving +=
            track_bytes(&transform.translation) + track_bytes(&transform.rotation) + track_bytes(&transform.scale);
    }
    [(skins + rest) as u64, moving]
}

pub struct SkinGpu {
    pub indices: wgpu::Buffer,
    pub format: wgpu::IndexFormat,
}

/// A batch drawn: a range of the indices of its skin, its pipeline and its bind group.
pub struct BatchGpu {
    pub indices: Range<u32>,
    pub state: State,
    pub pipeline: Arc<wgpu::RenderPipeline>,
    pub group: wgpu::BindGroup,
}

/// A look on the GPU: its model, the textures it holds, and the batches it draws of each skin, in
/// the order they are drawn.
pub struct LookGpu {
    pub model: Arc<ModelGpu>,
    pub textures: Vec<Arc<TextureGpu>>,
    pub skins: Vec<Vec<BatchGpu>>,
    /// The slots of its materials that move.
    pub moving: Vec<Moving>,
    /// The bytes of its own, outside its model and textures.
    pub bytes: u64,
}

impl LookGpu {
    pub fn radius(&self) -> f32 {
        self.model.model.radius
    }
}

/// A look ready to draw: from the pool, or with buffers and textures of its own.
pub enum Ready {
    Pooled(PooledLook),
    Own(LookGpu),
}

impl Ready {
    pub fn radius(&self) -> f32 {
        match self {
            Ready::Pooled(look) => look.model.model.radius,
            Ready::Own(look) => look.radius(),
        }
    }

    /// What it is made of, with the setting `reach`.
    pub fn extent(&self, reach: f32) -> Extent {
        let (rest, batches) = match self {
            Ready::Pooled(look) => (look.model.rest, look.skins.first().map_or(0, Vec::len)),
            Ready::Own(look) => (look.model.rest, look.skins.first().map_or(0, Vec::len)),
        };
        Extent {
            low: rest[0],
            high: rest[1],
            radius: self.radius(),
            batches,
            reach,
        }
    }

    /// The slots of its materials that move (`dress`).
    pub fn moving(&self) -> &[Moving] {
        match self {
            Ready::Pooled(look) => &look.moving,
            Ready::Own(look) => &look.moving,
        }
    }

    /// The model it draws.
    pub fn model(&self) -> &Model {
        match self {
            Ready::Pooled(look) => &look.model.model,
            Ready::Own(look) => &look.model.model,
        }
    }

    /// What moves in its model.
    pub fn animation(&self) -> &Animation {
        match self {
            Ready::Pooled(look) => &look.model.model.animation,
            Ready::Own(look) => &look.model.model.animation,
        }
    }

    /// Its levels of detail, its skins.
    pub fn levels(&self) -> usize {
        match self {
            Ready::Pooled(look) => look.skins.len(),
            Ready::Own(look) => look.skins.len(),
        }
    }

    /// The state of each batch of the level `level`, and its indices.
    pub fn batches(&self, level: usize) -> Vec<(State, u32)> {
        match self {
            Ready::Pooled(look) => look.skins[level]
                .iter()
                .map(|record| (record.state, record.count))
                .collect(),
            Ready::Own(look) => look.skins[level]
                .iter()
                .map(|batch| (batch.state, batch.indices.len() as u32))
                .collect(),
        }
    }
}

/// What the looks share: the models and the textures of their own, by their files, and the models
/// in the pool.
#[derive(Default)]
pub struct Caches {
    pub models: Cache<String, ModelGpu>,
    pub textures: Cache<String, TextureGpu>,
    pub pooled: Cache<String, ArenaModel>,
}

/// The key of `file` in the caches: a path in lower case with backslashes, `.mdx` and `.mdl` read
/// as `.m2`; a FileDataID by its number.
pub fn key(file: &FileRef) -> String {
    match file {
        FileRef::Path(path) => {
            let path = path.to_ascii_lowercase().replace('/', "\\");
            match [".mdx", ".mdl"].iter().find(|old| path.ends_with(*old)) {
                Some(old) => format!("{}.m2", &path[..path.len() - old.len()]),
                None => path,
            }
        }
        FileRef::Id(id) => format!("#{id}"),
    }
}

/// Which submeshes of `ids` the look shows.
pub fn shown(ids: &[u16], geosets: Geosets) -> Vec<bool> {
    match geosets {
        Geosets::All => vec![true; ids.len()],
        Geosets::Default => formats::default_geosets(ids),
        Geosets::Creature(chosen) => formats::creature_geosets(ids, chosen),
        Geosets::Character { hair, facial } => {
            let hair = HairGeoset {
                race: 0,
                sex: 0,
                variation: 0,
                geoset: hair,
                scalp: false,
            };
            let facial = FacialHair {
                race: 0,
                sex: 0,
                variation: 0,
                geosets: facial,
            };
            formats::look_geosets(ids, Some(&hair), Some(&facial))
        }
    }
}

/// The colour and transparency at rest of the batch `batch` of `model`, its weight included.
pub fn colour(model: &Model, batch: &formats::Batch) -> [f32; 4] {
    let [r, g, b, a] = batch
        .colour
        .map_or([1.0; 4], |colour| model.colours[usize::from(colour)]);
    let weight = model.weights[usize::from(model.weight_combos[usize::from(batch.weight_combo)])];
    [r, g, b, (a * weight).clamp(0.0, 1.0)]
}

/// The model `file` on the GPU.
fn model(shared: &Shared, formats: &dyn Formats, file: &FileRef) -> Result<ModelGpu, String> {
    let mut model = formats.model(file)?;
    model.radius = moving_radius(&model);
    let bones = model.animation.bones.len();
    let vertices: Vec<Vertex> = model.vertices.iter().map(|vertex| Vertex::of(vertex, bones)).collect();
    let mut bytes = (vertices.len() * size_of::<Vertex>()) as u64;
    let vertex_buffer = shared.buffer(
        "models vertices",
        bytemuck::cast_slice(&vertices),
        wgpu::BufferUsages::VERTEX,
    );
    let small = vertices.len() <= usize::from(u16::MAX) + 1;
    let skins = model
        .skins
        .iter()
        .map(|skin| {
            let (data, format) = if small {
                let indices: Vec<u16> = skin.triangles.iter().map(|index| *index as u16).collect();
                (bytemuck::cast_slice(&indices).to_vec(), wgpu::IndexFormat::Uint16)
            } else {
                (
                    bytemuck::cast_slice(&skin.triangles).to_vec(),
                    wgpu::IndexFormat::Uint32,
                )
            };
            bytes += data.len() as u64;
            // A copy's size is a multiple of 4 bytes.
            let mut data = data;
            data.resize(data.len().next_multiple_of(4), 0);
            SkinGpu {
                indices: shared.buffer("models indices", &data, wgpu::BufferUsages::INDEX),
                format,
            }
        })
        .collect();
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
    model.vertices = Vec::new();
    Ok(ModelGpu {
        cpu: cpu_bytes(&model),
        model,
        rest,
        vertices: vertex_buffer,
        skins,
        bytes,
    })
}

/// The texture `file` on the GPU: as BC when the device takes it and the file stores it so, else
/// as RGBA.
fn texture(shared: &Shared, formats: &dyn Formats, file: &FileRef) -> Result<TextureGpu, String> {
    let mut texture = if shared.block_compression {
        formats.texture(file)?
    } else {
        formats.texture_rgba(file)?
    };
    if texture.format != TextureFormat::Rgba8 && (texture.width % 4 != 0 || texture.height % 4 != 0) {
        texture = formats.texture_rgba(file)?;
    }
    shared.texture(&texture)
}

/// A texture of a batch on the GPU, with how it wraps.
type Bound = Option<(Arc<TextureGpu>, usize)>;

/// The view of `texture`, white for none.
fn view<'a>(shared: &'a Shared, texture: &'a Bound) -> &'a wgpu::TextureView {
    texture.as_ref().map_or(&shared.white, |(texture, _)| &texture.view)
}

/// The sampler of `texture` by how it wraps.
fn sampler<'a>(shared: &'a Shared, texture: &Bound) -> &'a wgpu::Sampler {
    &shared.samplers[texture.as_ref().map_or(0, |(_, wrap)| *wrap)]
}

/// A batch of a look before it goes to the GPU: its range of indices in its skin, the state of its
/// pipeline, what its shader reads, and its one or two textures with how each wraps (across 1, up
/// and down 2), none drawn white.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub indices: Range<u32>,
    pub state: State,
    pub params: BatchParams,
    pub textures: [Option<(FileRef, u32)>; 2],
}

/// The batches `look` draws of each skin of its model `data`, in the order they are drawn: those
/// of the submeshes it shows, seen at rest or whose material moves, but the layers merged into
/// their first, each with the shader WotLK chooses for it; and the slots of the materials that
/// move, which their batches point to (`dress`).
pub fn plan(data: &Model, look: &Look) -> (Vec<Vec<Planned>>, Vec<Moving>) {
    // The file of the texture `index` of the model: a file, one the display fills, or none.
    let file_of = |index: usize| match &data.textures[index].source {
        ModelTextureSource::File(file) => Some(file.clone()),
        ModelTextureSource::Filled(kind) => look
            .textures
            .iter()
            .find(|(filled, _)| filled == kind)
            .map(|(_, file)| file.clone()),
        ModelTextureSource::Unnamed => None,
    };
    let mut slots: Vec<Moving> = Vec::new();
    let skins = data
        .skins
        .iter()
        .map(|skin| {
            let ids: Vec<u16> = skin.submeshes.iter().map(|submesh| submesh.id).collect();
            let shown = shown(&ids, look.geosets);
            let shaders = shaders::select(data, skin);
            let mut batches: Vec<(i8, usize, Planned)> = Vec::new();
            for (order, (batch, shader)) in skin.batches.iter().zip(shaders).enumerate() {
                let colour = colour(data, batch);
                // A layer merged into its first is drawn by it.
                let Some(shader) = shader else {
                    continue;
                };
                let moving = dress::moving(data, batch, shader.textures.len());
                if !shown[usize::from(batch.submesh)] || (colour[3] <= 0.0 && moving.is_none()) {
                    continue;
                }
                // Its slot plus one, 0 for a material that does not move.
                let slot = moving.map_or(0, |moving| {
                    let at = slots.iter().position(|kept| *kept == moving).unwrap_or_else(|| {
                        slots.push(moving);
                        slots.len() - 1
                    });
                    at as u32 + 1
                });
                let submesh = &skin.submeshes[usize::from(batch.submesh)];
                let material = &data.materials[usize::from(batch.material)];
                let textures = [0, 1].map(|slot| {
                    let index = *shader.textures.get(slot)?;
                    file_of(index).map(|file| (file, data.textures[index].flags & 3))
                });
                batches.push((
                    batch.priority,
                    order,
                    Planned {
                        indices: submesh.start..submesh.start + submesh.count,
                        state: State::of(material),
                        params: BatchParams {
                            colour,
                            flags: flags(material),
                            model: [data.radius, 0.0, 0.0, 0.0],
                            combine: [
                                shader.combiner as u32,
                                shader.coords[0] as u32,
                                shader.coords[1] as u32,
                                slot,
                            ],
                        },
                        textures,
                    },
                ));
            }
            // The planes of priority first, then the order of the skin, which keeps a batch's
            // layers after it.
            batches.sort_by_key(|(priority, order, _)| (*priority, *order));
            batches.into_iter().map(|(_, _, batch)| batch).collect()
        })
        .collect();
    (skins, slots)
}

/// `look` ready to draw with buffers and textures of its own; the textures that could not be read
/// drawn white, said once each in `refused`.
pub fn look(
    shared: &Shared,
    formats: &dyn Formats,
    caches: &Caches,
    look: &Look,
    refused: &mut Vec<String>,
) -> Result<LookGpu, String> {
    let model = caches
        .models
        .get(&key(&look.model), || self::model(shared, formats, &look.model))?;
    let mut held: Vec<Arc<TextureGpu>> = Vec::new();
    let mut bytes = 0;
    // A texture on the GPU, with how it wraps; none for white.
    let mut texture_of = |texture: &Option<(FileRef, u32)>, refused: &mut Vec<String>| -> Bound {
        let (file, wrap) = texture.as_ref()?;
        match caches.textures.get(&key(file), || self::texture(shared, formats, file)) {
            Ok(texture) => {
                if !held.iter().any(|kept| Arc::ptr_eq(kept, &texture)) {
                    held.push(texture.clone());
                }
                Some((texture, *wrap as usize))
            }
            Err(why) => {
                let why = format!("{file:?}: {why}");
                if !refused.contains(&why) {
                    refused.push(why);
                }
                None
            }
        }
    };
    let (planned_skins, moving) = plan(&model.model, look);
    let skins = planned_skins
        .into_iter()
        .map(|batches| {
            batches
                .into_iter()
                .map(|planned| {
                    let [one, two] = [0, 1].map(|slot| texture_of(&planned.textures[slot], refused));
                    let uniform = shared.buffer(
                        "models batch",
                        bytemuck::bytes_of(&planned.params),
                        wgpu::BufferUsages::UNIFORM,
                    );
                    bytes += size_of::<BatchParams>() as u64;
                    let group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("models batch"),
                        layout: &shared.batch_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: uniform.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(view(shared, &one)),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(sampler(shared, &one)),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::TextureView(view(shared, &two)),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: wgpu::BindingResource::Sampler(sampler(shared, &two)),
                            },
                        ],
                    });
                    BatchGpu {
                        indices: planned.indices,
                        state: planned.state,
                        pipeline: shared.pipeline(planned.state),
                        group,
                    }
                })
                .collect()
        })
        .collect();
    Ok(LookGpu {
        model,
        textures: held,
        skins,
        moving,
        bytes,
    })
}
