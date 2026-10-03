//! Sample feature: lists every event it receives and asks for the cube to be repainted by
//! publishing `sample.paint`. It does not know which feature, if any, answers.

use std::collections::VecDeque;
use std::time::Instant;

use uniwow_api::serde::Serialize;
use uniwow_api::{Context, DockArea, Event, Feature, Registrar, egui};

const MAX_EVENTS: usize = 200;

struct NotesFeature {
    start: Instant,
    received: VecDeque<(f32, Event)>,
    /// Set by the "Simulate a failure" button; the next draw panics.
    fail_next_draw: bool,
}

impl Default for NotesFeature {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            received: VecDeque::new(),
            fail_next_draw: false,
        }
    }
}

impl Feature for NotesFeature {
    fn register(&mut self, reg: &mut Registrar) {
        reg.panel("events", "Events", DockArea::Left).subscribe("*");
    }

    fn panel_ui(&mut self, _panel: &str, ui: &mut egui::Ui, ctx: &mut Context) {
        if self.fail_next_draw {
            panic!("simulated failure requested from the Events panel");
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Paint the cube gold").clicked() {
                ctx.publish_as(
                    "sample.paint",
                    &Paint {
                        color: [1.0, 0.72, 0.18],
                    },
                );
            }
            if ui.button("Random colour").clicked() {
                ctx.publish_as(
                    "sample.paint",
                    &Paint {
                        color: self.random_color(),
                    },
                );
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Simulate a failure").clicked() {
                self.fail_next_draw = true;
            }
            if ui.button("Clear").clicked() {
                self.received.clear();
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (seconds, event) in &self.received {
                    ui.label(format!("{seconds:>7.2}  {}  from {}", event.topic, event.source));
                    ui.weak(format!("         {}", event.payload));
                }
            });
    }

    fn on_event(&mut self, event: &Event, _ctx: &mut Context) {
        if self.received.len() == MAX_EVENTS {
            self.received.pop_front();
        }
        self.received
            .push_back((self.start.elapsed().as_secs_f32(), event.clone()));
    }
}

impl NotesFeature {
    fn random_color(&self) -> [f32; 3] {
        let seed = self.start.elapsed().as_nanos() as u64;
        let channel = |shift: u32| ((seed.rotate_left(shift).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as f32) / 255.0;
        [channel(0), channel(21), channel(42)]
    }
}

/// Payload of `sample.paint`, as this feature writes it. Whoever answers declares its own type.
#[derive(Serialize)]
#[serde(crate = "uniwow_api::serde")]
struct Paint {
    color: [f32; 3],
}

uniwow_api::export_feature!(NotesFeature::default());
