//! Sample module: a cube drawn through the viewport service.
//!
//! Its colour changes through undoable commands, from its own panel, when another module
//! publishes `sample.paint`, or through the named command `cube.paint`. It never names the
//! modules that ask. `cube.color` answers on the calling thread, and the cube's GPU resources are
//! built in a job. Its position, rotation, scale and colour are animatable properties.

mod layer;

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::serde::{Deserialize, Serialize};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport;
use uniwow_api::{
    Command, Context, DockArea, Event, JobId, JobOutcome, MODULE_FAILED_TOPIC, Module, PropertyKind, PropertyValue,
    Registrar, decode_arguments, egui, log,
};

use layer::{CubeLayer, Gpu};

pub struct Params {
    pub color: [f32; 3],
    /// Centre of the cube.
    pub position: [f32; 3],
    /// Euler angles in degrees, applied around x, then y, then z.
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

/// Locks shared state even if a panic poisoned it: the values stay usable.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

const PRESETS: [(&str, [f32; 3]); 4] = [
    ("Red", [0.8, 0.12, 0.1]),
    ("Green", [0.15, 0.65, 0.2]),
    ("Blue", [0.15, 0.3, 0.85]),
    ("Gold", [1.0, 0.72, 0.18]),
];

struct CubeModule {
    params: Arc<Mutex<Params>>,
    /// Filled by the job building the GPU resources, emptied by the layer.
    gpu: Arc<Mutex<Option<Gpu>>>,
    gpu_job: Option<JobId>,
    drawn: bool,
}

impl Default for CubeModule {
    fn default() -> Self {
        Self {
            params: Arc::new(Mutex::new(Params {
                color: PRESETS[2].1,
                position: [0.0, 0.0, 1.0],
                rotation: [0.0; 3],
                scale: [1.0; 3],
            })),
            gpu: Arc::default(),
            gpu_job: None,
            drawn: false,
        }
    }
}

impl Module for CubeModule {
    fn register(&mut self, reg: &mut Registrar) {
        let params = self.params.clone();
        reg.panel("cube", "Cube", DockArea::Right)
            .subscribe("sample.paint")
            .subscribe(MODULE_FAILED_TOPIC)
            .command(
                "cube.paint",
                "Paints the cube; undoable. Painting it the colour it has changes nothing.",
                json!({ "type": "object", "properties": { "color": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 } }, "required": ["color"] }),
                json!({ "type": "object", "properties": { "color": {}, "changed": { "type": "boolean" } } }),
            )
            .command_on_caller(
                "cube.color",
                "The colour of the cube, read on the calling thread.",
                json!({ "type": "object" }),
                json!({ "type": "object", "properties": { "color": {} } }),
                Arc::new(move |_| Ok(json!({ "color": lock(&params).color }))),
            );
        let vector = |reg: &mut Registrar, name: &str, label: &str, range: [f64; 2], field: Field| {
            let kind = if name == "colour" {
                PropertyKind::Colour
            } else {
                PropertyKind::Vector
            };
            let (read, write) = (self.params.clone(), self.params.clone());
            reg.animatable(
                name,
                label,
                kind,
                range,
                move || PropertyValue::from_components(kind, &field(&mut lock(&read)).map(widen)),
                move |value| {
                    let numbers = value.components();
                    *field(&mut lock(&write)) = [numbers[0] as f32, numbers[1] as f32, numbers[2] as f32];
                },
            );
        };
        vector(reg, "position", "Position", [-100.0, 100.0], |p| &mut p.position);
        vector(reg, "rotation", "Rotation", [-3600.0, 3600.0], |p| &mut p.rotation);
        vector(reg, "scale", "Scale", [0.01, 100.0], |p| &mut p.scale);
        vector(reg, "colour", "Colour", [0.0, 1.0], |p| &mut p.color);
    }

