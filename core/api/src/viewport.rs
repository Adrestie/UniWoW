//! Interface of the "viewport" service: a 3D view to which modules add their drawing.

use std::sync::Arc;
use std::time::Duration;

use crate::{ServiceKey, egui_wgpu, glam, wgpu};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("viewport");

pub type Handle = Arc<dyn Viewport>;

/// Shared between threads (rule T3): a job may add a layer.
pub trait Viewport: Send + Sync {
    /// Adds a drawing layer. `owner` is the id of the module adding it.
    fn add_layer(&self, owner: &str, layer: Box<dyn Layer>);

    /// Removes every layer added by `owner`.
    fn remove_layers(&self, owner: &str);

    /// Formats of the render target, for modules that build their pipelines ahead, in a job.
    fn target(&self) -> Target;

    /// Waits for the frame signal of a frame after `after`, the number of the last one seen, for
    /// `timeout` at most and never more than `MAX_FRAME_WAIT`, so that a waiting thread checks its
    /// cancellation: the frame to come, or none. The signal is given once the viewport has
    /// submitted a frame, so that what a thread writes then is drawn by the next one; it is not
    /// given while the view is not drawn.
    fn wait_frame(&self, after: u64, timeout: Duration) -> Option<Frame>;

    /// Tells the budget of the view what `owner` keeps and wants on the GPU, until it tells again
    /// or its layers are removed: what the budget allows now, with what every other layer told
    /// last. On the interface thread, as it steers its loads.
    fn tell_budget(&self, owner: &str, demand: Demand) -> Allowance;

    /// What the budget of the view allows every layer, from what they told last.
    fn allowance(&self) -> Allowance;

    /// Sets the budget of the view, in bytes; kept in the settings of the view.
    fn set_budget(&self, bytes: u64);

    /// Sets the fog every layer draws with from the next frame, as the layer that knows how far the
    /// world is drawn says: the terrain, by its reach.
    fn set_fog(&self, fog: Fog);

    /// Sets the light of the map from the next frame, as the module `owner` of the light says: the
    /// sun and the colour of the fog of the view; its distances too when it gives them, those set by
    /// `set_fog` otherwise. None takes back the light `owner` gave, as does its failure. Without it,
    /// the fixed sun (`Sun::default`) and the fog set by `set_fog`.
    fn set_light(&self, owner: &str, light: Option<MapLight>);
}

/// The light of the map at the place of the camera and the hour: its sun, the colour of its fog in
/// linear, and where its fog starts and ends, in yards, and its rate when the fog of the game is
/// drawn (`Fog`), none for the editor's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapLight {
    pub sun: Sun,
    pub fog_colour: [f32; 3],
    pub fog: Option<[f32; 3]>,
}

