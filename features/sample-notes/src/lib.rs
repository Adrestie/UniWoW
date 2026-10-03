//! Sample feature: lists every event it receives, asks for the cube to be repainted by publishing
//! `sample.paint`, and exercises the jobs and the named commands. It does not know which feature,
//! if any, answers.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use uniwow_api::serde::Serialize;
use uniwow_api::serde_json::json;
use uniwow_api::{Context, DockArea, Event, Feature, JobContext, JobId, JobOutcome, Registrar, egui};

const MAX_EVENTS: usize = 200;
const MAX_RESULTS: usize = 30;
/// Primes are counted below this bound by trial division: a few seconds of one core.
const PRIME_BOUND: u64 = 12_000_000;
const CALLER_CALLS: usize = 20_000;
const INTERFACE_CALLS: usize = 300;

#[derive(Clone, Copy)]
enum Kind {
    Computation,
    Panic,
    Publish,
    Measure,
}

struct NotesFeature {
    start: Instant,
    received: VecDeque<(f32, Event)>,
    /// Set by the "Simulate a failure" button; the next draw panics.
    fail_next_draw: bool,
    jobs: HashMap<JobId, (Kind, Instant)>,
    results: VecDeque<String>,
}

impl Default for NotesFeature {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            received: VecDeque::new(),
            fail_next_draw: false,
            jobs: HashMap::new(),
            results: VecDeque::new(),
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
            if ui.button("Simulate a failure").clicked() {
                self.fail_next_draw = true;
            }
        });
        ui.separator();
        ui.strong("Jobs");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Long computation").clicked() {
                self.start_job(ctx, Kind::Computation, "Count primes");
            }
            if ui.button("Fill every core").clicked() {
                let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
                for _ in 0..cores {
                    self.start_job(ctx, Kind::Computation, "Count primes");
                }
            }
            if ui.button("Panic in a job").clicked() {
                self.start_job(ctx, Kind::Panic, "Panic on purpose");
            }
            if ui.button("Publish from a job").clicked() {
                self.start_job(ctx, Kind::Publish, "Publish an event");
            }
            if ui.button("Measure calls").clicked() {
                self.start_job(ctx, Kind::Measure, "Measure command calls");
            }
        });
        for result in &self.results {
            ui.label(result);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Events");
            if ui.button("Clear").clicked() {
                self.received.clear();
                self.results.clear();
            }
        });
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

    fn on_job(&mut self, job: JobId, outcome: JobOutcome, _ctx: &mut Context) {
        let Some((kind, started)) = self.jobs.remove(&job) else {
            return;
        };
        let seconds = started.elapsed().as_secs_f32();
        let text = match outcome {
            JobOutcome::Cancelled => format!("job {}: cancelled after {seconds:.1} s", job.0),
            JobOutcome::Panicked(message) => format!("job {}: panicked: {message}", job.0),
            outcome => match kind {
                Kind::Computation => match outcome.take::<u64>() {
                    Some(primes) => format!("job {}: {primes} primes below {PRIME_BOUND} in {seconds:.1} s", job.0),
                    None => format!("job {}: no result", job.0),
                },
                Kind::Measure => outcome.take::<Result<String, String>>().map_or_else(
                    || "no result".to_owned(),
                    |r| r.unwrap_or_else(|e| format!("failed: {e}")),
                ),
                Kind::Publish => match outcome.take::<Result<(), String>>() {
                    Some(Err(error)) => format!("job {}: {error}", job.0),
                    _ => format!("job {}: done in {seconds:.2} s", job.0),
                },
                Kind::Panic => format!("job {}: done in {seconds:.2} s", job.0),
            },
        };
        if self.results.len() == MAX_RESULTS {
            self.results.pop_front();
        }
        self.results.push_back(text);
    }
}

impl NotesFeature {
    fn start_job(&mut self, ctx: &mut Context, kind: Kind, label: &str) {
        let id = match kind {
            Kind::Computation => ctx.spawn(label, count_primes),
            Kind::Panic => ctx.spawn(label, |_| -> () { panic!("this job panics on purpose") }),
            Kind::Publish => ctx.spawn(label, |context| {
                let thread = format!("{:?}", std::thread::current().id());
                context.editor().publish("sample.from_job", json!({ "thread": thread }))
            }),
            Kind::Measure => ctx.spawn(label, measure_calls),
        };
        self.jobs.insert(id, (kind, Instant::now()));
    }

    fn random_color(&self) -> [f32; 3] {
        let seed = self.start.elapsed().as_nanos() as u64;
        let channel = |shift: u32| ((seed.rotate_left(shift).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as f32) / 255.0;
        [channel(0), channel(21), channel(42)]
    }
}

/// Counts primes below `PRIME_BOUND` by trial division, reporting progress and honouring cancel.
fn count_primes(context: &JobContext) -> u64 {
    let mut count = 0;
    for n in 2..PRIME_BOUND {
        if n % 100_000 == 0 {
            if context.is_cancelled() {
                return count;
            }
            context.set_progress(n as f32 / PRIME_BOUND as f32);
        }
        let mut divisor = 2;
        let mut prime = true;
        while divisor * divisor <= n {
            if n % divisor == 0 {
                prime = false;
                break;
            }
            divisor += 1;
        }
        if prime {
            count += 1;
        }
    }
    count
}

/// Calls `cube.color` (calling thread) then `cube.paint` (interface thread) in loops and reports
/// the calls per second of each kind.
fn measure_calls(context: &JobContext) -> Result<String, String> {
    let editor = context.editor();
    let started = Instant::now();
    for _ in 0..CALLER_CALLS {
        editor.call("cube.color", json!({}))?;
    }
    let caller_rate = CALLER_CALLS as f64 / started.elapsed().as_secs_f64();
    context.set_progress(0.5);

    // Painting the cube the colour it already has changes nothing and records no undo entry.
    let color = editor.call("cube.color", json!({}))?["color"].clone();
    let started = Instant::now();
    for _ in 0..INTERFACE_CALLS {
        if context.is_cancelled() {
            break;
        }
        editor.call("cube.paint", json!({ "color": color }))?;
    }
    let interface_rate = INTERFACE_CALLS as f64 / started.elapsed().as_secs_f64();
    Ok(format!(
        "calling-thread command: {caller_rate:.0} calls/s; interface-thread command: {interface_rate:.0} calls/s"
    ))
}

/// Payload of `sample.paint`, as this feature writes it. Whoever answers declares its own type.
#[derive(Serialize)]
#[serde(crate = "uniwow_api::serde")]
struct Paint {
    color: [f32; 3],
}

uniwow_api::export_feature!(NotesFeature::default());
