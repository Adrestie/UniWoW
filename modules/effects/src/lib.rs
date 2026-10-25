//! Spell effects in the 3D view: an effect description of WoW-mods (`forge`) made into its model
//! by `uniwow_api::vfx`, or a model of the client (`Spells\*.m2`), played in time: its bones, its
//! meshes, its particles and ribbons, as the client draws them. A reference character stands where
//! the effect plays, its attachment points shown, the effect on the one chosen. The time is an
//! animatable property (`effects/time`), so that the timeline can drive it.

mod client;
mod doc;
mod gpu;
mod inspector;
mod layer;
mod link;
mod load;
mod schema;
mod view;

#[cfg(test)]
mod render_tests;

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::glam::{Mat4, Vec3};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::vfx::frame::Ground;
use uniwow_api::vfx::m2::{Model, Skin};
use uniwow_api::vfx::texgen::Image;
use uniwow_api::{
    Arrangement, Context, DockArea, JobId, JobOutcome, Module, PropertyKind, PropertyValue, Registrar, egui, log,
    models, scene, vfs, viewport, wgpu,
};

use doc::{Doc, Edit, Node};
use gpu::Gpu;
use inspector::Inspector;
use layer::EffectLayer;
use uniwow_api::Gesture;
use uniwow_api::vfx::json;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// An effect loaded: its model, its skin, the images of its textures (none for those it does not
/// name or that could not be read), what forge noted.
pub struct Loaded {
    pub name: String,
    pub source: String,
    pub model: Model,
    pub skin: Option<Skin>,
    pub images: Vec<Option<Image>>,
    pub notes: Vec<String>,
    /// Changes with each effect loaded: the layer starts its particles again.
    pub version: u64,
}

/// What the module and its layer share.
pub struct Stage {
    pub effect: Option<Arc<Loaded>>,
    /// The groups of the effect's textures on the GPU, by the model's texture.
    pub textures: Option<Arc<Vec<Option<wgpu::BindGroup>>>>,
    /// Where the effect's origin stands in the world.
    pub anchor: Vec3,
    /// Seconds since the effect appeared.
    pub time: f64,
    pub playing: bool,
    pub looping: bool,
    pub speed: f32,
    /// How many times the hold of a state model loops in the preview.
    pub holds: u32,
    /// The length of the playback, seconds (the layer writes it).
    pub length: f64,
    pub show_bones: bool,
    /// Its time set by the Timeline's playhead: it does not move on by itself.
    pub driven: bool,
    /// Points drawn as dots, in the world: the attachment points of the reference.
    pub markers: Vec<([f32; 3], [f32; 4])>,
    /// The ground under the effect, in its space, for what the client lays on it.
    pub ground: Option<Arc<Ground>>,
    /// The attachment point chosen, and where it is on the reference character (the Effect view
    /// puts the effect there).
    pub attach_id: u32,
    pub attach_offset: Vec3,
    /// When the time last moved on.
    last_tick: Option<std::time::Instant>,
}

impl Stage {
    /// Moves the time on by what passed since the last call, when it plays: one clock for the 3D
    /// view and the Effect view, whichever draws first.
    pub fn tick(&mut self) {
        let now = std::time::Instant::now();
        let dt = self
            .last_tick
            .map_or(0.0, |last| now.duration_since(last).as_secs_f64().min(0.25));
        self.last_tick = Some(now);
        let length = self.effect.as_ref().map_or(0.0, |e| {
            uniwow_api::vfx::anim::Playback::of(&e.model, self.holds).length() / 1000.0
        });
        if self.playing && !self.driven && length > 0.0 {
            self.time += dt * f64::from(self.speed);
            if self.time > length {
                self.time = if self.looping { self.time % length } else { length };
                self.playing &= self.looping;
            }
        }
        self.length = length;
    }
}

impl Default for Stage {
    fn default() -> Self {
        Self {
            effect: None,
            textures: None,
            anchor: Vec3::ZERO,
            time: 0.0,
            playing: true,
            looping: true,
            speed: 1.0,
            holds: 2,
            length: 0.0,
            show_bones: false,
            driven: false,
            markers: Vec::new(),
            ground: None,
            attach_id: 19,
            attach_offset: Vec3::ZERO,
            last_tick: None,
        }
    }
}

/// The attachment points an effect is put on, as the spells' kits name them, and their id in the
/// models (SpellVisualKitModelAttach: head 20, chest 34, base 19 seen in the Monk's table; the hands
/// 21 and 22 a guess).
pub const ATTACHMENTS: [(&str, u32); 5] = [
    ("base", 19),
    ("chest", 34),
    ("head", 20),
    ("left_hand", 21),
    ("right_hand", 22),
];

/// WoW-mods' examples of effects.
const EXAMPLES: &str = r"E:\WoW-mods\tools\vfx\examples";
/// A new effect: a glow rising from the ground.
const NEW_EFFECT: &str = r##"{
 "name": "vfx_new",
 "duration": 2000,
 "radius": 3,
 "textures": {
  "glow": {
   "kind": "glow"
  }
 },
 "emitters": [
  {
   "texture": "glow",
   "shape": "plane",
   "area": [
    1,
    1
   ],
   "rate": 20,
   "life": 1.2,
   "speed": 1.5,
   "spread": 10,
   "colors": "#80c0ff",
   "size": 0.4
  }
 ]
}"##;

/// The reference character.
const CHARACTER: &str = "Character\\Human\\Male\\HumanMale.m2";
const CHARACTER_SKIN: &str = "Character\\Human\\Male\\HumanMaleSkin00_00.blp";