    fn init(&mut self, ctx: &mut Context) {
        let Some(view) = ctx.service(viewport::SERVICE) else {
            log::info!("no viewport service: the cube is not drawn");
            return;
        };
        let Some(gpu) = ctx.gpu().cloned() else {
            return;
        };
        view.add_layer(
            ctx.module_id(),
            Box::new(CubeLayer::new(self.params.clone(), self.gpu.clone())),
        );
        self.drawn = true;
        let target = view.target();
        self.gpu_job = Some(ctx.spawn("Build the cube's GPU resources", move |_| {
            layer::create(&gpu.device, &target)
        }));
    }

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        if Some(job) != self.gpu_job {
            return;
        }
        match outcome {
            JobOutcome::Panicked(message) => log::error!("the cube's GPU resources could not be built: {message}"),
            outcome => *lock(&self.gpu) = outcome.take::<Gpu>(),
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if !self.drawn {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No 3D view: the viewport module is not running, so the cube is not drawn.",
            );
            ui.separator();
        }
        let color = lock(&self.params).color;
        ui.horizontal(|ui| {
            ui.label("Colour");
            let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 18.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, to_color32(color));
        });
        ui.horizontal_wrapped(|ui| {
            for (name, preset) in PRESETS {
                if ui.button(name).clicked() {
                    self.paint(ctx, preset, name);
                }
            }
        });
        ui.separator();
        ui.weak("Ctrl+Z / Ctrl+Y undo and redo these changes.");
    }

    fn on_event(&mut self, event: &Event, ctx: &mut Context) {
        if event.topic == MODULE_FAILED_TOPIC {
            // The failed module may be the viewport: its service is then withdrawn.
            if self.drawn && ctx.service(viewport::SERVICE).is_none() {
                self.drawn = false;
            }
            return;
        }
        match event.decode::<Paint>() {
            Ok(paint) => self.paint(ctx, paint.color, &format!("requested by {}", event.source)),
            Err(error) => log::warn!("ignored: {error}"),
        }
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        match name {
            "cube.paint" => {
                let paint: Paint = decode_arguments(&arguments)?;
                let changed = lock(&self.params).color != paint.color;
                if changed {
                    self.paint(ctx, paint.color, "requested by a command");
                }
                Ok(json!({ "color": paint.color, "changed": changed }))
            }
            _ => Err(format!("'{name}' is not a command of the cube")),
        }
    }
}

impl CubeModule {
    fn paint(&mut self, ctx: &mut Context, color: [f32; 3], reason: &str) {
        ctx.execute(SetColor::new(color));
        let painted = Painted {
            color,
            reason: reason.to_owned(),
        };
        ctx.publish_as("sample.cube_painted", &painted);
    }
}

/// Payload of `sample.paint` and arguments of `cube.paint`, as this module reads them.
#[derive(Deserialize)]
#[serde(crate = "uniwow_api::serde")]
struct Paint {
    color: [f32; 3],
}

/// Payload of `sample.cube_painted`.
#[derive(Serialize)]
#[serde(crate = "uniwow_api::serde")]
struct Painted {
    color: [f32; 3],
    reason: String,
}

/// One of the vectors of `Params`.
type Field = fn(&mut Params) -> &mut [f32; 3];

/// The number an `f32` was written as: 0.15, not 0.15000000596046448.
fn widen(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

fn to_color32(c: [f32; 3]) -> egui::Color32 {
    egui::Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

fn cube(module: &mut dyn Any) -> &mut CubeModule {
    module
        .downcast_mut()
        .expect("commands of this module are applied to it")
}

/// Paints the cube. The colour it replaces is read when applied (see `Command`).
struct SetColor {
    new: [f32; 3],
    old: Option<[f32; 3]>,
}

impl SetColor {
    fn new(color: [f32; 3]) -> Self {
        Self { new: color, old: None }
    }
}

impl Command for SetColor {
    fn label(&self) -> String {
        "cube colour".to_owned()
    }

    fn apply(&mut self, module: &mut dyn Any) {
        let mut params = lock(&cube(module).params);
        self.old = Some(params.color);
        params.color = self.new;
    }

    fn revert(&mut self, module: &mut dyn Any) {
        if let Some(old) = self.old {
            lock(&cube(module).params).color = old;
        }
    }
}

uniwow_api::export_module!(CubeModule::default());

#[cfg(test)]
mod tests {
    use uniwow_api::{Command, Module, PropertyKind, PropertyValue, Registrar};

    use super::{CubeModule, PRESETS, SetColor, lock};

    #[test]
    fn paints_applied_in_one_pass_undo_back_to_where_they_started() {
        let mut cube = CubeModule::default();
        let start = lock(&cube.params).color;
        let (red, gold) = (PRESETS[0].1, PRESETS[3].1);
        // Both queued before either is applied, as when two events arrive in the same frame.
        let mut first = SetColor::new(red);
        let mut second = SetColor::new(gold);
        first.apply(&mut cube);
        second.apply(&mut cube);
        second.revert(&mut cube);
        assert_eq!(lock(&cube.params).color, red, "one undo gives the first paint back");
        first.revert(&mut cube);
        assert_eq!(lock(&cube.params).color, start, "two undos give the starting colour");
    }

    #[test]
    fn its_position_rotation_scale_and_colour_are_animatable() {
        let mut cube = CubeModule::default();
        let mut reg = Registrar::default();
        cube.register(&mut reg);
        let names: Vec<&str> = reg.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["position", "rotation", "scale", "colour"]);
        let scale = &reg.properties[2];
        (scale.write)(PropertyValue::Vector([2.0, 1.0, 0.5]));
        assert_eq!(lock(&cube.params).scale, [2.0, 1.0, 0.5]);
        assert_eq!((scale.read)(), PropertyValue::Vector([2.0, 1.0, 0.5]));
        assert_eq!(reg.properties[3].kind, PropertyKind::Colour);
        (reg.properties[3].write)(PropertyValue::Colour([0.15, 0.3, 0.85]));
        assert_eq!((reg.properties[3].read)(), PropertyValue::Colour([0.15, 0.3, 0.85]));
    }
}
