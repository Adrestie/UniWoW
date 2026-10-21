//! Interface of the "viewport" service: a 3D view to which modules add their drawing.

use std::sync::Arc;
use std::time::Duration;

use crate::{ServiceKey, egui, egui_wgpu, glam, wgpu};

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

    /// Adds a tool of the view, which receives the left button and the pointer over it and draws its
    /// buttons in the toolbar along its top (`Tool`). `owner` is the id of the module adding it; its
    /// tools go with its layers when it fails. None by default.
    fn add_tool(&self, owner: &str, tool: Box<dyn Tool>) {
        let _ = (owner, tool);
    }

    /// Removes every tool added by `owner`.
    fn remove_tools(&self, owner: &str) {
        let _ = owner;
    }

    /// The view of the last frame drawn, as the tools and the commands of other threads see it;
    /// none before the first, or by default.
    fn last_view(&self) -> Option<Seen> {
        None
    }
}

/// The view of a frame drawn: the camera's transform and place, and the size of the view in points
/// of the interface, its top left at 0, 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub view_proj: glam::Mat4,
    pub eye: glam::Vec3,
    pub size: [f32; 2],
}

impl Seen {
    /// The ray through the point `at` of the view, in points from its top left.
    pub fn ray(&self, at: [f32; 2]) -> Option<Ray> {
        let ndc = [
            at[0] / self.size[0].max(1.0) * 2.0 - 1.0,
            1.0 - at[1] / self.size[1].max(1.0) * 2.0,
        ];
        Ray::through(self.view_proj, self.eye, ndc)
    }

    /// Where the point `position` of the world falls in the view, in points from its top left; none
    /// behind the eye.
    pub fn project(&self, position: glam::Vec3) -> Option<[f32; 2]> {
        let clip = self.view_proj * position.extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        Some([
            (clip.x / clip.w + 1.0) / 2.0 * self.size[0],
            (1.0 - clip.y / clip.w) / 2.0 * self.size[1],
        ])
    }
}

/// A ray of the world: its origin and its direction, of length 1 in the world; a distance along it
/// is in yards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: glam::Vec3,
    pub direction: glam::Vec3,
}

impl Ray {
    /// The ray from `eye` through the point `ndc` of the view `view_proj`, in its coordinates from
    /// -1 to 1, Y up; none where the transform cannot be inverted.
    pub fn through(view_proj: glam::Mat4, eye: glam::Vec3, ndc: [f32; 2]) -> Option<Ray> {
        let inverse = view_proj.inverse();
        // Reverse Z: a depth of 0.5 is twice as far as the near plane, well in front of the eye.
        let point = inverse * glam::Vec4::new(ndc[0], ndc[1], 0.5, 1.0);
        if point.w.abs() < f32::EPSILON || !point.is_finite() {
            return None;
        }
        let direction = (point.truncate() / point.w - eye).try_normalize()?;
        Some(Ray { origin: eye, direction })
    }

    pub fn at(&self, distance: f32) -> glam::Vec3 {
        self.origin + self.direction * distance
    }

    /// The ray in the axes of a model placed by `transform`: its direction scaled with it, so that
    /// a distance along it stays one of the world.
    pub fn into_model(&self, transform: &glam::Mat4) -> Ray {
        let inverse = transform.inverse();
        Ray {
            origin: inverse.transform_point3(self.origin),
            direction: inverse.transform_vector3(self.direction),
        }
    }

    /// Where it enters the box `bounds`, its distance, 0 from inside; none where it misses.
    pub fn hits_box(&self, bounds: [[f32; 3]; 2]) -> Option<f32> {
        let (mut near, mut far) = (0.0f32, f32::INFINITY);
        for (axis, (&low, &high)) in bounds[0].iter().zip(&bounds[1]).enumerate() {
            let (origin, direction) = (self.origin[axis], self.direction[axis]);
            if direction.abs() < 1e-12 {
                if origin < low || origin > high {
                    return None;
                }
                continue;
            }
            let (a, b) = ((low - origin) / direction, (high - origin) / direction);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
            if near > far {
                return None;
            }
        }
        Some(near)
    }

    /// Where it meets the triangle `a`, `b`, `c`, of either side, its distance; none where it misses.
    pub fn hits_triangle(&self, a: glam::Vec3, b: glam::Vec3, c: glam::Vec3) -> Option<f32> {
        let (ab, ac) = (b - a, c - a);
        let p = self.direction.cross(ac);
        let det = ab.dot(p);
        if det.abs() < 1e-12 {
            return None;
        }
        let inverse = 1.0 / det;
        let t = self.origin - a;
        let u = t.dot(p) * inverse;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = t.cross(ab);
        let v = self.direction.dot(q) * inverse;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let distance = ac.dot(q) * inverse;
        (distance >= 0.0).then_some(distance)
    }
}