#[derive(Default)]
pub struct EffectsModule {
    stage: Arc<Mutex<Stage>>,
    incoming: Arc<Mutex<Option<Arc<Gpu>>>>,
    gpu: Option<Arc<Gpu>>,
    gpu_job: Option<JobId>,
    load_job: Option<JobId>,
    textures_job: Option<JobId>,
    character_job: Option<JobId>,
    ground_job: Option<(JobId, Vec3)>,
    drawn: bool,
    version: u64,
    error: Option<String>,
    /// The character's place, and its attachment points from it (by id).
    origin: Vec3,
    placed: bool,
    character: bool,
    points: Vec<(u32, Vec3)>,
    attach: usize,
    show_points: bool,
    /// The description edited, when the effect is one.
    doc: Option<Doc>,
    selected: Node,
    inspector: Inspector,
    forge_job: Option<JobId>,
    /// A drag of a value going on: one entry of the history.
    gesture: Option<Gesture>,
    /// The Effect view, the reference character it draws, and what makes it.
    view: view::EffectView,
    character_view: Option<view::Character>,
    character_data_job: Option<JobId>,
    character_upload_job: Option<JobId>,
    /// The viewport's target, which the pipelines are made for.
    target: Option<uniwow_api::viewport::Target>,
    /// A drag of the gizmo going on: one entry of the history.
    gizmo_gesture: Option<Gesture>,
    /// The dock laid out for the effects (`Back to the world` puts it back).
    arranged: bool,
    /// A change of the inspector, applied once its panel is drawn.
    pending: Option<inspector::Change>,
    /// The Timeline to show the effect's tracks, once presented.
    focus_timeline: bool,
    /// What the Timeline shows of the effect: the part and the description it was made from.
    presented: Option<(Node, json::Value)>,
    presented_version: u64,
    /// A change of keys in the Timeline going on: one entry of the history.
    timeline_gesture: Option<Gesture>,
    /// The time axes of the curves of a particle's life, by key.
    life_axes: std::collections::HashMap<String, uniwow_api::curve::TimeAxis>,
    /// The view asked at the start (`UNIWOW_EFFECTS_VIEW`), taken once the effect is shown.
    start_view: Option<View>,
    /// The time asked at the start (`UNIWOW_EFFECTS_TIME`, seconds): the effect held there.
    start_time: Option<f64>,
}

/// The module, for its commands.
pub fn module(any: &mut dyn Any) -> &mut EffectsModule {
    any.downcast_mut().expect("commands of this module are applied to it")
}

