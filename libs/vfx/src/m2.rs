//! The M2 model of the 3.3.5a client (version 264), read whole and written again from scratch, as
//! m2port's `m2.py` does it (WoW-mods, `mod-limit-break/tools/m2port`): every structure field by
//! field, the arrays laid out again in the same order and with the same alignments, so that a model
//! written here is the bytes `m2.py` writes. Its skin (`00.skin`) with it.

/// What went wrong reading a model.
#[derive(Debug, Clone, PartialEq)]
pub struct M2Error(pub String);

impl std::fmt::Display for M2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for M2Error {}

fn err<T>(text: impl Into<String>) -> Result<T, M2Error> {
    Err(M2Error(text.into()))
}

// --- values -------------------------------------------------------------------------------------

/// A value stored as it is in the file: its size, how it is written and read.
pub trait Value: Sized + Clone + PartialEq + std::fmt::Debug {
    const SIZE: usize;
    fn put(&self, out: &mut Vec<u8>);
    fn get(bytes: &[u8]) -> Self;
}

macro_rules! scalar {
    ($t:ty, $n:expr) => {
        impl Value for $t {
            const SIZE: usize = $n;
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn get(bytes: &[u8]) -> Self {
                <$t>::from_le_bytes(bytes[..$n].try_into().expect("its size"))
            }
        }
    };
}

scalar!(u8, 1);
scalar!(i8, 1);
scalar!(u16, 2);
scalar!(i16, 2);
scalar!(u32, 4);
scalar!(i32, 4);
scalar!(f32, 4);

impl<T: Value + Copy, const N: usize> Value for [T; N] {
    const SIZE: usize = T::SIZE * N;
    fn put(&self, out: &mut Vec<u8>) {
        for value in self {
            value.put(out);
        }
    }
    fn get(bytes: &[u8]) -> Self {
        std::array::from_fn(|at| T::get(&bytes[at * T::SIZE..]))
    }
}

/// An animated value: per sequence (or global sequence), its keys' times and values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track<T> {
    pub interpolation: u16,
    pub global_sequence: i16,
    pub timestamps: Vec<Vec<u32>>,
    pub values: Vec<Vec<T>>,
}

impl<T> Track<T> {
    /// No key.
    pub fn empty() -> Self {
        Self {
            interpolation: 0,
            global_sequence: -1,
            timestamps: Vec::new(),
            values: Vec::new(),
        }
    }

    /// One key at 0, never moving.
    pub fn fixed(value: T) -> Self {
        Self {
            interpolation: 0,
            global_sequence: -1,
            timestamps: vec![vec![0]],
            values: vec![vec![value]],
        }
    }

    /// The keys of one sequence, `interpolation` between them.
    pub fn keyed(interpolation: u16, times: Vec<u32>, values: Vec<T>) -> Self {
        Self {
            interpolation,
            global_sequence: -1,
            timestamps: vec![times],
            values: vec![values],
        }
    }
}

/// A track without values (the `enabled` of an event).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackBase {
    pub interpolation: u16,
    pub global_sequence: i16,
    pub timestamps: Vec<Vec<u32>>,
}

/// A particle's value over its life: times in fixed16 (0 to 32767), values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FBlock<T> {
    pub timestamps: Vec<i16>,
    pub values: Vec<T>,
}

