//! Sample feature: a cube drawn through the viewport service.
//!
//! Its colour and speed change through undoable commands, from its own panel or when another
//! feature publishes `sample.paint`. It never names that other feature.

mod layer;

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use uniwow_api::serde::{Deserialize, Serialize};
use uniwow_api::viewport;
use uniwow_api::{Command, Context, DockArea, Event, FEATURE_FAILED_TOPIC, Feature, Registrar, egui, log};

use layer::CubeLayer;

pub struct Params {
    pub color: [f32; 3],
    /// Turns per ten seconds.
    pub speed: f32,
}

const PRESETS: [(&str, [f32; 3]); 4] = [
    ("Red", [0.8, 0.12, 0.1]),
    ("Green", [0.15, 0.65, 0.2]),
    ("Blue", [0.15, 0.3, 0.85]),
    ("Gold", [1.0, 0.72, 0.18]),
];

struct CubeFeature {
    params: Rc<RefCell<Params>>,
    drawn: bool,
    /// Speed when the current slider drag started.
    drag_start_speed: Option<f32>,
}

impl Default for CubeFeature {
    fn default() -> Self {
        Self {
            params: Rc::new(RefCell::new(Params {
                color: PRESETS[2].1,
                speed: 1.0,
            })),
            drawn: false,
            drag_start_speed: None,
        }
    }
}

impl Feature for CubeFeature {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("cube", "Cube", DockArea::Right)
            .subscribe("sample.paint")
            .subscribe(FEATURE_FAILED_TOPIC);
    }

    fn init(&mut self, ctx: &mut Context) {
        match ctx.service::<viewport::Handle>(viewport::SERVICE) {
            Some(view) => {
                view.add_layer(ctx.feature_id(), Box::new(CubeLayer::new(self.params.clone())));
                self.drawn = true;
            }
            None => log::info!("no viewport service: the cube is not drawn"),
        }
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if !self.drawn {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No 3D view: the viewport feature is not running, so the cube is not drawn.",
            );
            ui.separator();
        }
        let color = self.params.borrow().color;
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

        let mut speed = self.params.borrow().speed;
        let response = ui.add(egui::Slider::new(&mut speed, 0.0..=4.0).text("Rotation speed"));
        if response.drag_started() {
            self.drag_start_speed = Some(self.params.borrow().speed);
        }
        if response.changed() {
            // Shown live while dragging; recorded as one command when the drag ends.
            self.params.borrow_mut().speed = speed;
            if !response.dragged() {
                let old = self.drag_start_speed.take().unwrap_or(speed);
                ctx.execute(SetSpeed { old, new: speed });
            }
        }
        if response.drag_stopped()
            && let Some(old) = self.drag_start_speed.take()
        {
            ctx.execute(SetSpeed { old, new: speed });
        }
        ui.separator();
        ui.weak("Ctrl+Z / Ctrl+Y undo and redo these changes.");
    }

    fn on_event(&mut self, event: &Event, ctx: &mut Context) {
        if event.topic == FEATURE_FAILED_TOPIC {
            // The failed feature may be the viewport: its service is then withdrawn.
            if self.drawn && ctx.service::<viewport::Handle>(viewport::SERVICE).is_none() {
                self.drawn = false;
            }
            return;
        }
        match event.decode::<Paint>() {
            Ok(paint) => self.paint(ctx, paint.color, &format!("requested by {}", event.source)),
            Err(error) => log::warn!("ignored: {error}"),
        }
    }
}

impl CubeFeature {
    fn paint(&mut self, ctx: &mut Context, color: [f32; 3], reason: &str) {
        let old = self.params.borrow().color;
        ctx.execute(SetColor { old, new: color });
        let painted = Painted {
            color,
            reason: reason.to_owned(),
        };
        ctx.publish_as("sample.cube_painted", &painted);
    }
}

/// Payload of `sample.paint`, as this feature reads it. The publisher declares its own type.
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

fn to_color32(c: [f32; 3]) -> egui::Color32 {
    egui::Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

fn cube(feature: &mut dyn Any) -> &mut CubeFeature {
    feature
        .downcast_mut()
        .expect("commands of this feature are applied to it")
}

struct SetColor {
    old: [f32; 3],
    new: [f32; 3],
}

impl Command for SetColor {
    fn label(&self) -> String {
        "cube colour".to_owned()
    }

    fn apply(&mut self, feature: &mut dyn Any) {
        cube(feature).params.borrow_mut().color = self.new;
    }

    fn revert(&mut self, feature: &mut dyn Any) {
        cube(feature).params.borrow_mut().color = self.old;
    }
}

struct SetSpeed {
    old: f32,
    new: f32,
}

impl Command for SetSpeed {
    fn label(&self) -> String {
        "cube speed".to_owned()
    }

    fn apply(&mut self, feature: &mut dyn Any) {
        cube(feature).params.borrow_mut().speed = self.new;
    }

    fn revert(&mut self, feature: &mut dyn Any) {
        cube(feature).params.borrow_mut().speed = self.old;
    }
}

uniwow_api::export_feature!(CubeFeature::default());