/// The WGSL of a colour in gamma made linear (`linear`) and back (`srgb`), which the shaders of the
/// view share.
pub const LINEAR_WGSL: &str = r"// The linear value of a value in gamma, as an sRGB target encodes it back.
fn linear(gamma: vec3<f32>) -> vec3<f32> {
    let low = gamma / 12.92;
    let high = pow((gamma + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, gamma <= vec3<f32>(0.04045));
}

// The value in gamma of a linear value, as an sRGB target encodes it.
fn srgb(value: vec3<f32>) -> vec3<f32> {
    let low = value * 12.92;
    let high = 1.055 * pow(value, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, value <= vec3<f32>(0.0031308));
}
";

/// The WGSL of the fog of the view (`fog_amount`, `fog_mix`, `beyond_fog`), which the shaders of the
/// view share, for a uniform `camera` with a perspective `view_proj`, an `eye` and a `fog` as `Fog`
/// gives them: where it starts, its middle, where it ends and its rate; with `LINEAR_WGSL`.
pub const FOG_WGSL: &str = r"// The share of the editor's fog at its middle.
const NEAR_FOG: f32 = 0.55;

// The depth of `position` along the view, by which the client fogs: the w of its place on the
// screen, the view being a perspective.
fn view_depth(position: vec3<f32>) -> f32 {
    return (camera.view_proj * vec4<f32>(position, 1.0)).w;
}

// The fog of the view: the game's when its rate is given, by the depth along the view, as the
// client draws it; the editor's otherwise, by the distance on the ground.
fn fog_amount(position: vec3<f32>) -> f32 {
    if camera.fog.w > 0.0 {
        let depth = view_depth(position);
        let left = clamp((camera.fog.z - depth) / max(camera.fog.z - camera.fog.x, 0.001), 0.0, 1.0);
        return 1.0 - pow(left, camera.fog.w);
    }
    let distance = length(position.xy - camera.eye.xy);
    return NEAR_FOG * smoothstep(camera.fog.x, camera.fog.y, distance)
        + (1.0 - NEAR_FOG) * smoothstep(camera.fog.y, camera.fog.z, distance);
}

// `colour`, linear, under the fog of the colour `fog` by `amount`: the game's mixed in gamma as the
// client draws, the colour bounded to 1 first; the editor's in linear.
fn fog_mix(colour: vec3<f32>, fog: vec3<f32>, amount: f32) -> vec3<f32> {
    if camera.fog.w > 0.0 {
        let bounded = clamp(colour, vec3<f32>(0.0), vec3<f32>(1.0));
        return linear(mix(srgb(bounded), srgb(fog), amount));
    }
    return mix(colour, fog, amount);
}

// Whether `position` lies beyond the end of the fog of the game, past which Noggit leaves out the
// models wholly beyond it.
fn beyond_fog(position: vec3<f32>) -> bool {
    return camera.fog.w > 0.0 && view_depth(position) > camera.fog.z;
}
";

/// The WGSL of the light of the view (`light`), which the shaders of the view share, for a uniform
/// `camera` with a `sun`, a `sun_colour` and an `ambient` as `Sun` gives them; with `LINEAR_WGSL`.
pub const LIGHT_WGSL: &str = r"// The light of a face of `normal`, as Noggit lights it and the client in gamma: the ambient light
// from 0.9 to 1.1 times as the face turns to the sun, plus the diffuse by the angle; made linear.
fn light(normal: vec3<f32>) -> vec3<f32> {
    let facing = clamp(dot(normalize(normal), camera.sun.xyz), 0.0, 1.0);
    return linear(camera.ambient.rgb * (0.9 + 0.2 * facing) + camera.sun_colour.rgb * facing);
}
";

/// The share of the editor's fog at its middle distance (`FOG_WGSL`).
pub const NEAR_FOG: f32 = 0.55;

/// The fog of the view and its colour, that of the sky, in linear. The editor's when `rate` is 0: by
/// the distance on the ground from the eye, none up to `start`, `NEAR_FOG` of it at `middle`, all of
/// it from `end`. The game's otherwise, as the client draws it: by the depth along the view,
/// 1 − ((end − depth) / (end − start))^rate, `middle` unused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    pub colour: [f32; 3],
    pub start: f32,
    pub middle: f32,
    pub end: f32,
    pub rate: f32,
}

impl Default for Fog {
    /// No fog within any map, until a layer sets it.
    fn default() -> Self {
        Self {
            colour: [0.36, 0.43, 0.52],
            start: 1.0e9,
            middle: 2.0e9,
            end: 4.0e9,
            rate: 0.0,
        }
    }
}

/// The light of the sun: the direction towards it, its colour on the ground (diffuse) and the light
/// everywhere (ambient), both in gamma. A face is lit, as Noggit lights it and the client in gamma,
/// by the ambient light from 0.9 to 1.1 times as the face turns to the sun, plus the diffuse by the
/// angle, made linear once, then multiplying its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub direction: [f32; 3],
    pub colour: [f32; 3],
    pub ambient: [f32; 3],
}

impl Default for Sun {
    /// The light the terrain had before the lights of the maps: as bright as it, 0.45 in linear,
    /// on a face turned away from the sun, and 1 on one facing it.
    fn default() -> Self {
        Self {
            direction: glam::Vec3::new(0.4, 0.3, 0.85).normalize().to_array(),
            colour: [0.1427; 3],
            ambient: [0.7793; 3],
        }
    }
}

