//! A look made ready to draw, in a job: its model from the cache of models, read once whoever asks,
//! its vertices and the indices of each skin in buffers; its batches, those of the submeshes it
//! shows and seen at rest, but the layers merged into their first, each with its pipeline, the
//! shader WotLK chooses for it (`shaders`), its one or two textures from the cache of textures
//! with their samplers, and its colour; all created and filled from the job.

use std::ops::Range;
use std::sync::Arc;

use uniwow_api::formats::{self, FacialHair, FileRef, Formats, HairGeoset, Model, ModelTextureSource, TextureFormat};
use uniwow_api::models::{Geosets, Look};
use uniwow_api::{bytemuck, wgpu};

use crate::cache::Cache;
use crate::gpu::{BatchParams, Shared, State, TextureGpu, Vertex, flags};
use crate::shaders;

/// The vertices and skins of a model on the GPU, and what of the model its looks read: its
/// vertices left out.
pub struct ModelGpu {
    pub model: Model,
    pub vertices: wgpu::Buffer,
    pub skins: Vec<SkinGpu>,
    pub bytes: u64,
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
    /// The bytes of its own, outside its model and textures.
    pub bytes: u64,
}

impl LookGpu {
    pub fn radius(&self) -> f32 {
        self.model.model.radius
    }
}

/// What the looks share: the models and the textures, by their files.
#[derive(Default)]
pub struct Caches {
    pub models: Cache<String, ModelGpu>,
    pub textures: Cache<String, TextureGpu>,
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
    let vertices: Vec<Vertex> = model
        .vertices
        .iter()
        .map(|vertex| Vertex {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
        })
        .collect();
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
    model.vertices = Vec::new();
    Ok(ModelGpu {
        model,
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

/// `look` ready to draw; the textures that could not be read drawn white, said once each in
/// `refused`.
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
    let data = &model.model;
    let mut held: Vec<Arc<TextureGpu>> = Vec::new();
    let mut bytes = 0;
    // The texture of the model `index` on the GPU, with how it wraps: a file, one its display
    // fills, or none for white.
    let mut texture_of = |index: usize, refused: &mut Vec<String>| -> Bound {
        let file = match &data.textures[index].source {
            ModelTextureSource::File(file) => file.clone(),
            ModelTextureSource::Filled(kind) => look
                .textures
                .iter()
                .find(|(filled, _)| filled == kind)
                .map(|(_, file)| file.clone())?,
            ModelTextureSource::Unnamed => return None,
        };
        match caches.textures.get(&key(&file), || texture(shared, formats, &file)) {
            Ok(texture) => {
                if !held.iter().any(|kept| Arc::ptr_eq(kept, &texture)) {
                    held.push(texture.clone());
                }
                Some((texture, (data.textures[index].flags & 3) as usize))
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
    let skins = data
        .skins
        .iter()
        .map(|skin| {
            let ids: Vec<u16> = skin.submeshes.iter().map(|submesh| submesh.id).collect();
            let shown = shown(&ids, look.geosets);
            let shaders = shaders::select(data, skin);
            let mut batches: Vec<(i8, usize, BatchGpu)> = Vec::new();
            for (order, (batch, shader)) in skin.batches.iter().zip(shaders).enumerate() {
                let colour = colour(data, batch);
                // A layer merged into its first is drawn by it.
                let Some(shader) = shader else {
                    continue;
                };
                if !shown[usize::from(batch.submesh)] || colour[3] <= 0.0 {
                    continue;
                }
                let submesh = &skin.submeshes[usize::from(batch.submesh)];
                let material = &data.materials[usize::from(batch.material)];
                let state = State::of(material);
                let [one, two] =
                    [0, 1].map(|slot| shader.textures.get(slot).and_then(|index| texture_of(*index, refused)));
                let params = BatchParams {
                    colour,
                    flags: flags(material),
                    model: [data.radius, 0.0, 0.0, 0.0],
                    combine: [
                        shader.combiner as u32,
                        shader.coords[0] as u32,
                        shader.coords[1] as u32,
                        0,
                    ],
                };
                let uniform = shared.buffer("models batch", bytemuck::bytes_of(&params), wgpu::BufferUsages::UNIFORM);
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
                batches.push((
                    batch.priority,
                    order,
                    BatchGpu {
                        indices: submesh.start..submesh.start + submesh.count,
                        state,
                        pipeline: shared.pipeline(state),
                        group,
                    },
                ));
            }
            // The planes of priority first, then the order of the skin, which keeps a batch's
            // layers after it.
            batches.sort_by_key(|(priority, order, _)| (*priority, *order));
            batches.into_iter().map(|(_, _, batch)| batch).collect()
        })
        .collect();
    Ok(LookGpu {
        model,
        textures: held,
        skins,
        bytes,
    })
}