/// A tool of the 3D view, such as the selection or the gizmos: called on the interface thread at
/// each frame the view is drawn, inside `catch_unwind`, a tool that panics removed and its module
/// reported. The camera keeps the right and middle buttons, the wheel and the flying keys.
pub trait Tool: Send {
    /// Draws its buttons in the toolbar along the top of the view; none by default.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let _ = ui;
    }

    /// The pointer over the view at this frame: the response of the view (the left button pressed,
    /// dragged, released, clicked twice, the modifiers), and the view; `painter` paints over the
    /// image. Nothing by default.
    fn pointer(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let _ = (input, painter);
    }

    /// Whether it takes the pointer alone at this frame, the other tools given none: a mode that
    /// places points by clicks (step 16.4). None by default.
    fn claims(&mut self, input: &ToolInput<'_>) -> bool {
        let _ = input;
        false
    }
}

/// What a tool receives at each frame.
pub struct ToolInput<'a> {
    pub response: &'a egui::Response,
    /// The view on the screen, in points.
    pub rect: egui::Rect,
    /// The view of the frame, its size that of `rect`.
    pub seen: Seen,
}

impl ToolInput<'_> {
    /// The ray under the point `at` of the screen.
    pub fn ray_at(&self, at: egui::Pos2) -> Option<Ray> {
        self.seen.ray([at.x - self.rect.left(), at.y - self.rect.top()])
    }

    /// Where the point `position` of the world falls on the screen; none behind the eye.
    pub fn project(&self, position: glam::Vec3) -> Option<egui::Pos2> {
        let [x, y] = self.seen.project(position)?;
        Some(egui::pos2(self.rect.left() + x, self.rect.top() + y))
    }
}

/// The light of the map at the place of the camera and the hour: its sun, the colour of its fog in
/// gamma, where its fog starts and ends, in yards, and its rate when the fog of the game is drawn
/// (`Fog`), none for the editor's, and then how far its lights alone end their fog, past which
/// nothing is drawn, which a building or the weather bringing the fog nearer leaves as it is; its sky
/// (`Sky`), the light of its water (`Water`), and how much the materials of the buildings that glow
/// at night glow at that hour, from 0 to 1 (`View::night`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapLight {
    pub sun: Sun,
    pub fog_colour: [f32; 3],
    pub fog: Option<[f32; 3]>,
    pub reach: Option<f32>,
    pub sky: Sky,
    pub water: Water,
    pub night: f32,
}

/// The colours of the sky, in gamma, from its top down to its fog (the bands 2 to 7 of a light): its
/// top, its middle, towards the horizon, over it, the horizon and the fog, on the rings of the dome
/// of the sky as Noggit draws it, 90°, 18°, 10°, 3° and 0° high, then the fog from −30° down.
pub type Sky = [[f32; 3]; 6];

/// The light of the water, in gamma: the colours of the river (the bands 16 and 17 of a light) and
/// of the ocean (14 and 15), shallow then deep, each with its alpha (those of its params); and the
/// colour of the sun (the band 9), which the water reflects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Water {
    pub river: [[f32; 4]; 2],
    pub ocean: [[f32; 4]; 2],
    pub sun: [f32; 3],
}

/// The WGSL of the fog of the view (`fog_amount`, `fog_mix`, `beyond_fog`), which the shaders of the
/// view share, for a uniform `camera` with a perspective `view_proj`, an `eye` and a `fog` as `Fog`
/// gives them: where it starts, its middle, where it ends and its rate.
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

// `colour` under the fog of the colour `fog` by `amount`, both in gamma as the client mixes them:
// the colour bounded to 1 first under the game's fog.
fn fog_mix(colour: vec3<f32>, fog: vec3<f32>, amount: f32) -> vec3<f32> {
    if camera.fog.w > 0.0 {
        return mix(clamp(colour, vec3<f32>(0.0), vec3<f32>(1.0)), fog, amount);
    }
    return mix(colour, fog, amount);
}

// Whether `position` lies beyond the end of the fog, past which nothing is drawn, as Noggit draws
// nothing beyond it: by the depth along the view under the game's fog, past the end of the fog of
// the lights alone (its `middle`), never short of its end; on the ground under the editor's, past
// its end.
fn beyond_fog(position: vec3<f32>) -> bool {
    if camera.fog.w > 0.0 {
        return view_depth(position) > max(camera.fog.y, camera.fog.z);
    }
    return length(position.xy - camera.eye.xy) > camera.fog.z;
}
";