/// The width of a band of distance of the budget, in yards: a quarter of a tile.
pub const BAND: f32 = 533.333_3 / 4.0;
/// The bands of the budget, up to 64 tiles from the eye; what lies beyond counts in the last.
pub const BANDS: usize = 256;
/// The share of the budget the loads fill; what is held is kept up to all of it, so that an item
/// at the edge is neither loaded nor released in turn.
pub const LOAD_SHARE: f64 = 0.9;

/// What a layer keeps on the GPU, as it tells the budget of the view: what it takes outside its
/// items, such as arrays of textures, and the bytes of its items held and of those it wants, by
/// their distance from the eye.
#[derive(Clone, Debug, PartialEq)]
pub struct Demand {
    pub fixed: u64,
    /// `BANDS` bands of `BAND` yards each.
    pub held: Vec<u64>,
    pub wanted: Vec<u64>,
}

impl Default for Demand {
    fn default() -> Self {
        Self {
            fixed: 0,
            held: vec![0; BANDS],
            wanted: vec![0; BANDS],
        }
    }
}

impl Demand {
    /// The band of an item `distance` yards from the eye.
    pub fn band(distance: f32) -> usize {
        ((distance.max(0.0) / BAND) as usize).min(BANDS - 1)
    }

    /// What it holds on the GPU in all.
    pub fn used(&self) -> u64 {
        self.fixed + self.held.iter().sum::<u64>()
    }
}

/// What the budget of the view allows, the same distances for every layer: the nearest items
/// first, whatever layer they belong to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Allowance {
    pub budget: u64,
    /// What every layer holds, as they told.
    pub used: u64,
    /// The distance from the eye, in yards, the loads of every layer fill `LOAD_SHARE` of the
    /// budget to, and the one what is held is kept to; infinite when all that is wanted fits.
    pub load: f32,
    pub keep: f32,
    /// When the budget holds fewer items than the layers want: the reach it leaves, in yards.
    pub limited: Option<f32>,
}

impl Default for Allowance {
    fn default() -> Self {
        Self {
            budget: u64::MAX,
            used: 0,
            load: f32::INFINITY,
            keep: f32::INFINITY,
            limited: None,
        }
    }
}

/// What `budget` allows the layers that told `demands`: the bands of all of them, the nearest
/// first, as far as their fixed costs and the items they want fit `LOAD_SHARE` of it, and all of it.
pub fn allow(budget: u64, demands: &[&Demand]) -> Allowance {
    let fixed: u64 = demands.iter().map(|demand| demand.fixed).sum();
    let used = demands.iter().map(|demand| demand.used()).sum();
    let wanted: Vec<u64> = (0..BANDS)
        .map(|band| {
            demands
                .iter()
                .map(|demand| demand.wanted.get(band).copied().unwrap_or(0))
                .sum()
        })
        .collect();
    let last = wanted.iter().rposition(|bytes| *bytes > 0).map_or(0, |band| band + 1);
    // The bands that fit `room`, the nearest first.
    let fitting = |room: u64| {
        let mut total = fixed;
        wanted[..last]
            .iter()
            .take_while(|bytes| {
                total += **bytes;
                total <= room
            })
            .count()
    };
    let reach = |bands: usize| {
        if bands == last {
            f32::INFINITY
        } else {
            bands as f32 * BAND
        }
    };
    let loaded = fitting((budget as f64 * LOAD_SHARE) as u64);
    let kept = fitting(budget);
    Allowance {
        budget,
        used,
        load: reach(loaded),
        keep: reach(kept),
        limited: (loaded < last).then_some(loaded as f32 * BAND),
    }
}

/// The longest a thread waits for the frame signal at a time.
pub const MAX_FRAME_WAIT: Duration = Duration::from_millis(100);

/// The frame to come, as the frame signal gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Counted from 1, the first frame drawn.
    pub number: u64,
    /// Seconds since the viewport started, as `View::time`, estimated from the frames before.
    pub time: f32,
}

