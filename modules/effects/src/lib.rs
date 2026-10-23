//! Spell effects in the 3D view: an effect description of WoW-mods (`forge`) made into its model
//! by `uniwow_api::vfx`, or a model of the client (`Spells\*.m2`), played in time: its bones, its
//! meshes, its particles and ribbons, as the client draws them. A reference character stands where
//! the effect plays, its attachment points shown, the effect on the one chosen. The time is an
//! animatable property (`effects/time`), so that the timeline can drive it.

mod doc;
mod gpu;
mod inspector;
mod layer;
mod load;
mod schema;

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
    Context, DockArea, JobId, JobOutcome, Module, PropertyKind, PropertyValue, Registrar, egui, log, models, scene,
    vfs, viewport, wgpu,
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
    /// Points drawn as dots, in the world: the attachment points of the reference.
    pub markers: Vec<([f32; 3], [f32; 4])>,
    /// The ground under the effect, in its space, for what the client lays on it.
    pub ground: Option<Arc<Ground>>,
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
            markers: Vec::new(),
            ground: None,
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
    /// A model of the client to open, as typed.
    client_path: String,
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

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if !self.drawn {
            ui.colored_label(ui.visuals().warn_fg_color, "No 3D view: the effects are not drawn.");
        }
        ui.horizontal(|ui| {
            if ui
                .button("Open…")
                .on_hover_text("A description of forge (.json) or a model (.m2) on the disk")
                .clicked()
            {
                self.open(ctx, None);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.client_path)
                    .hint_text("Spells\\X.m2")
                    .desired_width(160.0),
            );
            if ui.button("Open from the client").clicked() && !self.client_path.trim().is_empty() {
                let path = self.client_path.trim().to_owned();
                self.open(ctx, Some(path));
            }
        });
        if self.doc.as_ref().is_some_and(|d| d.stale) {
            self.reforge(ctx, false);
        }
        if let Some(error) = self.error.clone() {
            self.message(ui, &error, true);
        }
        let (name, notes, alive_text) = {
            let stage = lock(&self.stage);
            (
                stage.effect.as_ref().map(|e| format!("{} ({})", e.name, e.source)),
                stage.effect.as_ref().map(|e| e.notes.clone()).unwrap_or_default(),
                format!("{:.2} / {:.2} s", stage.time, stage.length),
            )
        };
        let Some(name) = name else {
            ui.weak("No effect open.");
            return;
        };
        ui.label(name);
        for note in notes {
            self.message(ui, &format!("note: {note}"), false);
        }
        ui.separator();
        {
            let mut stage = lock(&self.stage);
            ui.horizontal(|ui| {
                let label = if stage.playing { "Pause" } else { "Play" };
                if ui.button(label).clicked() {
                    stage.playing = !stage.playing;
                    if stage.playing && stage.time >= stage.length {
                        stage.time = 0.0;
                    }
                }
                if ui.button("⏮").on_hover_text("Back to the start").clicked() {
                    stage.time = 0.0;
                }
                ui.checkbox(&mut stage.looping, "Loop");
                ui.label(alive_text);
            });
            let length = stage.length.max(0.001);
            if ui
                .add(egui::Slider::new(&mut stage.time, 0.0..=length).text("time (s)"))
                .changed()
            {
                stage.playing = false;
            }
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut stage.speed, 0.05..=2.0).text("speed"));
            });
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut stage.holds).range(1..=20))
                    .on_hover_text("How many times the hold of a state model loops (birth, hold, decay)");
                ui.label("holds");
                ui.checkbox(&mut stage.show_bones, "Bones");
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .button("Here")
                .on_hover_text("Puts the effect and the character where the camera looks")
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
                if ui.button(label).clicked() {
                    self.look(ctx, view);
                }
            }
        });
        ui.horizontal(|ui| {
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
                });
            if ui.checkbox(&mut self.show_points, "Points").changed() || before != self.attach {
                self.update_anchor();
                self.sample_ground(ctx);
            }
        });
        if self.doc.is_some() {
            ui.separator();
            self.edit_ui(ui, ctx);
        }
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

    /// The description's editor: saving, the tree of its parts, the inspector of the one chosen.
    fn edit_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let mut save_as = false;
        let mut save = false;
        ui.horizontal(|ui| {
            let name = doc
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "new".into());
            if doc.unsaved() {
                ui.label(
                    egui::RichText::new(format!("{name} *"))
                        .background_color(uniwow_api::scene::state_colour::highlight()),
                )
                .on_hover_text("Changed since it was saved");
            } else {
                ui.label(name);
            }
            save = ui.add_enabled(doc.unsaved(), egui::Button::new("Save")).clicked();
            save_as = ui.button("Save as…").clicked();
        });
        if save || save_as {
            let path = if save_as { None } else { doc.path.clone() };
            if let Err(error) = self.save(path) {
                self.error = Some(error);
            }
        }
        let doc = self.doc.as_ref().expect("still there");
        let mut change = None;
        ui.columns(2, |columns| {
            egui::ScrollArea::vertical()
                .id_salt("effects tree")
                .max_height(420.0)
                .show(&mut columns[0], |ui| {
                    change = inspector::tree(ui, doc, &mut self.selected);
                });
            egui::ScrollArea::vertical()
                .id_salt("effects inspector")
                .max_height(420.0)
                .show(&mut columns[1], |ui| {
                    if let Some(c) = inspector::inspector(ui, &mut self.inspector, doc, &self.selected) {
                        change = Some(c);
                    }
                });
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
        if let Some(change) = change {
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