/// The WGSL of the light of the view (`light`), which the shaders of the view share, for a uniform
/// `camera` with a `sun`, a `sun_colour` and an `ambient` as `Sun` gives them.
pub const LIGHT_WGSL: &str = r"// The light of a face of `normal`, in gamma, as Noggit lights it and the client: the ambient light
// from 0.9 to 1.1 times as the face turns to the sun, plus the diffuse by the angle.
fn light(normal: vec3<f32>) -> vec3<f32> {
    let facing = clamp(dot(normalize(normal), camera.sun.xyz), 0.0, 1.0);
    return camera.ambient.rgb * (0.9 + 0.2 * facing) + camera.sun_colour.rgb * facing;
}
";

/// The share of the editor's fog at its middle distance (`FOG_WGSL`).
pub const NEAR_FOG: f32 = 0.55;

/// The fog of the view and its colour, that of the sky without the light of a map, in gamma. The
/// editor's when `rate` is 0: by the distance on the ground from the eye, none up to `start`,
/// `NEAR_FOG` of it at `middle`, all of it from `end`. The game's otherwise, as the client draws
/// it: by the depth along the view, 1 − ((end − depth) / (end − start))^rate, nothing drawn past
/// `middle`, the end of the fog of the lights alone (`MapLight::reach`), nor short of `end`.
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
            colour: [0.634, 0.687, 0.748],
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
/// angle, multiplying its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub direction: [f32; 3],
    pub colour: [f32; 3],
    pub ambient: [f32; 3],
}

impl Default for Sun {
    /// The light the terrain had before the lights of the maps: 0.7 on a face turned away from the
    /// sun, and 1 on one facing it.
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

/// Formats of the render target a layer draws into, its colour in gamma as the client's, blended and
/// resolved so; its pipelines must match them.
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
    /// The sky of the light of the map, none for one of the colour of the fog alone.
    pub sky: Option<Sky>,
    /// The light of the water of the map, none for the fixed colours of the water.
    pub water: Option<Water>,
    /// How much the materials of the buildings that glow at night (their flag 0x10) add their
    /// emissive colour, from 0 by day to 1 by night, as the light of the map gives it; 0 without
    /// it, the fixed light being that of noon.
    pub night: f32,
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

/// When a layer is drawn among the others in each phase: the ground first, then the stars, the sun,
/// the moon and the clouds, the models of the sky over them, as the client draws them (`0x7f0b43`,
/// `0x7f0c1a` in 12340), the horizon, the water, the scene, then the glare of the sun. Layers of one
/// stage are drawn in the order they were added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// The terrain and its sky.
    Ground,
    /// The stars, over the sky of the ground.
    Stars,
    /// The sun, the moon and the clouds, over the stars.
    Celestial,
    /// The models of the sky, over the sun, the moon and the clouds.
    Sky,
    /// The horizon of the map, over the sky, as Noggit paints it.
    Horizon,
    /// The liquids.
    Water,
    /// What stands on the ground, by default.
    #[default]
    Scene,
    /// The glare of the sun or of the moon, over everything, as the client draws it last.
    Glare,
}

/// The phases a frame is drawn in, over two passes. The first paints the sky (`Sky`), as Noggit and
/// the client paint it before the world, neither testing the depth nor writing it, then draws what
/// every layer draws opaque, writing the depth, such as what it saw at the frame before, over the
/// sky; nothing is drawn beyond the end of the fog (`beyond_fog`). The depth it leaves is reduced to a
/// pyramid (`Pyramid`), which the layers test the rest of what they draw against between the two
/// passes (`Layer::occlude`). The second draws the opaque they found in sight so (`Revealed`), then
/// what every layer blends over it all, split at the surface of the water
/// (`liquids::Surfaces::phase`) so that a blended batch under the water is seen through it: what
/// lies beyond the surface from the eye, under it from over it and over it from under it; the
/// water; then what lies on the eye's side. A blended batch of a layer is drawn over the opaque ones
/// of every other, whatever their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Sky,
    Opaque,
    Revealed,
    Beyond,
    Water,
    Near,
}

impl Phase {
    /// The phases, in the order they are drawn.
    pub const ALL: [Phase; 6] = [
        Phase::Sky,
        Phase::Opaque,
        Phase::Revealed,
        Phase::Beyond,
        Phase::Water,
        Phase::Near,
    ];

    /// Whether what is drawn in it is blended over what the world drew before; the sky, painted
    /// first, is blended over itself only.
    pub fn blended(self) -> bool {
        !matches!(self, Phase::Sky | Phase::Opaque | Phase::Revealed)
    }