/// Formats of the render target a layer draws into; its pipelines must match them.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub color_format: wgpu::TextureFormat,
    pub depth_format: wgpu::TextureFormat,
    pub sample_count: u32,
    /// How a layer's pipeline compares depths. Reverse Z: the depth is 1 at the near plane and
    /// falls towards 0 at infinity, where it is cleared; nearer is greater.
    pub depth_compare: wgpu::CompareFunction,
}

/// The camera and frame being drawn. World axes: X and Y on the ground, Z up.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub view_proj: glam::Mat4,
    /// The view alone, from the world to the camera: its first three rows are the axes of the
    /// camera, across, up and back, whatever its projection.
    pub view: glam::Mat4,
    pub eye: glam::Vec3,
    /// Size of the target in pixels.
    pub size: [u32; 2],
    /// Seconds since the viewport started.
    pub time: f32,
    pub fog: Fog,
    pub sun: Sun,
}

/// How a layer draws: in a render bundle of its own, which the viewport records and keeps by its
/// version, or in the pass of the view itself at each frame, for what a bundle cannot record, such
/// as `RenderPass::multi_draw_indexed_indirect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Drawing {
    #[default]
    Bundle,
    Pass,
}

/// When a layer is drawn among the others in each phase: the ground first, then the water, then the
/// scene. Layers of one stage are drawn in the order they were added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// The terrain and its sky.
    Ground,
    /// The liquids.
    Water,
    /// What stands on the ground, by default.
    #[default]
    Scene,
}

/// The phases a frame is drawn in, over two passes. The first draws what every layer draws opaque,
/// writing the depth, such as what it saw at the frame before; the depth it leaves is reduced to a
/// pyramid (`Pyramid`), which the layers test the rest of what they draw against between the two
/// passes (`Layer::occlude`). The second draws the opaque they found in sight so (`Revealed`), then
/// what every layer blends over it all, split at the surface of the water
/// (`liquids::Surfaces::phase`) so that a blended batch under the water is seen through it: what
/// lies beyond the surface from the eye, under it from over it and over it from under it; the
/// water; then what lies on the eye's side. A blended batch of a layer is drawn over the opaque ones
/// of every other, whatever their order. The sky of the ground begins the first blended phase, where
/// nothing opaque is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Opaque,
    Revealed,
    Beyond,
    Water,
    Near,
}

impl Phase {
    /// The phases, in the order they are drawn.
    pub const ALL: [Phase; 5] = [Phase::Opaque, Phase::Revealed, Phase::Beyond, Phase::Water, Phase::Near];

    /// Whether what is drawn in it is blended over what is drawn before.
    pub fn blended(self) -> bool {
        !matches!(self, Phase::Opaque | Phase::Revealed)
    }

    /// Whether it is drawn in the first pass, whose depth the pyramid is made of.
    pub fn first_pass(self) -> bool {
        self == Phase::Opaque
    }
}

/// The depth the first pass of a frame left, reduced to a pyramid of its farthest values (Hi-Z):
/// an `R32Float` texture of `levels` levels, the first of the size of the view, each next one half
/// the one before rounded down, as the levels of a texture are; each texel holds the least depth
/// (reverse Z: the farthest) of the 2 × 2 texels under it, and the last of a row or a column of
/// those beyond them too, so that every texel is covered. A box whose nearest depth is less than
/// the texels it covers at a level is hidden. Made again when the view changes size, which
/// `generation` counts, so that a layer keeps its bind group while it stays.
#[derive(Clone, Copy, Debug)]
pub struct Pyramid<'a> {
    /// Every level, read with `textureLoad` as a `texture_2d<f32>` that is not filterable.
    pub view: &'a wgpu::TextureView,
    pub size: [u32; 2],
    pub levels: u32,
    pub generation: u64,
}