impl Module for EffectsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("view", "Effect view", DockArea::Right);
        reg.panel("inspector", "Part", DockArea::Right);
        reg.menu_item("Effects", "New effect", "new")
            .menu_item("Effects", "Open an effect…", "open");
        for (name, path) in Self::examples() {
            reg.menu_item("Effects", &format!("Example: {name}"), &format!("example:{path}"));
        }
        reg.menu_item("Effects", "Show the effects' panels", "focus").menu_item(
            "Effects",
            "Back to the world",
            "world",
        );
        reg.panel("effects", "Effects", DockArea::Right)
            .command(
                "effects.open",
                "Opens an effect: a description of WoW-mods' forge (.json) or a model (.m2), on the disk or in the client (Spells\\X.m2).",
                json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
                json!({ "type": "object" }),
            )
            .command(
                "effects.state",
                "The effect shown: its name, its time and length in seconds, whether it plays.",
                json!({ "type": "object" }),
                json!({ "type": "object" }),
            );
        let (read, write) = (self.stage.clone(), self.stage.clone());
        reg.animatable(
            "time",
            "Effect time",
            PropertyKind::Number,
            [0.0, 600.0],
            move || PropertyValue::Number(lock(&read).time),
            move |value| {
                let mut stage = lock(&write);
                stage.time = value.components()[0].max(0.0);
                stage.playing = false;
            },
        );
    }

    fn init(&mut self, ctx: &mut Context) {
        self.character = true;
        self.show_points = true;
        // checks without the mouse: an effect opened, a view and a moment chosen at the start
        self.start_view = std::env::var("UNIWOW_EFFECTS_VIEW")
            .ok()
            .and_then(|v| match v.as_str() {
                "grazing" => Some(View::Grazing),
                "top" => Some(View::Top),
                "three" => Some(View::Three),
                _ => None,
            });
        self.start_time = std::env::var("UNIWOW_EFFECTS_TIME").ok().and_then(|t| t.parse().ok());
        if let Some(at) = std::env::var("UNIWOW_EFFECTS_AT").ok().and_then(|t| {
            let v: Vec<f32> = t.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2]))
        }) {
            self.origin = at;
            self.placed = true;
        }
        if let Ok(path) = std::env::var("UNIWOW_EFFECTS_OPEN") {
            self.open(ctx, Some(path));
        }
        let Some(view) = ctx.service(viewport::SERVICE) else {
            log::info!("no viewport service: the effects are not drawn");
            return;
        };
        let Some(gpu) = ctx.gpu().cloned() else {
            return;
        };
        view.add_layer(
            ctx.module_id(),
            Box::new(EffectLayer::new(self.stage.clone(), self.incoming.clone())),
        );
        self.drawn = true;
        let target = view.target();
        self.target = Some(target);
        self.gpu_job = Some(ctx.spawn("Build the effects' GPU resources", move |_| {
            Arc::new(gpu::create(&gpu.device, &gpu.queue, &target))
        }));
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, ctx: &mut Context) {
        if Some(job) == self.gpu_job {
            match outcome {
                JobOutcome::Panicked(message) => {
                    log::error!("the effects' GPU resources could not be built: {message}")
                }
                outcome => {
                    self.gpu = outcome.take::<Arc<Gpu>>();
                    *lock(&self.incoming) = self.gpu.clone();
                    self.upload_textures(ctx);
                }
            }
        } else if Some(job) == self.load_job {
            match outcome.take::<Result<Option<load::Opened>, String>>() {
                Some(Ok(Some(load::Opened::Model(loaded)))) => {
                    self.doc = None;
                    self.show(ctx, *loaded, true);
                }
                Some(Ok(Some(load::Opened::Description(path, value, layout)))) => {
                    self.doc = Some(Doc::new(Some(path), value, layout));
                    self.selected = Node::Effect;
                    self.reforge(ctx, true);
                }
                Some(Ok(None)) => {}
                Some(Err(error)) => self.error = Some(error),
                None => self.error = Some("the effect could not be read".into()),
            }
        } else if Some(job) == self.forge_job {
            self.forge_job = None;
            match outcome.take::<(bool, Result<Loaded, String>)>() {
                Some((first, Ok(loaded))) => self.show(ctx, loaded, first),
                Some((_, Err(error))) => self.error = Some(error),
                None => self.error = Some("the model could not be made".into()),
            }
            if self.doc.as_ref().is_some_and(|d| d.stale) {
                self.reforge(ctx, false);
            }
        } else if Some(job) == self.textures_job {
            if let Some((version, groups)) = outcome.take::<(u64, Vec<Option<wgpu::BindGroup>>)>() {
                let mut stage = lock(&self.stage);
                if stage.effect.as_ref().is_some_and(|e| e.version == version) {
                    stage.textures = Some(Arc::new(groups));
                }
            }
        } else if self.ground_job.is_some_and(|(j, _)| j == job) {
            let (_, at) = self.ground_job.take().expect("tested");
            let ground = outcome.take::<Ground>();
            // the character and the effect stand on the ground under them
            let under = ground.as_ref().and_then(|g| g.height(0.0, 0.0));
            if let Some(h) = under.filter(|h| h.abs() > 0.02)
                && lock(&self.stage).anchor == at
            {
                self.origin.z += h;
                self.place_character(ctx);
                return;
            }
            let mut stage = lock(&self.stage);
            if stage.anchor == at {
                stage.ground = ground.map(Arc::new);
            }
        } else if Some(job) == self.character_data_job {
            self.character_data_job = None;
            match outcome.take::<Result<load::CharacterData, String>>() {
                Some(Ok(data)) => self.upload_character(ctx, data),
                Some(Err(error)) => log::warn!("the Effect view's character: {error}"),
                None => {}
            }
        } else if Some(job) == self.character_upload_job {
            self.character_upload_job = None;
            if let Some(character) = outcome.take::<view::Character>() {
                self.character_view = Some(character);
                self.update_anchor();
            }
        } else if Some(job) == self.character_job {
            match outcome.take::<Result<Vec<(u32, Vec3)>, String>>() {
                Some(Ok(points)) => {
                    self.points = points;
                    self.update_anchor();
                    self.sample_ground(ctx);
                }
                Some(Err(error)) => log::warn!("the reference character's attachment points: {error}"),
                None => {}
            }
        }
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "effects.open" => {
                let path = arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("a path is needed")?
                    .to_owned();
                self.open(ctx, Some(path));
                Ok(json!({ "opening": true }))
            }
            "effects.state" => {
                let stage = lock(&self.stage);
                Ok(json!({
                    "name": stage.effect.as_ref().map(|e| e.name.clone()),
                    "time": stage.time,
                    "length": stage.length,
                    "playing": stage.playing,
                    "error": self.error,
                }))
            }
            _ => Err(format!("'{name}' is not a command of the effects")),
        }
    }

    fn panel_ui(&mut self, panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if self.doc.as_ref().is_some_and(|d| d.stale) {
            self.reforge(ctx, false);
        }
        match panel {
            "view" => self.view_ui(ui, ctx),
            "inspector" => self.part_ui(ui, ctx),
            _ => self.effect_ui(ui, ctx),
        }
    }

    fn on_menu(&mut self, action: &str, ctx: &mut Context) {
        match action {
            "new" => self.new_effect(ctx),
            "open" => self.open(ctx, None),
            "world" => ctx.restore_arrangement(),
            "focus" => self.arrange(ctx),
            other => {
                if let Some(path) = other.strip_prefix("example:") {
                    self.open(ctx, Some(path.to_owned()));
                }
            }
        }
    }
    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        self.sync_timeline(ctx);
    }

    fn unsaved(&self) -> Vec<String> {
        match &self.doc {
            Some(doc) if doc.unsaved() => vec![
                doc.value
                    .get("name")
                    .and_then(json::Value::as_str)
                    .unwrap_or("effect")
                    .to_owned(),
            ],
            _ => Vec::new(),
        }
    }

    fn save_unsaved(&mut self, _ctx: &mut Context) -> Result<(), String> {
        let path = self.doc.as_ref().and_then(|d| d.path.clone());
        self.save(path)
    }
}

/// How the camera looks at the effect.
#[derive(Clone, Copy)]
enum View {
    /// From the side, close to the ground: what cuts into it shows.
    Grazing,
    Top,
    Three,
}

/// How far about the effect the ground is read, in yards, and in how many samples a side.
const GROUND_REACH: f32 = 10.0;
const GROUND_SAMPLES: usize = 41;

/// The heights of the terrain about `anchor`, in the space of the effect (from `anchor`).
fn ground_under(scene: &dyn scene::Scene, anchor: Vec3) -> Ground {
    let step = 2.0 * GROUND_REACH / (GROUND_SAMPLES - 1) as f32;
    let above = 60.0;
    let mut heights = Vec::with_capacity(GROUND_SAMPLES * GROUND_SAMPLES);
    for j in 0..GROUND_SAMPLES {
        for i in 0..GROUND_SAMPLES {
            let (x, y) = (-GROUND_REACH + i as f32 * step, -GROUND_REACH + j as f32 * step);
            let ray = viewport::Ray {
                origin: anchor + Vec3::new(x, y, above),
                direction: Vec3::NEG_Z,
            };
            heights.push(scene.ground(&ray).map(|t| above - t));
        }
    }
    Ground {
        low: [-GROUND_REACH, -GROUND_REACH],
        step,
        count: GROUND_SAMPLES,
        heights,
    }
}

fn camera_target(ctx: &Context) -> Vec3 {
    match ctx.read_property("viewport/camera_target") {
        Ok(PropertyValue::Vector([x, y, z])) => Vec3::new(x as f32, y as f32, z as f32),
        _ => Vec3::ZERO,
    }
}