    /// Whether it is drawn in the first pass, whose depth the pyramid is made of.
    pub fn first_pass(self) -> bool {
        matches!(self, Phase::Sky | Phase::Opaque)
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
    use super::{Allowance, BAND, BANDS, Demand, Phase, Stage, allow};

    #[test]
    fn a_ray_through_the_view_meets_boxes_and_triangles_at_their_distance() {
        use glam::{Mat4, Vec3};
        let eye = Vec3::new(0.0, 0.0, 10.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let projection = Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.5);
        let seen = super::Seen {
            view_proj: projection * view,
            eye,
            size: [200.0, 200.0],
        };
        let ray = seen.ray([100.0, 100.0]).unwrap();
        assert!((ray.direction - Vec3::NEG_Z).length() < 1e-4);
        assert!((ray.hits_box([[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]).unwrap() - 9.0).abs() < 1e-4);
        assert_eq!(ray.hits_box([[5.0, 5.0, -1.0], [6.0, 6.0, 1.0]]), None);
        let hit = ray.hits_triangle(
            Vec3::new(-1.0, -1.0, 2.0),
            Vec3::new(1.0, -1.0, 2.0),
            Vec3::new(0.0, 1.0, 2.0),
        );
        assert!((hit.unwrap() - 8.0).abs() < 1e-4);
        let away = ray.hits_triangle(
            Vec3::new(2.0, 2.0, 0.0),
            Vec3::new(3.0, 2.0, 0.0),
            Vec3::new(2.0, 3.0, 0.0),
        );
        assert_eq!(away, None);
        let projected = seen.project(Vec3::ZERO).unwrap();
        assert!((projected[0] - 100.0).abs() < 1e-3 && (projected[1] - 100.0).abs() < 1e-3);
        // A model scaled twice: the distance along the ray in its axes stays one of the world.
        let placed =
            Mat4::from_scale_rotation_translation(Vec3::splat(2.0), glam::Quat::IDENTITY, Vec3::new(0.0, 0.0, 1.0));
        let local = ray.into_model(&placed);
        let unit = local.hits_box([[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]).unwrap();
        assert!((unit - 7.0).abs() < 1e-4, "{unit}");
    }

    #[test]
    fn the_sky_then_the_opaque_are_drawn_in_the_first_pass_what_is_revealed_after_them_then_the_blended() {
        assert_eq!(Phase::ALL[0], Phase::Sky);
        assert_eq!(Phase::ALL[1], Phase::Opaque);
        assert_eq!(Phase::ALL[2], Phase::Revealed);
        let first: Vec<Phase> = Phase::ALL.into_iter().filter(|phase| phase.first_pass()).collect();
        assert_eq!(first, [Phase::Sky, Phase::Opaque]);
        let blended: Vec<Phase> = Phase::ALL.into_iter().filter(|phase| phase.blended()).collect();
        assert_eq!(blended, [Phase::Beyond, Phase::Water, Phase::Near]);
        // The stars, the bodies and the clouds, the models of the sky over them, then the horizon,
        // before the water and the scene.
        assert!(Stage::Ground < Stage::Stars && Stage::Stars < Stage::Celestial);
        assert!(Stage::Celestial < Stage::Sky && Stage::Sky < Stage::Horizon && Stage::Horizon < Stage::Water);
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

/// Whether the WGSL `source` parses and validates as the device will take it, its error said
/// otherwise: for the tests of the modules' shaders, which a device is not needed for.
pub fn check_wgsl(source: &str) -> Result<(), String> {
    use crate::wgpu::naga;
    let module = naga::front::wgsl::parse_str(source).map_err(|error| error.emit_to_string(source))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|error| error.emit_to_string(source))?;
    Ok(())
}

#[cfg(test)]
mod wgsl_tests {
    #[test]
    fn a_shader_with_a_reserved_word_or_a_wrong_type_is_refused_with_its_error() {
        let fine = "@vertex fn vs(@location(0) seg_start: vec3<f32>) -> @builtin(position) vec4<f32> { return vec4<f32>(seg_start, 1.0); }";
        assert!(super::check_wgsl(fine).is_ok());
        let reserved = fine.replace("seg_start", "from");
        let error = super::check_wgsl(&reserved).unwrap_err();
        assert!(error.contains("reserved"), "{error}");
        let wrong = "@vertex fn vs() -> @builtin(position) vec4<f32> { return vec3<f32>(1.0); }";
        assert!(super::check_wgsl(wrong).is_err(), "a value of the wrong type");
    }
}