/// Drawn on the interface thread, but may be created on any thread.
pub trait Layer: Send {
    /// Writes what the layer draws with the view of this frame, such as its buffers of camera and
    /// instances, before any bundle is drawn, recording nothing. It runs at each frame, as `draw`
    /// does, inside a validation error scope: a layer that panics or fails here is removed and its
    /// module reported. Nothing by default.
    fn prepare(&mut self, gpu: &egui_wgpu::RenderState, view: &View) {
        let _ = (gpu, view);
    }

    /// Records what the layer computes on the GPU for this frame, such as the compute passes choosing
    /// what it draws, into an encoder of its own, submitted before the pass of the view. It runs at
    /// each frame after `prepare` when `computes` says so, inside a validation error scope, which
    /// the encoder is finished in: a layer that panics or fails here is removed and its module
    /// reported. Nothing by default.
    fn compute(&mut self, gpu: &egui_wgpu::RenderState, view: &View, encoder: &mut wgpu::CommandEncoder) {
        let _ = (gpu, view, encoder);
    }

    /// Whether the layer computes at this frame: an encoder is made for `compute` only then. Read at
    /// each frame, after `prepare`; none by default.
    fn computes(&self) -> bool {
        false
    }

    /// Records what the layer computes against the depth the first pass of this frame left, such as
    /// testing what it did not draw in `Phase::Opaque` to draw in `Phase::Revealed` what is in
    /// sight, into an encoder of its own, submitted between the two passes. It runs at each frame
    /// when `occludes` says so, inside a validation error scope, which the encoder is finished in: a
    /// layer that panics or fails here is removed and its module reported. Nothing by default.
    fn occlude(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        view: &View,
        pyramid: &Pyramid<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let _ = (gpu, view, pyramid, encoder);
    }

    /// Whether the layer computes against the pyramid at this frame: an encoder is made for
    /// `occlude` only then. Read at each frame, after `prepare`; none by default.
    fn occludes(&self) -> bool {
        false
    }

    /// How the layer draws: by `draw` into its bundle, by default, or by `draw_pass`. Read at each
    /// frame.
    fn drawing(&self) -> Drawing {
        Drawing::Bundle
    }

    /// When the layer is drawn among the others: in the scene, by default. Read at each frame.
    fn stage(&self) -> Stage {
        Stage::Scene
    }

    /// The version of what the layer records: its bundles are kept from frame to frame while the
    /// version stays the same, and recorded again when it changes, or when the device is created
    /// again. None, by default, records them at every frame. A layer whose bundles are kept
    /// changes what it draws through its buffers, written in `prepare`, or by a new version.
    fn version(&self) -> Option<u64> {
        None
    }

    /// Records what the layer draws in `phase` into its own render bundle of that phase, created by
    /// the viewport with the formats and sample count of `target`; create pipelines lazily from
    /// `gpu.device` to match. Called for a layer drawing in bundles, once for each phase; nothing
    /// by default.
    ///
    /// The viewport validates the bundles on its own: a layer that panics or records an invalid
    /// command is removed and its module reported as failed, without affecting the others.
    fn draw<'a>(
        &'a mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        view: &View,
        phase: Phase,
        bundle: &mut wgpu::RenderBundleEncoder<'a>,
    ) {
        let _ = (gpu, target, view, phase, bundle);
    }

    /// Draws what the layer draws in `phase` in the pass of the view at each frame, for a layer
    /// drawing in the pass, in the order of the layers, the bundles of the others run between. The
    /// pass is in no known state: the layer before may have left its own, and running bundles
    /// resets it, so the layer sets all it draws with (pipeline, bind groups, vertex and index
    /// buffers); a bundle never sees what it leaves. A layer that panics here is removed and its
    /// module reported, its other phase left; a GPU error in the pass, which the viewport learns
    /// only once the frame is finished, removes every layer drawn in the pass that frame. Nothing
    /// by default.
    fn draw_pass(
        &mut self,
        gpu: &egui_wgpu::RenderState,
        target: &Target,
        view: &View,
        phase: Phase,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let _ = (gpu, target, view, phase, pass);
    }

    /// What the layer drew with the view of the last frame, for the statistics of the view. Called
    /// after the layer is drawn; nothing by default.
    fn stats(&self) -> LayerStats {
        LayerStats::default()
    }

    /// Texts the view writes over its image, each above a point of the world, such as the names of
    /// what the layer draws; a few dozen at most. Called after the layer is drawn, as `stats` is;
    /// none by default.
    fn labels(&self) -> Vec<Label> {
        Vec::new()
    }
}