impl EffectsModule {
    fn open(&mut self, ctx: &mut Context, path: Option<String>) {
        let vfs = ctx.service(vfs::SERVICE);
        self.load_job = Some(
            ctx.spawn("Open an effect", move |_| -> Result<Option<load::Opened>, String> {
                let path = match path {
                    Some(path) => path,
                    None => {
                        let Some(picked) = uniwow_api::rfd::FileDialog::new()
                            .set_title("Open an effect")
                            .add_filter("Effect description or model", &["json", "m2"])
                            .pick_file()
                        else {
                            return Ok(None);
                        };
                        picked.to_string_lossy().into_owned()
                    }
                };
                load::open(&path, vfs.as_deref()).map(Some)
            }),
        );
    }

    /// The reference character of the Effect view: read by a job once the client's archives are
    /// open, its textures uploaded by another.
    fn load_character(&mut self, ctx: &mut Context) {
        if self.character_view.is_some() || self.character_data_job.is_some() || self.character_upload_job.is_some() {
            return;
        }
        let Some(vfs) = ctx.service(vfs::SERVICE) else {
            return;
        };
        if !matches!(vfs.state(), uniwow_api::vfs::VfsState::Ready { .. }) {
            return;
        }
        self.character_data_job = Some(ctx.spawn("Read the reference character", move |_| {
            load::character(Some(vfs.as_ref()), CHARACTER, CHARACTER_SKIN)
        }));
    }

    fn upload_character(&mut self, ctx: &mut Context, data: load::CharacterData) {
        let (Some(resources), Some(gpu)) = (self.gpu.clone(), ctx.gpu().cloned()) else {
            return;
        };
        self.character_upload_job = Some(ctx.spawn("Upload the reference character's textures", move |_| {
            let groups = data
                .images
                .iter()
                .zip(&data.model.textures)
                .map(|(image, texture)| {
                    image.as_ref().map(|image| {
                        gpu::texture_group(
                            &gpu.device,
                            &gpu.queue,
                            &resources.texture_layout,
                            &resources.samplers,
                            image,
                            texture.flags,
                        )
                    })
                })
                .collect();
            view::Character {
                model: data.model,
                skin: data.skin,
                groups,
                points: data.points,
            }
        }));
    }

    /// Where the part chosen stands in the Effect view: its place (`pos`), on the attachment point.
    fn gizmo_point(&self) -> Option<Vec3> {
        let doc = self.doc.as_ref()?;
        if matches!(self.selected, Node::Effect | Node::Texture(_)) {
            return None;
        }
        let part = self.selected.get(&doc.value)?;
        let pos = match part.get("pos").and_then(json::Value::as_list) {
            Some([x, y, z]) => Vec3::new(
                x.as_f64().unwrap_or(0.0) as f32,
                y.as_f64().unwrap_or(0.0) as f32,
                z.as_f64().unwrap_or(0.0) as f32,
            ),
            _ => Vec3::ZERO,
        };
        Some(pos + lock(&self.stage).attach_offset)
    }

    /// The part chosen moved by the gizmo: its place changed, one entry of the history a drag.
    fn gizmo_moved(&mut self, ctx: &mut Context, moved: view::Moved) {
        if moved.done {
            if let Some(gesture) = self.gizmo_gesture.take() {
                ctx.end_gesture(gesture);
            }
            return;
        }
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let before = doc.value.clone();
        let mut after = before.clone();
        let Some(part) = self.selected.get_mut(&mut after) else {
            return;
        };
        let mut pos: Vec<json::Value> = part
            .get("pos")
            .and_then(json::Value::as_list)
            .map(<[json::Value]>::to_vec)
            .unwrap_or_else(|| vec![json::Value::Int(0); 3]);
        pos.resize(3, json::Value::Int(0));
        let v = pos[moved.axis].as_f64().unwrap_or(0.0) + f64::from(moved.yards);
        // a hundredth of a yard: what the inspector shows
        pos[moved.axis] = json::Value::Float((v * 100.0).round() / 100.0);
        part.set("pos", json::Value::List(pos));
        match self.gizmo_gesture {
            Some(gesture) if ctx.resume_gesture(gesture) => {}
            _ => self.gizmo_gesture = Some(ctx.begin_gesture("effect: part moved")),
        }
        ctx.execute(Edit {
            label: "effect: part moved".into(),
            before,
            after,
        });
    }