// --- structures ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub id: u16,
    pub variation: u16,
    pub duration: u32,
    pub move_speed: f32,
    pub flags: u32,
    pub frequency: i16,
    pub padding: u16,
    pub replay_min: u32,
    pub replay_max: u32,
    pub blend_time: u32,
    pub bounds: [u8; 28],
    pub variation_next: i16,
    pub alias_next: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub key_bone_id: i32,
    pub flags: u32,
    pub parent: i16,
    pub submesh_id: u16,
    pub crc: u32,
    pub translation: Track<[f32; 3]>,
    pub rotation: Track<[i16; 4]>,
    pub scale: Track<[f32; 3]>,
    pub pivot: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Color {
    pub color: Track<[f32; 3]>,
    pub alpha: Track<i16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub kind: u32,
    pub flags: u32,
    /// Its path in the game, ended by a zero.
    pub filename: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weight {
    pub weight: Track<i16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextureTransform {
    pub translation: Track<[f32; 3]>,
    pub rotation: Track<[f32; 4]>,
    pub scaling: Track<[f32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub flags: u16,
    pub blend: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub id: u32,
    pub bone: u16,
    pub unknown: u16,
    pub position: [f32; 3],
    pub animate_attached: Track<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub identifier: u32,
    pub data: u32,
    pub bone: u32,
    pub position: [f32; 3],
    pub enabled: TrackBase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub kind: u16,
    pub bone: i16,
    pub position: [f32; 3],
    pub ambient_color: Track<[f32; 3]>,
    pub ambient_intensity: Track<f32>,
    pub diffuse_color: Track<[f32; 3]>,
    pub diffuse_intensity: Track<f32>,
    pub attenuation_start: Track<f32>,
    pub attenuation_end: Track<f32>,
    pub visibility: Track<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    pub kind: u32,
    pub fov: f32,
    pub far_clip: f32,
    pub near_clip: f32,
    pub positions: Track<[f32; 9]>,
    pub position_base: [f32; 3],
    pub target_positions: Track<[f32; 9]>,
    pub target_base: [f32; 3],
    pub roll: Track<[f32; 3]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    pub id: i32,
    pub bone: u32,
    pub position: [f32; 3],
    pub textures: Vec<u16>,
    pub materials: Vec<u16>,
    pub color: Track<[f32; 3]>,
    pub alpha: Track<i16>,
    pub height_above: Track<f32>,
    pub height_below: Track<f32>,
    pub edges_per_second: f32,
    pub edge_lifetime: f32,
    pub gravity: f32,
    pub texture_rows: u16,
    pub texture_cols: u16,
    pub tex_slot: Track<u16>,
    pub visibility: Track<u8>,
    pub priority_plane: i16,
    pub color_index: u8,
    pub transform_lookup: u8,
}

/// A particle emitter. Its gravity, when its flags hold 0x800000 (compressed), keeps its bits as
/// `f32::from_bits`.
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    pub id: i32,
    pub flags: u32,
    pub position: [f32; 3],
    pub bone: u16,
    pub texture: u16,
    pub geometry_model: Vec<u8>,
    pub recursion_model: Vec<u8>,
    pub blend: u8,
    pub emitter_type: u8,
    pub color_index: u16,
    pub particle_type: u8,
    pub head_or_tail: u8,
    pub tile_rotation: i16,
    pub rows: u16,
    pub cols: u16,
    pub emission_speed: Track<f32>,
    pub speed_variation: Track<f32>,
    pub vertical_range: Track<f32>,
    pub horizontal_range: Track<f32>,
    pub gravity: Track<f32>,
    pub lifespan: Track<f32>,
    pub lifespan_vary: f32,
    pub emission_rate: Track<f32>,
    pub emission_rate_vary: f32,
    pub area_length: Track<f32>,
    pub area_width: Track<f32>,
    pub z_source: Track<f32>,
    pub color_track: FBlock<[f32; 3]>,
    pub alpha_track: FBlock<i16>,
    pub scale_track: FBlock<[f32; 2]>,
    pub scale_vary: [f32; 2],
    pub head_cell: FBlock<u16>,
    pub tail_cell: FBlock<u16>,
    pub tail_length: f32,
    pub twinkle_speed: f32,
    pub twinkle_percent: f32,
    pub twinkle_scale: [f32; 2],
    pub burst_multiplier: f32,
    pub drag: f32,
    pub base_spin: f32,
    pub base_spin_vary: f32,
    pub spin: f32,
    pub spin_vary: f32,
    pub tumble: [u8; 24],
    pub wind: [f32; 3],
    pub wind_time: f32,
    pub follow_speed1: f32,
    pub follow_scale1: f32,
    pub follow_speed2: f32,
    pub follow_scale2: f32,
    pub spline_points: Vec<[f32; 3]>,
    pub enabled: Track<u8>,
}

/// A whole model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub version: u32,
    /// Its name, ended by a zero.
    pub name: Vec<u8>,
    pub flags: u32,
    pub num_skin: u32,
    pub boxes: Vec<u8>,
    pub global_loops: Vec<u32>,
    pub sequences: Vec<Sequence>,
    pub sequence_lookups: Vec<i16>,
    pub bones: Vec<Bone>,
    pub key_bone_lookup: Vec<i16>,
    pub vertices: Vec<[u8; 48]>,
    pub colors: Vec<Color>,
    pub textures: Vec<Texture>,
    pub texture_weights: Vec<Weight>,
    pub texture_transforms: Vec<TextureTransform>,
    pub replaceable_texture_lookup: Vec<i16>,
    pub materials: Vec<Material>,
    pub bone_lookup: Vec<i16>,
    pub texture_lookup: Vec<i16>,
    pub texture_unit_lookup: Vec<i16>,
    pub transparency_lookup: Vec<i16>,
    pub texture_transform_lookup: Vec<i16>,
    pub collision_indices: Vec<u16>,
    pub collision_positions: Vec<[f32; 3]>,
    pub collision_normals: Vec<[f32; 3]>,
    pub attachments: Vec<Attachment>,
    pub attachment_lookup: Vec<i16>,
    pub events: Vec<Event>,
    pub lights: Vec<Light>,
    pub cameras: Vec<Camera>,
    pub camera_lookup: Vec<i16>,
    pub ribbons: Vec<Ribbon>,
    pub particles: Vec<Particle>,
    pub texture_combiner_combos: Vec<u16>,
}

/// The header flag telling the texture combiner combos are written.
pub const COMBINER_FLAG: u32 = 0x08;
/// The sequence flag telling its keys are in the model.
pub const EMBEDDED: u32 = 0x20;

/// The size of each structure in the file.
pub const SEQUENCE_SIZE: usize = 64;
pub const BONE_SIZE: usize = 88;
pub const RIBBON_SIZE: usize = 176;
pub const PARTICLE_SIZE: usize = 476;
pub const CAMERA_SIZE: usize = 100;
pub const LIGHT_SIZE: usize = 156;

// --- writing ------------------------------------------------------------------------------------

/// The bytes being laid out: each array placed as it comes, 16-aligned (4-aligned for values of
/// fewer than 4 bytes), as `m2.py`'s `Writer`.
struct Writer {
    d: Vec<u8>,
}

impl Writer {
    fn align(&mut self, n: usize) {
        while !self.d.len().is_multiple_of(n) {
            self.d.push(0);
        }
    }

    fn put(&mut self, data: &[u8], align: usize) -> u32 {
        if data.is_empty() {
            return 0;
        }
        self.align(align);
        let at = self.d.len();
        self.d.extend_from_slice(data);
        at as u32
    }

    /// An array of plain values: (count, offset).
    fn values<T: Value>(&mut self, items: &[T]) -> (u32, u32) {
        if items.is_empty() {
            return (0, 0);
        }
        let mut data = Vec::with_capacity(items.len() * T::SIZE);
        for item in items {
            item.put(&mut data);
        }
        let align = if T::SIZE < 4 { 4 } else { 16 };
        (items.len() as u32, self.put(&data, align))
    }

    /// An array of structures: their room first, then each record and what it refers to.
    fn records<R>(&mut self, items: &[R], size: usize, record: impl Fn(&mut Self, &R) -> Vec<u8>) -> (u32, u32) {
        if items.is_empty() {
            return (0, 0);
        }
        let at = self.put(&vec![0; size * items.len()], 16) as usize;
        for (index, item) in items.iter().enumerate() {
            let bytes = record(self, item);
            debug_assert_eq!(bytes.len(), size);
            self.d[at + index * size..at + (index + 1) * size].copy_from_slice(&bytes);
        }
        (items.len() as u32, at as u32)
    }

    /// M2Array<M2Array<T>>: the inner arrays, then the outer one.
    fn nested<T: Value>(&mut self, lists: &[Vec<T>]) -> (u32, u32) {
        if lists.is_empty() {
            return (0, 0);
        }
        let refs: Vec<(u32, u32)> = lists
            .iter()
            .map(|values| if values.is_empty() { (0, 0) } else { self.values(values) })
            .collect();
        let mut data = Vec::with_capacity(refs.len() * 8);
        for (count, at) in &refs {
            data.extend_from_slice(&count.to_le_bytes());
            data.extend_from_slice(&at.to_le_bytes());
        }
        (refs.len() as u32, self.put(&data, 16))
    }

    fn track<T: Value>(&mut self, out: &mut Vec<u8>, track: &Track<T>) {
        let (tc, to) = self.nested(&track.timestamps);
        let (vc, vo) = self.nested(&track.values);
        out.extend_from_slice(&track.interpolation.to_le_bytes());
        out.extend_from_slice(&track.global_sequence.to_le_bytes());
        for value in [tc, to, vc, vo] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn trackbase(&mut self, out: &mut Vec<u8>, track: &TrackBase) {
        let (tc, to) = self.nested(&track.timestamps);
        out.extend_from_slice(&track.interpolation.to_le_bytes());
        out.extend_from_slice(&track.global_sequence.to_le_bytes());
        out.extend_from_slice(&tc.to_le_bytes());
        out.extend_from_slice(&to.to_le_bytes());
    }

    fn fblock<T: Value>(&mut self, out: &mut Vec<u8>, block: &FBlock<T>) {
        let (tc, to) = self.values(&block.timestamps);
        let (vc, vo) = self.values(&block.values);
        for value in [tc, to, vc, vo] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn array<T: Value>(&mut self, out: &mut Vec<u8>, items: &[T]) {
        let (count, at) = self.values(items);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
    }
}

fn sequence_bytes(s: &Sequence) -> Vec<u8> {
    let mut o = Vec::with_capacity(SEQUENCE_SIZE);
    s.id.put(&mut o);
    s.variation.put(&mut o);
    s.duration.put(&mut o);
    s.move_speed.put(&mut o);
    s.flags.put(&mut o);
    s.frequency.put(&mut o);
    s.padding.put(&mut o);
    s.replay_min.put(&mut o);
    s.replay_max.put(&mut o);
    s.blend_time.put(&mut o);
    o.extend_from_slice(&s.bounds);
    s.variation_next.put(&mut o);
    s.alias_next.put(&mut o);
    o
}

fn bone_bytes(w: &mut Writer, b: &Bone) -> Vec<u8> {
    let mut o = Vec::with_capacity(BONE_SIZE);
    b.key_bone_id.put(&mut o);
    b.flags.put(&mut o);
    b.parent.put(&mut o);
    b.submesh_id.put(&mut o);
    b.crc.put(&mut o);
    w.track(&mut o, &b.translation);
    w.track(&mut o, &b.rotation);
    w.track(&mut o, &b.scale);
    b.pivot.put(&mut o);
    o
}

fn color_bytes(w: &mut Writer, c: &Color) -> Vec<u8> {
    let mut o = Vec::with_capacity(40);
    w.track(&mut o, &c.color);
    w.track(&mut o, &c.alpha);
    o
}

fn texture_bytes(w: &mut Writer, t: &Texture) -> Vec<u8> {
    let mut o = Vec::with_capacity(16);
    t.kind.put(&mut o);
    t.flags.put(&mut o);
    w.array(&mut o, &t.filename);
    o
}

fn weight_bytes(w: &mut Writer, t: &Weight) -> Vec<u8> {
    let mut o = Vec::with_capacity(20);
    w.track(&mut o, &t.weight);
    o
}

fn transform_bytes(w: &mut Writer, t: &TextureTransform) -> Vec<u8> {
    let mut o = Vec::with_capacity(60);
    w.track(&mut o, &t.translation);
    w.track(&mut o, &t.rotation);
    w.track(&mut o, &t.scaling);
    o
}

fn material_bytes(m: &Material) -> Vec<u8> {
    let mut o = Vec::with_capacity(4);
    m.flags.put(&mut o);
    m.blend.put(&mut o);
    o
}

fn attachment_bytes(w: &mut Writer, a: &Attachment) -> Vec<u8> {
    let mut o = Vec::with_capacity(40);
    a.id.put(&mut o);
    a.bone.put(&mut o);
    a.unknown.put(&mut o);
    a.position.put(&mut o);
    w.track(&mut o, &a.animate_attached);
    o
}

fn event_bytes(w: &mut Writer, e: &Event) -> Vec<u8> {
    let mut o = Vec::with_capacity(36);
    e.identifier.put(&mut o);
    e.data.put(&mut o);
    e.bone.put(&mut o);
    e.position.put(&mut o);
    w.trackbase(&mut o, &e.enabled);
    o
}

fn light_bytes(w: &mut Writer, l: &Light) -> Vec<u8> {
    let mut o = Vec::with_capacity(LIGHT_SIZE);
    l.kind.put(&mut o);
    l.bone.put(&mut o);
    l.position.put(&mut o);
    w.track(&mut o, &l.ambient_color);
    w.track(&mut o, &l.ambient_intensity);
    w.track(&mut o, &l.diffuse_color);
    w.track(&mut o, &l.diffuse_intensity);
    w.track(&mut o, &l.attenuation_start);
    w.track(&mut o, &l.attenuation_end);
    w.track(&mut o, &l.visibility);
    o
}

fn camera_bytes(w: &mut Writer, c: &Camera) -> Vec<u8> {
    let mut o = Vec::with_capacity(CAMERA_SIZE);
    c.kind.put(&mut o);
    c.fov.put(&mut o);
    c.far_clip.put(&mut o);
    c.near_clip.put(&mut o);
    w.track(&mut o, &c.positions);
    c.position_base.put(&mut o);
    w.track(&mut o, &c.target_positions);
    c.target_base.put(&mut o);
    w.track(&mut o, &c.roll);
    o
}

fn ribbon_bytes(w: &mut Writer, r: &Ribbon) -> Vec<u8> {
    let mut o = Vec::with_capacity(RIBBON_SIZE);
    r.id.put(&mut o);
    r.bone.put(&mut o);
    r.position.put(&mut o);
    w.array(&mut o, &r.textures);
    w.array(&mut o, &r.materials);
    w.track(&mut o, &r.color);
    w.track(&mut o, &r.alpha);
    w.track(&mut o, &r.height_above);
    w.track(&mut o, &r.height_below);
    r.edges_per_second.put(&mut o);
    r.edge_lifetime.put(&mut o);
    r.gravity.put(&mut o);
    r.texture_rows.put(&mut o);
    r.texture_cols.put(&mut o);
    w.track(&mut o, &r.tex_slot);
    w.track(&mut o, &r.visibility);
    r.priority_plane.put(&mut o);
    r.color_index.put(&mut o);
    r.transform_lookup.put(&mut o);
    o
}

fn particle_bytes(w: &mut Writer, p: &Particle) -> Vec<u8> {
    let mut o = Vec::with_capacity(PARTICLE_SIZE);
    p.id.put(&mut o);
    p.flags.put(&mut o);
    p.position.put(&mut o);
    p.bone.put(&mut o);
    p.texture.put(&mut o);
    w.array(&mut o, &p.geometry_model);
    w.array(&mut o, &p.recursion_model);
    p.blend.put(&mut o);
    p.emitter_type.put(&mut o);
    p.color_index.put(&mut o);
    p.particle_type.put(&mut o);
    p.head_or_tail.put(&mut o);
    p.tile_rotation.put(&mut o);
    p.rows.put(&mut o);
    p.cols.put(&mut o);
    w.track(&mut o, &p.emission_speed);
    w.track(&mut o, &p.speed_variation);
    w.track(&mut o, &p.vertical_range);
    w.track(&mut o, &p.horizontal_range);
    w.track(&mut o, &p.gravity);
    w.track(&mut o, &p.lifespan);
    p.lifespan_vary.put(&mut o);
    w.track(&mut o, &p.emission_rate);
    p.emission_rate_vary.put(&mut o);
    w.track(&mut o, &p.area_length);
    w.track(&mut o, &p.area_width);
    w.track(&mut o, &p.z_source);
    w.fblock(&mut o, &p.color_track);
    w.fblock(&mut o, &p.alpha_track);
    w.fblock(&mut o, &p.scale_track);
    p.scale_vary.put(&mut o);
    w.fblock(&mut o, &p.head_cell);
    w.fblock(&mut o, &p.tail_cell);
    p.tail_length.put(&mut o);
    p.twinkle_speed.put(&mut o);
    p.twinkle_percent.put(&mut o);
    p.twinkle_scale.put(&mut o);
    p.burst_multiplier.put(&mut o);
    p.drag.put(&mut o);
    p.base_spin.put(&mut o);
    p.base_spin_vary.put(&mut o);
    p.spin.put(&mut o);
    p.spin_vary.put(&mut o);
    o.extend_from_slice(&p.tumble);
    p.wind.put(&mut o);
    p.wind_time.put(&mut o);
    p.follow_speed1.put(&mut o);
    p.follow_scale1.put(&mut o);
    p.follow_speed2.put(&mut o);
    p.follow_scale2.put(&mut o);
    w.array(&mut o, &p.spline_points);
    w.track(&mut o, &p.enabled);
    o
}

/// The size of the header: name, flags, the arrays before and after the count of skins, the boxes,
/// the arrays after them, the combos when the flags say so.
fn header_size(flags: u32) -> usize {
    8 + 8 + 4 + 8 * 6 + 4 + 8 * 11 + 56 + 8 * 11 + if flags & COMBINER_FLAG != 0 { 8 } else { 0 }
}

/// The model as a version 264 file.
pub fn write(m: &Model) -> Vec<u8> {
    let size = header_size(m.flags);
    let mut w = Writer { d: vec![0; size] };
    let mut name = m.name.clone();
    if name.last() != Some(&0) {
        name.push(0);
    }
    let name_ref = w.values(&name);
    // In the order of the header, each array and what it refers to.
    let global_loops = w.values(&m.global_loops);
    let sequences = w.records(&m.sequences, SEQUENCE_SIZE, |_, s| sequence_bytes(s));
    let sequence_lookups = w.values(&m.sequence_lookups);
    let bones = w.records(&m.bones, BONE_SIZE, bone_bytes);
    let key_bone_lookup = w.values(&m.key_bone_lookup);
    let vertices = w.values(&m.vertices);
    let colors = w.records(&m.colors, 40, color_bytes);
    let textures = w.records(&m.textures, 16, texture_bytes);
    let texture_weights = w.records(&m.texture_weights, 20, weight_bytes);
    let texture_transforms = w.records(&m.texture_transforms, 60, transform_bytes);
    let replaceable = w.values(&m.replaceable_texture_lookup);
    let materials = w.records(&m.materials, 4, |_, m| material_bytes(m));
    let bone_lookup = w.values(&m.bone_lookup);
    let texture_lookup = w.values(&m.texture_lookup);
    let unit_lookup = w.values(&m.texture_unit_lookup);
    let transparency_lookup = w.values(&m.transparency_lookup);
    let transform_lookup = w.values(&m.texture_transform_lookup);
    let collision_indices = w.values(&m.collision_indices);
    let collision_positions = w.values(&m.collision_positions);
    let collision_normals = w.values(&m.collision_normals);
    let attachments = w.records(&m.attachments, 40, attachment_bytes);
    let attachment_lookup = w.values(&m.attachment_lookup);
    let events = w.records(&m.events, 36, event_bytes);
    let lights = w.records(&m.lights, LIGHT_SIZE, light_bytes);
    let cameras = w.records(&m.cameras, CAMERA_SIZE, camera_bytes);
    let camera_lookup = w.values(&m.camera_lookup);
    let ribbons = w.records(&m.ribbons, RIBBON_SIZE, ribbon_bytes);
    let particles = w.records(&m.particles, PARTICLE_SIZE, particle_bytes);
    let combos = if m.flags & COMBINER_FLAG != 0 {
        w.values(&m.texture_combiner_combos)
    } else {
        (0, 0)
    };

    let mut h = Vec::with_capacity(size);
    h.extend_from_slice(b"MD20");
    264u32.put(&mut h);
    let pair = |h: &mut Vec<u8>, (count, at): (u32, u32)| {
        count.put(h);
        at.put(h);
    };
    pair(&mut h, name_ref);
    m.flags.put(&mut h);
    for r in [
        global_loops,
        sequences,
        sequence_lookups,
        bones,
        key_bone_lookup,
        vertices,
    ] {
        pair(&mut h, r);
    }
    m.num_skin.put(&mut h);
    for r in [
        colors,
        textures,
        texture_weights,
        texture_transforms,
        replaceable,
        materials,
        bone_lookup,
        texture_lookup,
        unit_lookup,
        transparency_lookup,
        transform_lookup,
    ] {
        pair(&mut h, r);
    }
    let mut boxes = m.boxes.clone();
    boxes.resize(56, 0);
    h.extend_from_slice(&boxes);
    for r in [
        collision_indices,
        collision_positions,
        collision_normals,
        attachments,
        attachment_lookup,
        events,
        lights,
        cameras,
        camera_lookup,
        ribbons,
        particles,
    ] {
        pair(&mut h, r);
    }
    if m.flags & COMBINER_FLAG != 0 {
        pair(&mut h, combos);
    }
    debug_assert_eq!(h.len(), size);
    w.d[..size].copy_from_slice(&h);
    w.d
}

// --- reading ------------------------------------------------------------------------------------

/// Reads one blob, every array checked against its size.
struct Reader<'a> {
    d: &'a [u8],
    /// The sequences whose keys are in .anim files: their key sets are left empty.
    outside: Vec<bool>,
}

impl Reader<'_> {
    fn u32_at(&self, at: usize) -> Result<u32, M2Error> {
        self.d
            .get(at..at + 4)
            .map(u32::get)
            .ok_or_else(|| M2Error(format!("a number past the end at {at}")))
    }

    fn check(&self, count: u32, at: u32, size: usize, what: &str) -> Result<(), M2Error> {
        if count > 0 && at as usize + count as usize * size > self.d.len() {
            return err(format!(
                "{what}: {count} x {size} at {at} past the end ({})",
                self.d.len()
            ));
        }
        Ok(())
    }

    fn values<T: Value>(&self, count: u32, at: u32, what: &str) -> Result<Vec<T>, M2Error> {
        self.check(count, at, T::SIZE, what)?;
        Ok((0..count as usize)
            .map(|index| T::get(&self.d[at as usize + index * T::SIZE..]))
            .collect())
    }

    fn array<T: Value>(&self, p: &mut usize, what: &str) -> Result<Vec<T>, M2Error> {
        let (count, at) = (self.u32_at(*p)?, self.u32_at(*p + 4)?);
        *p += 8;
        self.values(count, at, what)
    }

    /// The key sets of a track: those of a sequence whose keys are outside the model left empty.
    fn keys<T: Value>(&self, count: u32, at: u32, gs: i16, what: &str) -> Result<Vec<Vec<T>>, M2Error> {
        self.check(count, at, 8, what)?;
        (0..count as usize)
            .map(|index| {
                if gs < 0 && self.outside.get(index).copied().unwrap_or(false) {
                    return Ok(Vec::new());
                }
                let ofs = at as usize + index * 8;
                self.values(self.u32_at(ofs)?, self.u32_at(ofs + 4)?, what)
            })
            .collect()
    }

    fn track<T: Value>(&self, p: &mut usize, what: &str) -> Result<Track<T>, M2Error> {
        let at = *p;
        *p += 20;
        let head = self
            .d
            .get(at..at + 20)
            .ok_or_else(|| M2Error(format!("{what} past the end")))?;
        Ok(Track {
            interpolation: u16::get(head),
            global_sequence: i16::get(&head[2..]),
            timestamps: self.keys(u32::get(&head[4..]), u32::get(&head[8..]), i16::get(&head[2..]), what)?,
            values: self.keys(u32::get(&head[12..]), u32::get(&head[16..]), i16::get(&head[2..]), what)?,
        })
    }

    fn trackbase(&self, p: &mut usize, what: &str) -> Result<TrackBase, M2Error> {
        let at = *p;
        *p += 12;
        let head = self
            .d
            .get(at..at + 12)
            .ok_or_else(|| M2Error(format!("{what} past the end")))?;
        Ok(TrackBase {
            interpolation: u16::get(head),
            global_sequence: i16::get(&head[2..]),
            timestamps: self.keys(u32::get(&head[4..]), u32::get(&head[8..]), i16::get(&head[2..]), what)?,
        })
    }

    fn fblock<T: Value>(&self, p: &mut usize, what: &str) -> Result<FBlock<T>, M2Error> {
        let at = *p;
        *p += 16;
        let (tc, to, vc, vo) = (
            self.u32_at(at)?,
            self.u32_at(at + 4)?,
            self.u32_at(at + 8)?,
            self.u32_at(at + 12)?,
        );
        Ok(FBlock {
            timestamps: self.values(tc, to, what)?,
            values: self.values(vc, vo, what)?,
        })
    }

    fn value<T: Value>(&self, p: &mut usize) -> Result<T, M2Error> {
        let bytes = self
            .d
            .get(*p..*p + T::SIZE)
            .ok_or_else(|| M2Error(format!("a value past the end at {p}")))?;
        *p += T::SIZE;
        Ok(T::get(bytes))
    }

    fn records<R>(
        &self,
        p: &mut usize,
        size: usize,
        what: &str,
        record: impl Fn(&Self, usize) -> Result<R, M2Error>,
    ) -> Result<Vec<R>, M2Error> {
        let (count, at) = (self.u32_at(*p)?, self.u32_at(*p + 4)?);
        *p += 8;
        self.check(count, at, size, what)?;
        (0..count as usize)
            .map(|index| record(self, at as usize + index * size))
            .collect()
    }
}

fn read_sequence(r: &Reader, at: usize) -> Result<Sequence, M2Error> {
    let mut p = at;
    Ok(Sequence {
        id: r.value(&mut p)?,
        variation: r.value(&mut p)?,
        duration: r.value(&mut p)?,
        move_speed: r.value(&mut p)?,
        flags: r.value(&mut p)?,
        frequency: r.value(&mut p)?,
        padding: r.value(&mut p)?,
        replay_min: r.value(&mut p)?,
        replay_max: r.value(&mut p)?,
        blend_time: r.value(&mut p)?,
        bounds: r.value(&mut p)?,
        variation_next: r.value(&mut p)?,
        alias_next: r.value(&mut p)?,
    })
}

fn read_bone(r: &Reader, at: usize) -> Result<Bone, M2Error> {
    let mut p = at;
    Ok(Bone {
        key_bone_id: r.value(&mut p)?,
        flags: r.value(&mut p)?,
        parent: r.value(&mut p)?,
        submesh_id: r.value(&mut p)?,
        crc: r.value(&mut p)?,
        translation: r.track(&mut p, "bone translation")?,
        rotation: r.track(&mut p, "bone rotation")?,
        scale: r.track(&mut p, "bone scale")?,
        pivot: r.value(&mut p)?,
    })
}

fn read_particle(r: &Reader, at: usize) -> Result<Particle, M2Error> {
    let mut p = at;
    let id = r.value(&mut p)?;
    let flags: u32 = r.value(&mut p)?;
    Ok(Particle {
        id,
        flags,
        position: r.value(&mut p)?,
        bone: r.value(&mut p)?,
        texture: r.value(&mut p)?,
        geometry_model: r.array(&mut p, "geometry model")?,
        recursion_model: r.array(&mut p, "recursion model")?,
        blend: r.value(&mut p)?,
        emitter_type: r.value(&mut p)?,
        color_index: r.value(&mut p)?,
        particle_type: r.value(&mut p)?,
        head_or_tail: r.value(&mut p)?,
        tile_rotation: r.value(&mut p)?,
        rows: r.value(&mut p)?,
        cols: r.value(&mut p)?,
        emission_speed: r.track(&mut p, "emission speed")?,
        speed_variation: r.track(&mut p, "speed variation")?,
        vertical_range: r.track(&mut p, "vertical range")?,
        horizontal_range: r.track(&mut p, "horizontal range")?,
        gravity: r.track(&mut p, "gravity")?,
        lifespan: r.track(&mut p, "lifespan")?,
        lifespan_vary: r.value(&mut p)?,
        emission_rate: r.track(&mut p, "emission rate")?,
        emission_rate_vary: r.value(&mut p)?,
        area_length: r.track(&mut p, "area length")?,
        area_width: r.track(&mut p, "area width")?,
        z_source: r.track(&mut p, "z source")?,
        color_track: r.fblock(&mut p, "colour")?,
        alpha_track: r.fblock(&mut p, "alpha")?,
        scale_track: r.fblock(&mut p, "scale")?,
        scale_vary: r.value(&mut p)?,
        head_cell: r.fblock(&mut p, "head cell")?,
        tail_cell: r.fblock(&mut p, "tail cell")?,
        tail_length: r.value(&mut p)?,
        twinkle_speed: r.value(&mut p)?,
        twinkle_percent: r.value(&mut p)?,
        twinkle_scale: r.value(&mut p)?,
        burst_multiplier: r.value(&mut p)?,
        drag: r.value(&mut p)?,
        base_spin: r.value(&mut p)?,
        base_spin_vary: r.value(&mut p)?,
        spin: r.value(&mut p)?,
        spin_vary: r.value(&mut p)?,
        tumble: r.value(&mut p)?,
        wind: r.value(&mut p)?,
        wind_time: r.value(&mut p)?,
        follow_speed1: r.value(&mut p)?,
        follow_scale1: r.value(&mut p)?,
        follow_speed2: r.value(&mut p)?,
        follow_scale2: r.value(&mut p)?,
        spline_points: r.array(&mut p, "spline points")?,
        enabled: r.track(&mut p, "enabled")?,
    })
}

fn read_ribbon(r: &Reader, at: usize) -> Result<Ribbon, M2Error> {
    let mut p = at;
    Ok(Ribbon {
        id: r.value(&mut p)?,
        bone: r.value(&mut p)?,
        position: r.value(&mut p)?,
        textures: r.array(&mut p, "ribbon textures")?,
        materials: r.array(&mut p, "ribbon materials")?,
        color: r.track(&mut p, "ribbon colour")?,
        alpha: r.track(&mut p, "ribbon alpha")?,
        height_above: r.track(&mut p, "height above")?,
        height_below: r.track(&mut p, "height below")?,
        edges_per_second: r.value(&mut p)?,
        edge_lifetime: r.value(&mut p)?,
        gravity: r.value(&mut p)?,
        texture_rows: r.value(&mut p)?,
        texture_cols: r.value(&mut p)?,
        tex_slot: r.track(&mut p, "texture slot")?,
        visibility: r.track(&mut p, "visibility")?,
        priority_plane: r.value(&mut p)?,
        color_index: r.value(&mut p)?,
        transform_lookup: r.value(&mut p)?,
    })
}

/// A model from its file, version 264 (a 3.3.5a client's model or one written here); refused when a
/// sequence's keys are in an .anim file.
pub fn read(d: &[u8]) -> Result<Model, M2Error> {
    read_model(d, false)
}

/// A model from its file, the keys of its sequences in .anim files left out (a character's, read
/// for its bones and attachment points at rest or in Stand).
pub fn read_without_anims(d: &[u8]) -> Result<Model, M2Error> {
    read_model(d, true)
}

fn read_model(d: &[u8], lenient: bool) -> Result<Model, M2Error> {
    if d.get(..4) != Some(b"MD20".as_slice()) {
        return err("no MD20 magic");
    }
    let mut r = Reader { d, outside: Vec::new() };
    let version = r.u32_at(4)?;
    if version != 264 {
        return err(format!("version {version}: 264 alone is read here"));
    }
    let mut p = 8;
    let name = r.array(&mut p, "name")?;
    let flags = r.value(&mut p)?;
    let global_loops = r.array(&mut p, "global loops")?;
    let sequences = r.records(&mut p, SEQUENCE_SIZE, "sequences", read_sequence)?;
    // Every sequence's keys must be in the model: those in .anim files are not read here.
    r.outside = sequences.iter().map(|s| s.flags & EMBEDDED == 0).collect();
    if let Some(outside) = sequences.iter().find(|s| s.flags & EMBEDDED == 0 && !lenient) {
        return err(format!(
            "sequence {}: keys outside the model (an .anim file)",
            outside.id
        ));
    }
    let sequence_lookups = r.array(&mut p, "sequence lookups")?;
    let bones = r.records(&mut p, BONE_SIZE, "bones", read_bone)?;
    let key_bone_lookup = r.array(&mut p, "key bone lookup")?;
    let vertices = r.array(&mut p, "vertices")?;
    let num_skin = r.value(&mut p)?;
    let colors = r.records(&mut p, 40, "colours", |r, at| {
        let mut p = at;
        Ok(Color {
            color: r.track(&mut p, "colour")?,
            alpha: r.track(&mut p, "alpha")?,
        })
    })?;
    let textures = r.records(&mut p, 16, "textures", |r, at| {
        let mut p = at;
        Ok(Texture {
            kind: r.value(&mut p)?,
            flags: r.value(&mut p)?,
            filename: r.array(&mut p, "texture name")?,
        })
    })?;
    let texture_weights = r.records(&mut p, 20, "weights", |r, at| {
        let mut p = at;
        Ok(Weight {
            weight: r.track(&mut p, "weight")?,
        })
    })?;
    let texture_transforms = r.records(&mut p, 60, "transforms", |r, at| {
        let mut p = at;
        Ok(TextureTransform {
            translation: r.track(&mut p, "translation")?,
            rotation: r.track(&mut p, "rotation")?,
            scaling: r.track(&mut p, "scaling")?,
        })
    })?;
    let replaceable_texture_lookup = r.array(&mut p, "replaceable textures")?;
    let materials = r.records(&mut p, 4, "materials", |r, at| {
        let mut p = at;
        Ok(Material {
            flags: r.value(&mut p)?,
            blend: r.value(&mut p)?,
        })
    })?;
    let bone_lookup = r.array(&mut p, "bone lookup")?;
    let texture_lookup = r.array(&mut p, "texture lookup")?;
    let texture_unit_lookup = r.array(&mut p, "texture unit lookup")?;
    let transparency_lookup = r.array(&mut p, "transparency lookup")?;
    let texture_transform_lookup = r.array(&mut p, "texture transform lookup")?;
    let boxes = d
        .get(p..p + 56)
        .ok_or_else(|| M2Error("boxes past the end".into()))?
        .to_vec();
    p += 56;
    let collision_indices = r.array(&mut p, "collision indices")?;
    let collision_positions = r.array(&mut p, "collision positions")?;
    let collision_normals = r.array(&mut p, "collision normals")?;
    let attachments = r.records(&mut p, 40, "attachments", |r, at| {
        let mut p = at;
        Ok(Attachment {
            id: r.value(&mut p)?,
            bone: r.value(&mut p)?,
            unknown: r.value(&mut p)?,
            position: r.value(&mut p)?,
            animate_attached: r.track(&mut p, "animate attached")?,
        })
    })?;
    let attachment_lookup = r.array(&mut p, "attachment lookup")?;
    let events = r.records(&mut p, 36, "events", |r, at| {
        let mut p = at;
        Ok(Event {
            identifier: r.value(&mut p)?,
            data: r.value(&mut p)?,
            bone: r.value(&mut p)?,
            position: r.value(&mut p)?,
            enabled: r.trackbase(&mut p, "event")?,
        })
    })?;
    let lights = r.records(&mut p, LIGHT_SIZE, "lights", |r, at| {
        let mut p = at;
        Ok(Light {
            kind: r.value(&mut p)?,
            bone: r.value(&mut p)?,
            position: r.value(&mut p)?,
            ambient_color: r.track(&mut p, "light")?,
            ambient_intensity: r.track(&mut p, "light")?,
            diffuse_color: r.track(&mut p, "light")?,
            diffuse_intensity: r.track(&mut p, "light")?,
            attenuation_start: r.track(&mut p, "light")?,
            attenuation_end: r.track(&mut p, "light")?,
            visibility: r.track(&mut p, "light")?,
        })
    })?;
    let cameras = r.records(&mut p, CAMERA_SIZE, "cameras", |r, at| {
        let mut p = at;
        Ok(Camera {
            kind: r.value(&mut p)?,
            fov: r.value(&mut p)?,
            far_clip: r.value(&mut p)?,
            near_clip: r.value(&mut p)?,
            positions: r.track(&mut p, "camera")?,
            position_base: r.value(&mut p)?,
            target_positions: r.track(&mut p, "camera")?,
            target_base: r.value(&mut p)?,
            roll: r.track(&mut p, "camera")?,
        })
    })?;
    let camera_lookup = r.array(&mut p, "camera lookup")?;
    let ribbons = r.records(&mut p, RIBBON_SIZE, "ribbons", read_ribbon)?;
    let particles = r.records(&mut p, PARTICLE_SIZE, "particles", read_particle)?;
    let texture_combiner_combos = if flags & COMBINER_FLAG != 0 {
        r.array(&mut p, "texture combiner combos")?
    } else {
        Vec::new()
    };
    Ok(Model {
        version,
        name,
        flags,
        num_skin,
        boxes,
        global_loops,
        sequences,
        sequence_lookups,
        bones,
        key_bone_lookup,
        vertices,
        colors,
        textures,
        texture_weights,
        texture_transforms,
        replaceable_texture_lookup,
        materials,
        bone_lookup,
        texture_lookup,
        texture_unit_lookup,
        transparency_lookup,
        texture_transform_lookup,
        collision_indices,
        collision_positions,
        collision_normals,
        attachments,
        attachment_lookup,
        events,
        lights,
        cameras,
        camera_lookup,
        ribbons,
        particles,
        texture_combiner_combos,
    })
}

// --- skins --------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Submesh {
    pub id: u16,
    pub level: u16,
    pub vertex_start: u16,
    pub vertex_count: u16,
    pub index_start: u16,
    pub index_count: u16,
    pub bone_count: u16,
    pub bone_combo: u16,
    pub bone_influences: u16,
    pub center_bone: u16,
    pub center: [f32; 3],
    pub sort_center: [f32; 3],
    pub sort_radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub flags: u8,
    pub priority: i8,
    pub shader: u16,
    pub submesh: u16,
    pub geoset: u16,
    pub color: i16,
    pub material: u16,
    pub layer: u16,
    pub texture_count: u16,
    pub texture_combo: u16,
    pub coord_combo: u16,
    pub weight_combo: u16,
    pub transform_combo: u16,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skin {
    pub vertices: Vec<u16>,
    pub indices: Vec<u16>,
    pub bones: Vec<[u8; 4]>,
    pub submeshes: Vec<Submesh>,
    pub batches: Vec<Batch>,
    pub bone_count_max: u32,
}

fn submesh_bytes(s: &Submesh, o: &mut Vec<u8>) {
    for value in [
        s.id,
        s.level,
        s.vertex_start,
        s.vertex_count,
        s.index_start,
        s.index_count,
        s.bone_count,
        s.bone_combo,
        s.bone_influences,
        s.center_bone,
    ] {
        value.put(o);
    }
    s.center.put(o);
    s.sort_center.put(o);
    s.sort_radius.put(o);
}

fn batch_bytes(b: &Batch, o: &mut Vec<u8>) {
    b.flags.put(o);
    b.priority.put(o);
    b.shader.put(o);
    b.submesh.put(o);
    b.geoset.put(o);
    b.color.put(o);
    for value in [
        b.material,
        b.layer,
        b.texture_count,
        b.texture_combo,
        b.coord_combo,
        b.weight_combo,
        b.transform_combo,
    ] {
        value.put(o);
    }
}

/// The skin as its file (`00.skin`).
pub fn write_skin(s: &Skin) -> Vec<u8> {
    let mut w = Writer { d: vec![0; 48] };
    let packed = |w: &mut Writer, count: usize, data: Vec<u8>| -> (u32, u32) {
        let at = w.put(&data, 4);
        (count as u32, if count > 0 { at } else { 0 })
    };
    let mut bytes = Vec::new();
    for v in &s.vertices {
        v.put(&mut bytes);
    }
    let vertices = packed(&mut w, s.vertices.len(), std::mem::take(&mut bytes));
    for v in &s.indices {
        v.put(&mut bytes);
    }
    let indices = packed(&mut w, s.indices.len(), std::mem::take(&mut bytes));
    for b in &s.bones {
        bytes.extend_from_slice(b);
    }
    let bones = packed(&mut w, s.bones.len(), std::mem::take(&mut bytes));
    for m in &s.submeshes {
        submesh_bytes(m, &mut bytes);
    }
    let submeshes = packed(&mut w, s.submeshes.len(), std::mem::take(&mut bytes));
    for b in &s.batches {
        batch_bytes(b, &mut bytes);
    }
    let batches = packed(&mut w, s.batches.len(), std::mem::take(&mut bytes));
    let mut head = b"SKIN".to_vec();
    for (count, at) in [vertices, indices, bones, submeshes, batches] {
        count.put(&mut head);
        at.put(&mut head);
    }
    s.bone_count_max.put(&mut head);
    w.d[..48].copy_from_slice(&head);
    w.d
}

/// A skin from its file.
pub fn read_skin(d: &[u8]) -> Result<Skin, M2Error> {
    if d.get(..4) != Some(b"SKIN".as_slice()) || d.len() < 48 {
        return err("no SKIN magic");
    }
    let refs: Vec<(usize, usize)> = (0..5)
        .map(|at| (u32::get(&d[4 + 8 * at..]) as usize, u32::get(&d[8 + 8 * at..]) as usize))
        .collect();
    for ((count, at), size) in refs.iter().zip([2, 2, 4, 48, 24]) {
        if *count > 0 && at + count * size > d.len() {
            return err("skin array past the end");
        }
    }
    let (v, i, b, m, t) = (refs[0], refs[1], refs[2], refs[3], refs[4]);
    let submesh = |at: usize| {
        let mut p = at;
        let r = Reader { d, outside: Vec::new() };
        let mut next = || -> u16 {
            let value = u16::get(&d[p..]);
            p += 2;
            value
        };
        let fields: [u16; 10] = std::array::from_fn(|_| next());
        let center = <[f32; 3]>::get(&d[p..]);
        let sort_center = <[f32; 3]>::get(&d[p + 12..]);
        let sort_radius = f32::get(&d[p + 24..]);
        let _ = r;
        Submesh {
            id: fields[0],
            level: fields[1],
            vertex_start: fields[2],
            vertex_count: fields[3],
            index_start: fields[4],
            index_count: fields[5],
            bone_count: fields[6],
            bone_combo: fields[7],
            bone_influences: fields[8],
            center_bone: fields[9],
            center,
            sort_center,
            sort_radius,
        }
    };
    let batch = |at: usize| Batch {
        flags: d[at],
        priority: d[at + 1] as i8,
        shader: u16::get(&d[at + 2..]),
        submesh: u16::get(&d[at + 4..]),
        geoset: u16::get(&d[at + 6..]),
        color: i16::get(&d[at + 8..]),
        material: u16::get(&d[at + 10..]),
        layer: u16::get(&d[at + 12..]),
        texture_count: u16::get(&d[at + 14..]),
        texture_combo: u16::get(&d[at + 16..]),
        coord_combo: u16::get(&d[at + 18..]),
        weight_combo: u16::get(&d[at + 20..]),
        transform_combo: u16::get(&d[at + 22..]),
    };
    Ok(Skin {
        vertices: (0..v.0).map(|k| u16::get(&d[v.1 + 2 * k..])).collect(),
        indices: (0..i.0).map(|k| u16::get(&d[i.1 + 2 * k..])).collect(),
        bones: (0..b.0).map(|k| <[u8; 4]>::get(&d[b.1 + 4 * k..])).collect(),
        submeshes: (0..m.0).map(|k| submesh(m.1 + 48 * k)).collect(),
        batches: (0..t.0).map(|k| batch(t.1 + 24 * k)).collect(),
        bone_count_max: u32::get(&d[44..]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_written_reads_back_as_it_was() {
        let mut model = Model {
            version: 264,
            name: b"test\0".to_vec(),
            num_skin: 1,
            boxes: vec![0; 56],
            global_loops: vec![1000],
            ..Model::default()
        };
        model.bones.push(Bone {
            key_bone_id: -1,
            flags: 0x200,
            parent: -1,
            submesh_id: 0,
            crc: 0,
            translation: Track::keyed(1, vec![0, 500], vec![[0.0; 3], [1.0, 2.0, 3.0]]),
            rotation: Track::fixed([0, 0, 0, 32767]),
            scale: Track::empty(),
            pivot: [0.5, 0.0, 0.0],
        });
        model.textures.push(Texture {
            kind: 0,
            flags: 3,
            filename: b"Spells\\X.blp\0".to_vec(),
        });
        let bytes = write(&model);
        let back = read(&bytes).unwrap();
        assert_eq!(back, model);
        assert_eq!(write(&back), bytes, "written again, the same bytes");
        let skin = Skin {
            vertices: vec![0, 1, 2],
            indices: vec![0, 1, 2],
            bones: vec![[0; 4]; 3],
            submeshes: vec![Submesh {
                vertex_count: 3,
                index_count: 3,
                ..Submesh::default()
            }],
            batches: vec![Batch {
                texture_count: 1,
                color: -1,
                ..Batch::default()
            }],
            bone_count_max: 21,
        };
        assert_eq!(read_skin(&write_skin(&skin)).unwrap(), skin);
    }
}