/// A text written over the view, above a point of the world.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub position: glam::Vec3,
    pub text: String,
    /// Its colour, RGBA.
    pub colour: [u8; 4],
}

/// What a layer drew in a frame, as the statistics of the view show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerStats {
    pub draws: u64,
    pub triangles: u64,
    /// What it holds on the GPU.
    pub bytes: u64,
    /// What it drew, counted its own way, such as `54 tiles`.
    pub items: String,
    /// The time its module spent on the interface thread for it this frame, outside `prepare` and
    /// `draw`, such as steering what it loads.
    pub steering: Duration,
}

#[cfg(test)]
mod tests {
    use super::{Allowance, BAND, BANDS, Demand, Phase, allow};

    #[test]
    fn the_opaque_is_drawn_in_the_first_pass_what_is_revealed_after_it_then_the_blended() {
        assert_eq!(Phase::ALL[0], Phase::Opaque);
        assert_eq!(Phase::ALL[1], Phase::Revealed);
        let first: Vec<Phase> = Phase::ALL.into_iter().filter(|phase| phase.first_pass()).collect();
        assert_eq!(first, [Phase::Opaque]);
        let blended: Vec<Phase> = Phase::ALL.into_iter().filter(|phase| phase.blended()).collect();
        assert_eq!(blended, [Phase::Beyond, Phase::Water, Phase::Near]);
    }

    /// A demand of `fixed` bytes, wanting `bytes` in each of the bands `wanted` and holding `held`.
    fn demand(fixed: u64, wanted: std::ops::Range<usize>, bytes: u64, held: u64) -> Demand {
        let mut demand = Demand {
            fixed,
            ..Demand::default()
        };
        for band in wanted {
            demand.wanted[band] = bytes;
            demand.held[band] = held;
        }
        demand
    }

    #[test]
    fn the_budget_gives_every_layer_the_same_reach_the_nearest_items_first() {
        // 20 fixed and 10 a band: 90 % of 100 holds 7 bands, all of it 8.
        let one = demand(20, 0..10, 10, 5);
        let allowance = allow(100, &[&one]);
        assert_eq!(allowance.load, 7.0 * BAND);
        assert_eq!(allowance.keep, 8.0 * BAND);
        assert_eq!(allowance.limited, Some(7.0 * BAND));
        assert_eq!(allowance.used, 20 + 10 * 5);

        // All fits: no limit, no reach.
        let all = allow(1_000, &[&one]);
        assert_eq!((all.load, all.keep, all.limited), (f32::INFINITY, f32::INFINITY, None));

        // Two layers share the bands: 10 + 10 a band, the fixed costs of both first.
        let other = demand(10, 0..10, 10, 0);
        let shared = allow(100, &[&one, &other]);
        assert_eq!(shared.load, 3.0 * BAND, "30 fixed, then 20 a band within 90");
        assert_eq!(shared.keep, 3.0 * BAND, "within 100");

        // A layer wanting nothing yet takes its fixed cost only.
        assert_eq!(
            allow(100, &[&demand(50, 0..0, 0, 0)]),
            Allowance {
                budget: 100,
                used: 50,
                load: f32::INFINITY,
                keep: f32::INFINITY,
                limited: None,
            }
        );
        assert_eq!(allow(100, &[]).limited, None);
    }

    #[test]
    fn an_item_falls_in_the_band_of_its_distance_the_farthest_in_the_last() {
        assert_eq!(Demand::band(0.0), 0);
        assert_eq!(Demand::band(BAND * 1.5), 1);
        assert_eq!(Demand::band(-5.0), 0);
        assert_eq!(Demand::band(1e9), BANDS - 1);
        assert_eq!(Demand::default().held.len(), BANDS);
    }
}