    /// The Effect view's panel: its camera's angles, its ground, its character, the view itself.
    fn view_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        if lock(&self.stage).effect.is_none() && self.doc.is_none() {
            self.welcome(ui, ctx);
            return;
        }
        self.load_character(ctx);
        if self.doc.as_ref().is_some_and(|d| d.stale) {
            self.reforge(ctx, false);
        }
        ui.horizontal_wrapped(|ui| {
            for (label, angle) in [
                ("Grazing", view::Angle::Grazing),
                ("Top", view::Angle::Top),
                ("Three-quarter", view::Angle::Three),
            ] {
                let tip = match angle {
                    view::Angle::Grazing => "From the side, close to the ground: what cuts into the ground shows",
                    view::Angle::Top => "From above: what is laid on the ground",
                    view::Angle::Three => "Three-quarter, as a player sees it",
                };
                if ui.button(label).on_hover_text(tip).clicked() {
                    self.view.angle(angle);
                }
            }
            if ui.button("Frame").on_hover_text("The whole effect in view").clicked() {
                let radius = lock(&self.stage)
                    .effect
                    .as_ref()
                    .map_or(3.0, |e| view::radius_of(&e.model));
                // far enough for a large model, up to 60 yards, aimed at its middle
                self.view.distance = (radius * 2.6).clamp(4.0, 60.0);
                self.view.target = Vec3::new(0.0, 0.0, radius * 0.6);
            }
            ui.checkbox(&mut self.view.ground, "Ground")
                .on_hover_text("A neutral ground with a grid of a yard");
            ui.add(egui::Slider::new(&mut self.view.slope, 0.0..=30.0).text("slope °"))
                .on_hover_text("The ground rising along X: what is laid on it, what cuts into it");
            ui.checkbox(&mut self.view.character, "Character")
                .on_hover_text("The reference character (the client's human male)");
            ui.checkbox(&mut self.view.points, "Points")
                .on_hover_text("Its attachment points; the effect stands on the one chosen in the Effect panel");
            let mut background = self.view.background;
            if egui::color_picker::color_edit_button_rgb(ui, &mut background)
                .on_hover_text("The background")
                .changed()
            {
                self.view.background = background;
            }
        });
        let (Some(resources), Some(gpu), Some(target)) = (self.gpu.clone(), ctx.gpu().cloned(), self.target) else {
            ui.weak("No GPU: the Effect view is not drawn.");
            return;
        };
        let gizmo = self.gizmo_point();
        let parts = self.part_points();
        let points: Vec<Vec3> = parts.iter().map(|(_, at)| *at).collect();
        let shown = self.view.show(
            ui,
            (&gpu, &resources, &target),
            &self.stage,
            self.character_view.as_ref(),
            gizmo,
            &points,
        );
        if let Some(moved) = shown.moved {
            self.gizmo_moved(ctx, moved);
        }
        if let Some(k) = shown.picked {
            self.selected = parts[k].0.clone();
        }
    }

    /// Where each part stands in the Effect view (its place, on the attachment point), for a click
    /// to choose it.
    fn part_points(&self) -> Vec<(Node, Vec3)> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let offset = lock(&self.stage).attach_offset;
        let root = &doc.value;
        let place = |part: &json::Value| -> Vec3 {
            match part.get("pos").and_then(json::Value::as_list) {
                Some([x, y, z]) => Vec3::new(
                    x.as_f64().unwrap_or(0.0) as f32,
                    y.as_f64().unwrap_or(0.0) as f32,
                    z.as_f64().unwrap_or(0.0) as f32,
                ),
                _ => Vec3::ZERO,
            }
        };
        let mut out = Vec::new();
        for (list, make) in [
            ("emitters", Node::Emitter as fn(usize) -> Node),
            ("ribbons", Node::Ribbon),
            ("meshes", Node::Mesh),
            ("lights", Node::Light),
        ] {
            for (i, part) in root
                .get(list)
                .and_then(json::Value::as_list)
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                out.push((make(i), place(part) + offset));
            }
        }
        for (name, part) in root.get("bones").and_then(json::Value::as_map).unwrap_or_default() {
            out.push((Node::Bone(name.clone()), place(part) + offset));
        }
        out
    }

    /// The part chosen shown in the Timeline: presented again when it or the description changed,
    /// the user's changes of its keys brought back into the description, the playhead followed.
    fn sync_timeline(&mut self, ctx: &mut Context) {
        let Some(timeline) = ctx.service(uniwow_api::timeline::SERVICE) else {
            return;
        };
        let owner = ctx.module_id().to_owned();
        let Some(doc) = self.doc.as_ref() else {
            if self.presented.take().is_some() {
                timeline.withdraw(&owner);
            }
            lock(&self.stage).driven = false;
            return;
        };
        let current = (self.selected.clone(), doc.value.clone());
        if self.presented.as_ref() != Some(&current) {
            self.presented_version += 1;
            let name = doc.value.get("name").and_then(json::Value::as_str).unwrap_or("effect");
            let part = match &self.selected {
                Node::Effect => String::new(),
                other => format!(
                    " · {}",
                    format!("{other:?}").to_lowercase().replace(['(', ')', '"'], " ").trim()
                ),
            };
            let presented = link::present(
                &doc.value,
                &self.selected,
                &format!("{name}{part}"),
                self.presented_version,
            );
            timeline.present(&owner, presented);
            self.presented = Some(current);
        }
        if std::mem::take(&mut self.focus_timeline) {
            timeline.focus(&owner);
        }
        let mut changed = false;
        if let Some((version, edited)) = timeline.take_edit(&owner)
            && version == self.presented_version
            && let Some(after) = link::apply(&doc.value, &self.selected, &edited)
        {
            match self.timeline_gesture {
                Some(gesture) if ctx.resume_gesture(gesture) => {}
                _ => self.timeline_gesture = Some(ctx.begin_gesture("effect: keys changed in the Timeline")),
            }
            ctx.execute(Edit {
                label: "effect: keys changed in the Timeline".into(),
                before: doc.value.clone(),
                after,
            });
            changed = true;
        }
        if !changed && let Some(gesture) = self.timeline_gesture.take() {
            ctx.end_gesture(gesture);
        }
        let mut stage = lock(&self.stage);
        match timeline.playhead(&owner) {
            Some(playhead) => {
                stage.driven = true;
                stage.holds = 1;
                stage.time = playhead.time;
            }
            None => stage.driven = false,
        }
    }

    /// The particle's values over its life (colour, alpha, size) of the emitter chosen, as curves,
    /// the life from 0 to 1; 8 keys at most, as the client's.
    fn life_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Node::Emitter(_) = self.selected else {
            return;
        };
        let (Some(doc), Some(editor)) = (self.doc.as_ref(), ctx.service(uniwow_api::curve::SERVICE)) else {
            return;
        };
        let Some(part) = self.selected.get(&doc.value) else {
            return;
        };
        ui.separator();
        ui.label("Over a particle's life (8 keys at most)");
        let mut after = None;
        for (key, colour, default, label) in [
            ("colors", true, "[[0, \"#ffffff\"], [1, \"#ffffff\"]]", "colour"),
            ("alpha", false, "[[0, 0], [0.15, 1], [1, 0]]", "alpha"),
            ("size", false, "0.5", "size"),
        ] {
            let value = part
                .get(key)
                .cloned()
                .unwrap_or_else(|| json::parse(default).unwrap_or(json::Value::Null));
            let value = match value {
                json::Value::Str(c) if colour => json::Value::List(vec![
                    json::Value::List(vec![json::Value::Int(0), json::Value::Str(c.clone())]),
                    json::Value::List(vec![json::Value::Int(1), json::Value::Str(c)]),
                ]),
                other => other,
            };
            let names = if colour {
                vec!["red", "green", "blue"]
            } else {
                vec![label]
            };
            let mut shown: Vec<uniwow_api::curve::ShownCurve> = link::life_curves(&value, colour)
                .into_iter()
                .zip(names)
                .enumerate()
                .map(|(n, (curve, name))| uniwow_api::curve::ShownCurve {
                    label: name.to_owned(),
                    colour: if colour {
                        uniwow_api::sequence::number_colour(PropertyKind::Colour, n)
                    } else {
                        [230, 160, 40]
                    },
                    curve,
                    visible: true,
                })
                .collect();
            let axis = self
                .life_axes
                .entry(key.to_owned())
                .or_insert(uniwow_api::curve::TimeAxis {
                    first: -0.05,
                    pixels_per_unit: 300.0,
                });
            ui.label(label);
            let output = ui
                .allocate_ui(egui::vec2(ui.available_width(), 110.0), |ui| {
                    editor.show(
                        ui,
                        egui::Id::new(("effects life", key)),
                        &mut shown,
                        axis,
                        &uniwow_api::curve::CurveOptions {
                            snap: None,
                            playhead: None,
                            span: Some([0.0, 1.0]),
                        },
                    )
                })
                .inner;
            let before = link::life_curves(&value, colour);
            let curves: Vec<_> = shown.into_iter().map(|s| s.curve).collect();
            // written only when a key really moved, appeared or went
            let moved = curves.iter().zip(&before).any(|(a, b)| {
                a.keys.len() != b.keys.len()
                    || a.keys
                        .iter()
                        .zip(&b.keys)
                        .any(|(x, y)| (x.time - y.time).abs() > 1e-6 || (x.value - y.value).abs() > 1e-6)
            });
            if output.change != uniwow_api::curve::CurveChange::None && moved {
                match link::life_value(&curves, colour) {
                    Some(new) if Some(&new) != part.get(key) => after = Some((key, new)),
                    Some(_) => {}
                    None => {
                        self.error = Some(format!(
                            "{label}: {} keys at most over a particle's life",
                            link::LIFE_KEYS
                        ))
                    }
                }
            }
        }
        if let Some((key, new)) = after {
            let before = doc.value.clone();
            let mut value = before.clone();
            if let Some(part) = self.selected.get_mut(&mut value) {
                part.set(key, new);
            }
            ctx.execute(Edit {
                label: "effect: a particle's life changed".into(),
                before,
                after: value,
            });
        }
    }

    /// A message of forge: clicked, it chooses the part it names.
    fn message(&mut self, ui: &mut egui::Ui, text: &str, error: bool) {
        let colour = if error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().weak_text_color()
        };
        match Node::named_in(text).filter(|_| self.doc.is_some()) {
            Some(node) => {
                let link = ui.add(
                    egui::Label::new(egui::RichText::new(text).color(colour).underline()).sense(egui::Sense::click()),
                );
                if link.on_hover_text("Shows this part in the inspector").clicked() {
                    self.selected = node;
                }
            }
            None => {
                ui.colored_label(colour, text);
            }
        }
    }

    /// Shows an effect loaded: from its start when `first`, at the same time when it was only made
    /// again after a change.
    fn show(&mut self, ctx: &mut Context, mut loaded: Loaded, first: bool) {
        self.version += 1;
        loaded.version = self.version;
        self.error = None;
        {
            let mut stage = lock(&self.stage);
            stage.effect = Some(Arc::new(loaded));
            stage.textures = None;
            if first {
                stage.time = 0.0;
                stage.playing = true;
            }
        }
        if first {
            // the effects' panels laid out, the view framing the effect, the Timeline on it
            if !self.arranged {
                self.arrange(ctx);
            }
            if let Some(effect) = lock(&self.stage).effect.clone() {
                self.view.frame(view::radius_of(&effect.model));
            }
            self.focus_timeline = true;
            if !self.placed {
                self.origin = camera_target(ctx);
            }
            self.place_character(ctx);
            if let Some(view) = self.start_view.take() {
                self.look(ctx, view);
            }
            if let Some(time) = self.start_time.take() {
                let mut stage = lock(&self.stage);
                stage.time = time;
                stage.playing = false;
            }
        }
        self.upload_textures(ctx);
    }

    /// Makes the model again from the description, in a job; one at a time, the last change made
    /// once the one running is done.
    fn reforge(&mut self, ctx: &mut Context, first: bool) {
        if self.forge_job.is_some() {
            return;
        }
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.stale = false;
        let spec = doc.value.clone();
        let dir = doc
            .path
            .as_ref()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_default();
        let source = doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let vfs = ctx.service(vfs::SERVICE);
        self.forge_job = Some(ctx.spawn("Make the effect's model", move |_| {
            (first, load::forged(&spec, &dir, &source, vfs.as_deref()))
        }));
    }

    /// The examples of WoW-mods' tools, by name and path.
    fn examples() -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(EXAMPLES)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "json"))
                    .filter(|p| p.file_stem().is_some_and(|s| s != "lab"))
                    .map(|p| {
                        (
                            p.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                            p.display().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// The layout of the effects: their view in the middle, their tree on the left, the part chosen
    /// on the right, the Timeline at the bottom; the layout before kept for "Back to the world".
    fn arrange(&mut self, ctx: &mut Context) {
        let own = |panel: &str| ("effects".to_owned(), panel.to_owned());
        ctx.arrange(Arrangement {
            center: vec![own("view")],
            left: vec![own("effects")],
            right: vec![own("inspector")],
            bottom: vec![("timeline".to_owned(), "timeline".to_owned())],
        });
        self.arranged = true;
    }

    /// A new effect: a glow rising, to start from; not saved until asked.
    fn new_effect(&mut self, ctx: &mut Context) {
        let value = json::parse(NEW_EFFECT).expect("written here");
        self.doc = Some(Doc::new(
            None,
            value,
            json::Layout {
                crlf: true,
                final_newline: true,
            },
        ));
        self.selected = Node::Emitter(0);
        self.reforge(ctx, true);
    }

    /// When nothing is open: what to do, in three buttons, and what each panel is for.
    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading("Spell effects");
            ui.label("Particles, trails and shapes, as the 3.3.5a client draws them.");
            ui.add_space(16.0);
            let big = |ui: &mut egui::Ui, text: &str| ui.add_sized([260.0, 40.0], egui::Button::new(egui::RichText::new(text).size(16.0)));
            if big(ui, "✚  New effect").on_hover_text("Starts from a glow rising").clicked() {
                self.new_effect(ctx);
            }
            ui.add_space(6.0);
            let examples = Self::examples();
            ui.menu_button(egui::RichText::new("★  Open an example").size(16.0), |ui| {
                if examples.is_empty() {
                    ui.weak("No example found in WoW-mods\\tools\\vfx\\examples");
                }
                for (name, path) in &examples {
                    if ui.button(name).clicked() {
                        self.open(ctx, Some(path.clone()));
                        ui.close();
                    }
                }
            });
            ui.add_space(6.0);
            if big(ui, "📂  Open a file…").on_hover_text("An effect's description (.json) or a model (.m2)").clicked() {
                self.open(ctx, None);
            }
            ui.add_space(20.0);
            ui.weak("Effect view: the effect in 3D, played. Effect (left): its parts. Part (right): the settings of the part chosen.");
            ui.weak("Timeline (bottom): its time, its phases and the keys of the part chosen. Menu Effects: the same, from anywhere.");
        });
    }

    /// The Effect panel: the file, the parts in a tree, forge's notes; the effect in the world.
    fn effect_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        if lock(&self.stage).effect.is_none() && self.doc.is_none() {
            self.welcome(ui, ctx);
            return;
        }
        if let Some(error) = self.error.clone() {
            self.message(ui, &error, true);
        }
        let mut save_as = false;
        let mut save = false;
        let mut world = false;
        if let Some(doc) = self.doc.as_ref() {
            ui.horizontal_wrapped(|ui| {
                let name = doc
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "new effect".into());
                if doc.unsaved() {
                    ui.label(
                        egui::RichText::new(format!("{name} *"))
                            .background_color(uniwow_api::scene::state_colour::highlight()),
                    )
                    .on_hover_text("Changed since it was saved");
                } else {
                    ui.label(name);
                }
                save = ui
                    .add_enabled(doc.unsaved(), egui::Button::new("Save"))
                    .on_hover_text("Writes the description (.json)")
                    .clicked();
                save_as = ui
                    .button("Save as…")
                    .on_hover_text("Writes it under another name")
                    .clicked();
                world = self.arranged
                    && ui
                        .button("⟲ Back to the world")
                        .on_hover_text("Puts the editor's panels back as they were")
                        .clicked();
            });
        } else {
            let name = lock(&self.stage)
                .effect
                .as_ref()
                .map(|e| e.name.clone())
                .unwrap_or_default();
            ui.label(format!("{name} (a model: seen, not edited)"));
        }
        if save || save_as {
            let path = if save_as {
                None
            } else {
                self.doc.as_ref().and_then(|d| d.path.clone())
            };
            if let Err(error) = self.save(path) {
                self.error = Some(error);
            }
        }
        if world {
            ctx.restore_arrangement();
            self.arranged = false;
        }
        let notes = lock(&self.stage)
            .effect
            .as_ref()
            .map(|e| e.notes.clone())
            .unwrap_or_default();
        for note in notes {
            self.message(ui, &format!("note: {note}"), false);
        }
        ui.separator();
        if let Some(doc) = self.doc.as_ref() {
            let mut change = None;
            egui::ScrollArea::vertical()
                .id_salt("effects tree")
                .max_height(ui.available_height() - 90.0)
                .show(ui, |ui| {
                    change = inspector::tree(ui, doc, &mut self.selected);
                });
            if let Some(change) = change {
                self.apply_change(ctx, change);
            }
        }
        ui.separator();
        egui::CollapsingHeader::new("In the world's 3D view")
            .id_salt("effects world")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("Here")
                        .on_hover_text("Puts the effect and the character where the 3D view's camera looks")
                        .clicked()
                    {
                        self.origin = camera_target(ctx);
                        self.place_character(ctx);
                    }
                    for (label, view) in [
                        ("Grazing", View::Grazing),
                        ("Top", View::Top),
                        ("Three-quarter", View::Three),
                    ] {
                        if ui
                            .button(label)
                            .on_hover_text("Moves the 3D view's camera about the effect")
                            .clicked()
                        {
                            self.look(ctx, view);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.checkbox(&mut self.character, "Character").changed() {
                        self.place_character(ctx);
                    }
                    let before = self.attach;
                    egui::ComboBox::from_id_salt("effects attach")
                        .selected_text(ATTACHMENTS[self.attach].0)
                        .show_ui(ui, |ui| {
                            for (index, (name, id)) in ATTACHMENTS.iter().enumerate() {
                                ui.selectable_value(&mut self.attach, index, format!("{name} ({id})"));
                            }
                        })
                        .response
                        .on_hover_text("The attachment point the effect is put on");
                    if ui.checkbox(&mut self.show_points, "Points").changed() || before != self.attach {
                        self.update_anchor();
                        self.sample_ground(ctx);
                    }
                    ui.checkbox(&mut lock(&self.stage).show_bones, "Bones");
                });
            });
    }

    /// The Part panel: the settings of the part chosen in the tree, and a particle's life.
    fn part_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        if self.doc.is_none() {
            ui.weak("Open or create an effect (menu Effects): the settings of the part chosen in its tree show here.");
            return;
        }
        egui::ScrollArea::vertical().id_salt("effects part").show(ui, |ui| {
            if let Some(doc) = self.doc.as_ref() {
                self.pending = inspector::inspector(ui, &mut self.inspector, doc, &self.selected);
            }
            self.life_ui(ui, ctx);
        });
        // a drag makes one entry of the history, however many frames it lasts
        let dragging = std::mem::take(&mut self.inspector.dragging);
        match (dragging, self.gesture) {
            (true, Some(gesture)) => {
                if !ctx.resume_gesture(gesture) {
                    self.gesture = Some(ctx.begin_gesture("effect: value dragged"));
                }
            }
            (true, None) => self.gesture = Some(ctx.begin_gesture("effect: value dragged")),
            (false, Some(gesture)) => {
                ctx.end_gesture(gesture);
                self.gesture = None;
            }
            (false, None) => {}
        }
        if let Some(change) = self.pending.take() {
            self.apply_change(ctx, change);
        }
    }

    /// A change of the description, one entry of the history.
    fn apply_change(&mut self, ctx: &mut Context, change: inspector::Change) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let before = doc.value.clone();
        if let Some(select) = change.select {
            self.selected = select;
        }
        ctx.execute(Edit {
            label: change.label,
            before,
            after: change.value,
        });
    }
    /// Saves the description to `path`, or to a file chosen.
    fn save(&mut self, path: Option<PathBuf>) -> Result<(), String> {
        let Some(doc) = self.doc.as_mut() else {
            return Ok(());
        };
        let path = match path {
            Some(path) => path,
            None => {
                let name = doc
                    .value
                    .get("name")
                    .and_then(json::Value::as_str)
                    .unwrap_or("vfx")
                    .to_owned();
                let Some(path) = uniwow_api::rfd::FileDialog::new()
                    .set_title("Save the effect's description")
                    .add_filter("Effect description", &["json"])
                    .set_file_name(format!("{name}.json"))
                    .save_file()
                else {
                    return Ok(());
                };
                path
            }
        };
        doc.save(path)
    }

    /// Uploads the textures of the effect shown, once the GPU resources are there.
    fn upload_textures(&mut self, ctx: &mut Context) {
        let (Some(resources), Some(gpu)) = (self.gpu.clone(), ctx.gpu().cloned()) else {
            return;
        };
        let Some(effect) = lock(&self.stage).effect.clone() else {
            return;
        };
        self.textures_job = Some(ctx.spawn("Upload an effect's textures", move |_| {
            let groups = effect
                .images
                .iter()
                .zip(&effect.model.textures)
                .map(|(image, texture)| {
                    image.as_ref().map(|image| {
                        gpu::texture_group(
                            &gpu.device,
                            &gpu.queue,
                            &resources.texture_layout,
                            &resources.samplers,
                            image,
                            texture.flags,
                        )
                    })
                })
                .collect::<Vec<_>>();
            (effect.version, groups)
        }));
    }

    /// The reference character at the effect's origin (or taken away), and its attachment points.
    fn place_character(&mut self, ctx: &mut Context) {
        self.placed = true;
        if let Some(models) = ctx.service(models::SERVICE) {
            if self.character {
                let look = models.look(&models::Look {
                    model: uniwow_api::formats::FileRef::Path(CHARACTER.into()),
                    textures: vec![(1, uniwow_api::formats::FileRef::Path(CHARACTER_SKIN.into()))],
                    geosets: models::Geosets::Character {
                        hair: 0,
                        facial: [0; 5],
                    },
                });
                models.place(
                    ctx.module_id(),
                    &[models::Instance {
                        id: 1,
                        look,
                        transform: Mat4::from_translation(self.origin),
                        alpha: 1.0,
                        motion: models::Motion::Standing,
                        interior: None,
                    }],
                );
            } else {
                models.clear(ctx.module_id());
            }
        }
        if self.character && self.points.is_empty() && self.character_job.is_none() {
            let vfs = ctx.service(vfs::SERVICE);
            self.character_job = Some(ctx.spawn("Read the reference character's attachment points", move |_| {
                load::attachment_points(vfs.as_deref(), CHARACTER)
            }));
        }
        self.update_anchor();
        self.sample_ground(ctx);
    }

    /// The heights of the ground about the effect, read by a job from the scene's terrain.
    fn sample_ground(&mut self, ctx: &mut Context) {
        let Some(scene) = ctx.service(scene::SERVICE) else {
            return;
        };
        let anchor = lock(&self.stage).anchor;
        let job = ctx.spawn("Read the ground under the effect", move |_| {
            ground_under(scene.as_ref(), anchor)
        });
        self.ground_job = Some((job, anchor));
    }

    /// The effect on the attachment point chosen of the character (at its origin without it), and
    /// the points shown.
    fn update_anchor(&mut self) {
        let id = ATTACHMENTS[self.attach].1;
        let on = if self.character {
            self.points.iter().find(|(p, _)| *p == id).map(|(_, at)| *at)
        } else {
            None
        };
        let mut stage = lock(&self.stage);
        stage.anchor = self.origin + on.unwrap_or(Vec3::ZERO);
        stage.attach_id = id;
        stage.attach_offset = self
            .character_view
            .as_ref()
            .and_then(|c| c.points.iter().find(|(p, _)| *p == id).map(|(_, at)| *at))
            .unwrap_or(Vec3::ZERO);
        stage.markers = if self.character && self.show_points {
            self.points
                .iter()
                .filter(|(p, _)| ATTACHMENTS.iter().any(|(_, a)| a == p))
                .map(|(p, at)| {
                    let color = if *p == id {
                        [1.0, 0.8, 0.1, 1.0]
                    } else {
                        [0.3, 0.8, 1.0, 0.8]
                    };
                    ((self.origin + *at).to_array(), color)
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    fn look(&self, ctx: &mut Context, view: View) {
        let target = self.origin + Vec3::new(0.0, 0.0, 1.0);
        let eye = match view {
            View::Grazing => self.origin + Vec3::new(7.0, 0.0, 0.4),
            View::Top => self.origin + Vec3::new(0.01, 0.0, 9.0),
            View::Three => self.origin + Vec3::new(5.0, 5.0, 4.0),
        };
        log::info!("effects: the camera at {eye}, looking at {target}");
        let vector = |v: Vec3| PropertyValue::Vector([f64::from(v.x), f64::from(v.y), f64::from(v.z)]);
        for (path, value) in [("viewport/camera_position", eye), ("viewport/camera_target", target)] {
            if let Err(error) = ctx.write_property(path, vector(value)) {
                log::warn!("{path}: {error}");
            }
        }
    }
}

uniwow_api::export_module!(EffectsModule::default());
