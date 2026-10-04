//! The catalogue of named commands (F6), the `Editor` handle given to other threads, and the
//! queue through which they reach the interface thread (T4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use uniwow_api::serde_json::Value;
use uniwow_api::{
    AppliedChange, CallId, CommandHandler, CommandInfo, CommandSpec, EditorBackend, Event, PropertyInfo, PropertyValue,
    ReadProperty, WriteProperty, egui, log,
};

use crate::guard::guarded_as;
use crate::host::Reported;

/// A command of the catalogue.
pub struct Entry {
    pub info: CommandInfo,
    /// Set for a command running on the calling thread.
    pub handler: Option<CommandHandler>,
}

/// An animatable property of the catalogue.
#[derive(Clone)]
pub struct PropertyEntry {
    pub info: PropertyInfo,
    pub read: ReadProperty,
    pub write: WriteProperty,
}

/// Where the answer of a call goes.
pub enum ReplyTo {
    /// A thread waiting in `Editor::call`.
    Thread(mpsc::Sender<Result<Value, String>>),
    /// A module, through `Module::on_reply`.
    Module(String, CallId),
    /// The Commands panel of the kernel.
    Kernel(u64),
}

/// What other threads ask of the interface thread, served in the order they asked. `thread` is
/// the thread that asked: undo groups belong to a caller on one thread (S4).
pub enum Request {
    Call {
        caller: String,
        thread: ThreadId,
        name: String,
        arguments: Value,
        reply: ReplyTo,
    },
    BeginGroup {
        caller: String,
        thread: ThreadId,
        label: String,
    },
    EndGroup {
        caller: String,
        thread: ThreadId,
    },
    /// A change the caller's module already made, to record (F2).
    RecordChange {
        caller: String,
        thread: ThreadId,
        label: String,
        change: Box<dyn AppliedChange>,
    },
    /// A job's thread finished its job: the groups it left open are closed.
    ThreadEnded {
        thread: ThreadId,
    },
    /// The answer of a call run on the thread of the compiled module offering the command.
    Answer {
        caller: String,
        name: String,
        reply: ReplyTo,
        result: Result<Value, String>,
    },
}

/// Chooses, once every module has registered, which declaration of each command name the
/// catalogue keeps: a command a module declares itself wins over a delegated one; between two of
/// the same kind, the first registered wins. Each declaration set aside is logged with the one
/// that wins. Only the kind of declaration counts, never which module it comes from (R1).
pub fn choose_commands(declared: Vec<(String, CommandSpec)>) -> Vec<(String, CommandSpec)> {
    let mut kept: Vec<(String, CommandSpec)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (owner, spec) in declared {
        let Some(&at) = index.get(&spec.name) else {
            index.insert(spec.name.clone(), kept.len());
            kept.push((owner, spec));
            continue;
        };
        let (winner, kept_spec) = &kept[at];
        if kept_spec.delegated && !spec.delegated {
            log::warn!(
                "command '{}' offered by '{winner}' on behalf of a module or a script set aside: '{owner}' declares it",
                spec.name
            );
            kept[at] = (owner, spec);
        } else {
            log::warn!("command '{}' of '{owner}' set aside: '{winner}' offers it", spec.name);
        }
    }
    kept
}

/// The module part of a caller: `scripting-lua#paint.lua #3` → `scripting-lua`.
pub fn module_of(caller: &str) -> &str {
    caller.split('#').next().unwrap_or(caller)
}

struct Subscription {
    /// Who subscribed: its subscriptions close when its module fails.
    caller: String,
    topic: String,
    sender: mpsc::Sender<Event>,
    receiver: Arc<Mutex<mpsc::Receiver<Event>>>,
}

/// State shared between the interface thread and every `Editor` handle.
pub struct Bridge {
    pub catalogue: RwLock<BTreeMap<String, Entry>>,
    /// Animatable properties by path; only those of running modules are reached.
    pub properties: RwLock<BTreeMap<String, PropertyEntry>>,
    /// Ids of the running modules; only their commands can be called.
    pub running: RwLock<HashSet<String>>,
    /// Events published from other threads, delivered at the next frame.
    pub events: Mutex<Vec<Event>>,
    /// Failures of commands run on other threads, applied at the next frame.
    pub failures: Mutex<Vec<Reported>>,
    /// Settings by space (a module id, or `module#module`) then key. Shared, so that any
    /// thread reads them at once, the interface thread included.
    pub settings: RwLock<BTreeMap<String, BTreeMap<String, Value>>>,
    /// Set when a setting changes; the interface thread then saves them.
    pub settings_changed: AtomicBool,
    subscriptions: Mutex<HashMap<u64, Subscription>>,
    next_subscription: AtomicU64,
    requests: mpsc::Sender<Request>,
    interface_thread: ThreadId,
    wake: Option<egui::Context>,
}

impl Bridge {
    /// The bridge, and the receiving end of the queue, kept by the interface thread.
    pub fn new(wake: Option<egui::Context>) -> (Arc<Self>, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel();
        let bridge = Self {
            catalogue: RwLock::default(),
            properties: RwLock::default(),
            running: RwLock::default(),
            events: Mutex::default(),
            failures: Mutex::default(),
            settings: RwLock::default(),
            settings_changed: AtomicBool::new(false),
            subscriptions: Mutex::default(),
            next_subscription: AtomicU64::new(1),
            requests,
            interface_thread: std::thread::current().id(),
            wake,
        };
        (Arc::new(bridge), receiver)
    }

    /// Hands a published event to the subscriptions of other threads.
    pub fn deliver(&self, event: &Event) {
        for subscription in self.subscriptions.lock().unwrap_or_else(|e| e.into_inner()).values() {
            if subscription.topic == "*" || subscription.topic == event.topic {
                // A subscriber that stopped reading is no reason to fail.
                let _ = subscription.sender.send(event.clone());
            }
        }
    }

    /// Closes the subscriptions of a module that failed. Their channels close with them: a thread
    /// waiting in `next_event` wakes at once with an error, and nothing more piles up there.
    pub fn close_subscriptions(&self, module: &str) {
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, subscription| module_of(&subscription.caller) != module);
    }

    /// Refuses a caller whose module no longer runs: its scripts, jobs and module threads get
    /// errors from then on. The kernel itself always acts.
    pub fn active(&self, caller: &str) -> Result<(), String> {
        let module = module_of(caller);
        if caller == "kernel" || self.running.read().unwrap_or_else(|e| e.into_inner()).contains(module) {
            Ok(())
        } else {
            Err(format!("'{module}' is not running: '{caller}' can no longer act"))
        }
    }

    /// Refuses settings of another module than the caller's.
    fn settings_of(&self, caller: &str, space: &str) -> Result<(), String> {
        self.active(caller)?;
        if module_of(space) == module_of(caller) {
            Ok(())
        } else {
            Err(format!("'{caller}' cannot reach the settings of '{space}'"))
        }
    }

    fn on_interface_thread(&self) -> bool {
        std::thread::current().id() == self.interface_thread
    }

    /// Queues a request for the interface thread.
    pub fn queue(&self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.wake();
        }
    }

    /// The command, if it exists and its module is running.
    pub fn lookup(&self, name: &str) -> Result<(String, Option<CommandHandler>), String> {
        // The set of running modules is never read while the catalogue is held (section 5).
        let (owner, handler) = {
            let catalogue = self.catalogue.read().unwrap_or_else(|e| e.into_inner());
            let entry = catalogue.get(name).ok_or_else(|| format!("unknown command '{name}'"))?;
            (entry.info.owner.clone(), entry.handler.clone())
        };
        if !self.running.read().unwrap_or_else(|e| e.into_inner()).contains(&owner) {
            return Err(format!("'{name}' belongs to '{owner}', which is not running"));
        }
        Ok((owner, handler))
    }

    /// Runs a command handled on the calling thread. A panic makes its module fail.
    pub fn run_on_caller(
        &self,
        owner: &str,
        name: &str,
        handler: &CommandHandler,
        arguments: Value,
    ) -> Result<Value, String> {
        guarded_as(owner, || handler(arguments)).unwrap_or_else(|panic| {
            self.failures.lock().unwrap_or_else(|e| e.into_inner()).push(Reported {
                reporter: "kernel".to_owned(),
                culprit: owner.to_owned(),
                message: format!("command '{name}' panicked: {panic}"),
            });
            self.wake();
            Err(format!("'{name}' failed: {panic}"))
        })
    }

    /// The property, if it exists and its module is running.
    fn property(&self, path: &str) -> Result<PropertyEntry, String> {
        let entry = self
            .properties
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
            .ok_or_else(|| format!("unknown property '{path}'"))?;
        if !self
            .running
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&entry.info.owner)
        {
            return Err(format!(
                "'{path}' belongs to '{}', which is not running",
                entry.info.owner
            ));
        }
        Ok(entry)
    }

    /// Runs a property's function; a panic there makes its module fail, at the next frame.
    fn run_property<R>(&self, entry: &PropertyEntry, f: impl FnOnce() -> R) -> Result<R, String> {
        let owner = &entry.info.owner;
        guarded_as(owner, f).map_err(|panic| {
            self.failures.lock().unwrap_or_else(|e| e.into_inner()).push(Reported {
                reporter: "kernel".to_owned(),
                culprit: owner.clone(),
                message: format!("property '{}' panicked: {panic}", entry.info.path),
            });
            self.wake();
            format!("'{}' failed: {panic}", entry.info.path)
        })
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.request_repaint();
        }
    }
}

impl EditorBackend for Bridge {
    fn properties(&self) -> Vec<PropertyInfo> {
        let running = self.running.read().unwrap_or_else(|e| e.into_inner());
        self.properties
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|entry| running.contains(&entry.info.owner))
            .map(|entry| entry.info.clone())
            .collect()
    }

    fn read_property(&self, caller: &str, path: &str) -> Result<PropertyValue, String> {
        self.active(caller)?;
        let entry = self.property(path)?;
        self.run_property(&entry, || (entry.read)())
    }

    fn write_property(&self, caller: &str, path: &str, value: PropertyValue) -> Result<(), String> {
        self.active(caller)?;
        let entry = self.property(path)?;
        if value.kind() != entry.info.kind {
            return Err(format!(
                "'{path}' takes a {}, not a {}",
                entry.info.kind.name(),
                value.kind().name()
            ));
        }
        let value = value.clamped(entry.info.range);
        self.run_property(&entry, || (entry.write)(value))
    }

    fn commands(&self) -> Vec<CommandInfo> {
        let running = self.running.read().unwrap_or_else(|e| e.into_inner());
        self.catalogue
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|entry| running.contains(&entry.info.owner))
            .map(|entry| entry.info.clone())
            .collect()
    }

    fn call(&self, caller: &str, name: &str, arguments: Value) -> Result<Value, String> {
        self.active(caller)?;
        let (owner, handler) = self.lookup(name)?;
        if let Some(handler) = handler {
            return self.run_on_caller(&owner, name, &handler, arguments);
        }
        // Waiting on the interface thread for the interface thread would never end.
        if self.on_interface_thread() {
            return Err(format!(
                "'{name}' runs on the interface thread: call it with Context::call from there"
            ));
        }
        let (reply, answer) = mpsc::channel();
        self.queue(Request::Call {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
            name: name.to_owned(),
            arguments,
            reply: ReplyTo::Thread(reply),
        });
        answer
            .recv()
            .map_err(|_| format!("'{name}' got no answer: the editor is closing"))?
    }

    fn publish(&self, source: &str, topic: &str, payload: Value) -> Result<(), String> {
        self.active(source)?;
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(Event {
            topic: topic.to_owned(),
            source: source.to_owned(),
            payload,
        });
        self.wake();
        Ok(())
    }

    fn subscribe(&self, caller: &str, topic: &str) -> Result<u64, String> {
        self.active(caller)?;
        let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        let subscription = Subscription {
            caller: caller.to_owned(),
            topic: topic.to_owned(),
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        };
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, subscription);
        Ok(id)
    }

    fn next_event(&self, caller: &str, subscription: u64, timeout: Duration) -> Result<Option<Event>, String> {
        self.active(caller)?;
        let unknown = || format!("subscription {subscription} does not exist, was closed, or its module stopped");
        // The map is released before waiting, so that other threads can publish meanwhile.
        let receiver = self
            .subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&subscription)
            .ok_or_else(unknown)?
            .receiver
            .clone();
        let receiver = receiver.lock().unwrap_or_else(|e| e.into_inner());
        match receiver.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            // Closed by another thread while this one waited.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(unknown()),
        }
    }

    fn unsubscribe(&self, subscription: u64) {
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&subscription);
    }

    fn setting(&self, caller: &str, space: &str, key: &str) -> Result<Option<Value>, String> {
        self.settings_of(caller, space)?;
        let settings = self.settings.read().unwrap_or_else(|e| e.into_inner());
        Ok(settings.get(space).and_then(|space| space.get(key)).cloned())
    }

    fn set_setting(&self, caller: &str, space: &str, key: &str, value: Value) -> Result<(), String> {
        self.settings_of(caller, space)?;
        self.settings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(space.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        self.settings_changed.store(true, Ordering::Relaxed);
        self.wake();
        Ok(())
    }

    fn begin_group(&self, caller: &str, label: &str) -> Result<(), String> {
        self.active(caller)?;
        self.queue(Request::BeginGroup {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
            label: label.to_owned(),
        });
        Ok(())
    }

    fn end_group(&self, caller: &str) -> Result<(), String> {
        self.active(caller)?;
        self.queue(Request::EndGroup {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
        });
        Ok(())
    }

    fn record_change(&self, caller: &str, label: &str, change: Box<dyn AppliedChange>) -> Result<(), String> {
        self.active(caller)?;
        self.queue(Request::RecordChange {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
            label: label.to_owned(),
            change,
        });
        Ok(())
    }

    fn report_failure(&self, caller: &str, message: &str) {
        self.failures.lock().unwrap_or_else(|e| e.into_inner()).push(Reported {
            reporter: "kernel".to_owned(),
            culprit: module_of(caller).to_owned(),
            message: message.to_owned(),
        });
        self.wake();
    }

    fn is_active(&self, caller: &str) -> bool {
        self.active(caller).is_ok()
    }
}

/// Serves queued calls for at most `budget`. After a call, it waits up to `idle` for the next one,
/// so that a thread calling in a loop gets many answers within one frame. Returns whether the
/// budget ran out, calls perhaps still waiting.
pub fn serve(
    receiver: &mpsc::Receiver<Request>,
    budget: Duration,
    idle: Duration,
    mut handle: impl FnMut(Request),
) -> bool {
    let deadline = Instant::now() + budget;
    let mut served = false;
    loop {
        let request = match receiver.try_recv() {
            Ok(request) => request,
            Err(mpsc::TryRecvError::Empty) if served => {
                let left = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(idle.min(left)) {
                    Ok(request) => request,
                    Err(_) => return false,
                }
            }
            Err(_) => return false,
        };
        handle(request);
        served = true;
        if Instant::now() >= deadline {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use uniwow_api::serde_json::{Value, json};
    use uniwow_api::{CommandInfo, EditorBackend, PropertyInfo, PropertyKind, PropertyValue};

    use uniwow_api::{CommandSpec, RunsOn};

    use super::{Bridge, Entry, PropertyEntry, ReplyTo, Request, choose_commands, serve};
    use crate::random::Random;

    fn declared(owner: &str, name: &str, delegated: bool) -> (String, CommandSpec) {
        let spec = CommandSpec {
            name: name.to_owned(),
            description: String::new(),
            arguments: json!({}),
            result: json!({}),
            runs_on: RunsOn::Interface,
            delegated,
        };
        (owner.to_owned(), spec)
    }

    fn owners(kept: &[(String, CommandSpec)]) -> Vec<(&str, &str)> {
        kept.iter()
            .map(|(owner, spec)| (spec.name.as_str(), owner.as_str()))
            .collect()
    }

    #[test]
    fn a_module_s_own_command_wins_over_a_delegated_one_in_any_order() {
        for declared in [
            vec![
                declared("modules", "cube.paint", true),
                declared("cube", "cube.paint", false),
            ],
            vec![
                declared("cube", "cube.paint", false),
                declared("modules", "cube.paint", true),
            ],
        ] {
            assert_eq!(owners(&choose_commands(declared)), vec![("cube.paint", "cube")]);
        }
    }

    #[test]
    fn between_two_of_the_same_kind_the_first_registered_wins() {
        let delegated = vec![declared("modules", "x.do", true), declared("lua", "x.do", true)];
        assert_eq!(owners(&choose_commands(delegated)), vec![("x.do", "modules")]);
        let direct = vec![declared("cube", "x.do", false), declared("terrain", "x.do", false)];
        assert_eq!(owners(&choose_commands(direct)), vec![("x.do", "cube")]);
    }

    fn info(name: &str, owner: &str, on_caller: bool) -> CommandInfo {
        CommandInfo {
            name: name.to_owned(),
            owner: owner.to_owned(),
            description: String::new(),
            arguments: json!({}),
            result: json!({}),
            on_caller,
        }
    }

    /// A bridge with `cube.color` (calling thread) and `cube.paint` (interface thread).
    fn bridge() -> (Arc<Bridge>, std::sync::mpsc::Receiver<super::Request>) {
        let (bridge, receiver) = Bridge::new(None);
        let thread = Arc::new(|_: Value| Ok(json!(format!("{:?}", std::thread::current().id()))));
        let mut catalogue = bridge.catalogue.write().unwrap();
        catalogue.insert(
            "cube.color".to_owned(),
            Entry {
                info: info("cube.color", "cube", true),
                handler: Some(thread),
            },
        );
        catalogue.insert(
            "cube.paint".to_owned(),
            Entry {
                info: info("cube.paint", "cube", false),
                handler: None,
            },
        );
        drop(catalogue);
        bridge.running.write().unwrap().insert("cube".to_owned());
        (bridge, receiver)
    }

    #[test]
    fn threads_calling_at_once_each_get_their_answers_in_their_order() {
        let (bridge, receiver) = bridge();
        const THREADS: u64 = 8;
        const CALLS: u64 = 150;
        let workers: Vec<_> = (0..THREADS)
            .map(|index| {
                let editor = uniwow_api::Editor::new(bridge.clone(), "cube");
                std::thread::spawn(move || {
                    let mut random = Random::new(index + 1);
                    for n in 0..CALLS {
                        if n % 10 == 0 {
                            editor.begin_group("ten").unwrap();
                        }
                        if random.below(4) == 0 {
                            std::thread::yield_now();
                        }
                        let asked = json!({ "thread": index, "n": n });
                        assert_eq!(editor.call("cube.paint", asked.clone()), Ok(asked));
                        if n % 10 == 9 {
                            editor.end_group().unwrap();
                        }
                    }
                })
            })
            .collect();
        // What each thread's requests were, in the order served: B, its ten numbers, E, B…
        let mut served: std::collections::HashMap<std::thread::ThreadId, Vec<String>> = Default::default();
        let mut random = Random::new(99);
        let mut handle = |request: Request| match request {
            Request::Call {
                thread,
                arguments,
                reply: ReplyTo::Thread(reply),
                ..
            } => {
                served.entry(thread).or_default().push(arguments["n"].to_string());
                let _ = reply.send(Ok(arguments));
            }
            Request::BeginGroup { thread, .. } => served.entry(thread).or_default().push("B".to_owned()),
            Request::EndGroup { thread, .. } => served.entry(thread).or_default().push("E".to_owned()),
            _ => {}
        };
        while workers.iter().any(|worker| !worker.is_finished()) {
            let budget = Duration::from_micros(random.below(2_000));
            serve(&receiver, budget, Duration::from_micros(100), &mut handle);
        }
        // What the threads sent before ending, the last EndGroup among them.
        while serve(&receiver, Duration::from_secs(1), Duration::ZERO, &mut handle) {}
        for worker in workers {
            worker.join().unwrap();
        }
        let expected: Vec<String> = (0..CALLS)
            .flat_map(|n| {
                let mut step = Vec::new();
                if n % 10 == 0 {
                    step.push("B".to_owned());
                }
                step.push(n.to_string());
                if n % 10 == 9 {
                    step.push("E".to_owned());
                }
                step
            })
            .collect();
        assert_eq!(served.len(), THREADS as usize);
        for sequence in served.values() {
            assert_eq!(sequence, &expected);
        }
    }

    #[test]
    fn an_unknown_command_is_an_error() {
        let (bridge, _receiver) = bridge();
        let error = bridge.call("cube", "cube.fly", json!({})).unwrap_err();
        assert_eq!(error, "unknown command 'cube.fly'");
    }

    #[test]
    fn a_command_of_a_stopped_module_is_an_error() {
        let (bridge, _receiver) = bridge();
        bridge.running.write().unwrap().clear();
        let error = bridge.call("cube", "cube.color", json!({})).unwrap_err();
        assert!(error.contains("not running"), "{error}");
    }

    #[test]
    fn a_caller_command_runs_on_the_calling_thread() {
        let (bridge, _receiver) = bridge();
        let here = format!("{:?}", std::thread::current().id());
        let worker = {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                let there = format!("{:?}", std::thread::current().id());
                (bridge.call("cube", "cube.color", json!({})).unwrap(), there)
            })
        };
        let (answer, there) = worker.join().unwrap();
        assert_eq!(answer, json!(there));
        assert_ne!(there, here);
    }

    #[test]
    fn an_interface_command_waits_for_the_interface_thread() {
        let (bridge, receiver) = bridge();
        let worker = {
            let bridge = bridge.clone();
            std::thread::spawn(move || bridge.call("cube", "cube.paint", json!({ "n": 1 })))
        };
        let request = receiver.recv_timeout(Duration::from_secs(5)).expect("queued");
        let Request::Call {
            name,
            reply: ReplyTo::Thread(reply),
            ..
        } = request
        else {
            panic!("a worker waits for the answer of a call")
        };
        assert_eq!(name, "cube.paint");
        reply.send(Ok(json!("painted"))).unwrap();
        assert_eq!(worker.join().unwrap(), Ok(json!("painted")));
    }

    #[test]
    fn an_interface_command_cannot_be_awaited_on_the_interface_thread() {
        // The bridge was created on this thread, which stands for the interface thread.
        let (bridge, _receiver) = bridge();
        let error = bridge.call("cube", "cube.paint", json!({})).unwrap_err();
        assert!(error.contains("Context::call"), "{error}");
    }

    #[test]
    fn successive_calls_are_served_within_one_budget() {
        let (bridge, receiver) = bridge();
        let worker = {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                for n in 0..50 {
                    bridge.call("cube", "cube.paint", json!(n)).unwrap();
                }
            })
        };
        // Wait for the first call, then serve: the 49 others arrive within the same budget.
        let (mut served, mut rounds) = (0, 0);
        while served < 50 {
            let mut count = 0;
            serve(
                &receiver,
                Duration::from_secs(2),
                Duration::from_millis(200),
                |request| {
                    if let Request::Call {
                        reply: ReplyTo::Thread(reply),
                        ..
                    } = request
                    {
                        reply.send(Ok(Value::Null)).unwrap();
                        count += 1;
                    }
                },
            );
            if count == 0 {
                std::thread::sleep(Duration::from_millis(1));
            } else {
                served += count;
                rounds += 1;
            }
        }
        worker.join().unwrap();
        assert_eq!((served, rounds), (50, 1));
    }

    #[test]
    fn a_subscription_receives_its_topic_only() {
        let (bridge, _receiver) = bridge();
        let paints = bridge.subscribe("cube", "cube.painted").unwrap();
        let everything = bridge.subscribe("cube", "*").unwrap();
        for topic in ["cube.painted", "other"] {
            bridge.deliver(&uniwow_api::Event {
                topic: topic.to_owned(),
                source: "cube".to_owned(),
                payload: json!({}),
            });
        }
        let short = Duration::from_millis(50);
        let topic = |subscription| {
            bridge
                .next_event("cube", subscription, short)
                .map(|e| e.map(|e| e.topic))
        };
        assert_eq!(topic(paints), Ok(Some("cube.painted".to_owned())));
        assert_eq!(topic(paints), Ok(None));
        assert_eq!(topic(everything), Ok(Some("cube.painted".to_owned())));
        assert_eq!(topic(everything), Ok(Some("other".to_owned())));
        bridge.unsubscribe(paints);
        assert!(
            topic(paints).is_err(),
            "a closed subscription is an error, not a timeout"
        );
        assert!(topic(42).is_err(), "so is an unknown one");
    }

    #[test]
    fn the_subscriptions_of_a_failed_module_close_and_wake_their_reader() {
        let (bridge, _receiver) = bridge();
        bridge.running.write().unwrap().insert("lua".to_owned());
        let script = bridge.subscribe("lua#events.lua #1", "*").unwrap();
        let cube = bridge.subscribe("cube", "*").unwrap();
        let reader = {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let next = bridge.next_event("lua#events.lua #1", script, Duration::from_secs(10));
                (next.map(|e| e.is_some()), started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        bridge.close_subscriptions("lua");
        let (next, waited) = reader.join().unwrap();
        assert!(next.is_err(), "the reader gets an error");
        assert!(waited < Duration::from_secs(2), "at once, not after its timeout");
        // Events go on reaching the other modules' subscriptions only.
        bridge.deliver(&uniwow_api::Event {
            topic: "any".to_owned(),
            source: "cube".to_owned(),
            payload: json!({}),
        });
        assert!(
            bridge
                .next_event("cube", cube, Duration::from_millis(50))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_caller_whose_module_stopped_is_refused() {
        let (bridge, _receiver) = bridge();
        let script = "lua#paint.lua #1";
        bridge.running.write().unwrap().insert("lua".to_owned());
        assert!(bridge.call(script, "cube.color", json!({})).is_ok());
        bridge.running.write().unwrap().remove("lua");
        assert!(
            bridge
                .call(script, "cube.color", json!({}))
                .unwrap_err()
                .contains("not running")
        );
        assert!(bridge.publish(script, "topic", json!({})).is_err());
        assert!(bridge.subscribe(script, "topic").is_err());
        assert!(bridge.set_setting(script, "lua", "key", json!(1)).is_err());
        assert!(bridge.begin_group(script, "group").is_err());
        assert!(bridge.end_group(script).is_err());
        assert!(
            bridge.call("kernel", "cube.color", json!({})).is_ok(),
            "the kernel always acts"
        );
    }

    #[test]
    fn settings_are_read_at_once_from_any_thread_in_their_own_space() {
        // This thread stands for the interface thread.
        let (bridge, _receiver) = bridge();
        bridge.running.write().unwrap().insert("modules".to_owned());
        bridge.set_setting("modules#a", "modules#a", "size", json!(1)).unwrap();
        bridge.set_setting("modules#b", "modules#b", "size", json!(2)).unwrap();
        assert_eq!(bridge.setting("modules#a", "modules#a", "size"), Ok(Some(json!(1))));
        assert_eq!(bridge.setting("modules#b", "modules#b", "size"), Ok(Some(json!(2))));
        assert_eq!(bridge.setting("modules", "modules", "size"), Ok(None));
        assert!(
            bridge.setting("modules#a", "cube", "size").is_err(),
            "not another module's"
        );
    }

    #[test]
    fn the_catalogue_lists_only_running_modules() {
        let (bridge, _receiver) = bridge();
        assert_eq!(bridge.commands().len(), 2);
        bridge.running.write().unwrap().clear();
        assert!(bridge.commands().is_empty());
    }

    /// A bridge with the property `cube/scale`, a vector from 0.01 to 100, whose writes go to
    /// the returned value; writing `[13, 13, 13]` panics.
    fn bridge_with_scale() -> (Arc<Bridge>, Arc<Mutex<PropertyValue>>) {
        let (bridge, _receiver) = bridge();
        let scale = Arc::new(Mutex::new(PropertyValue::Vector([1.0; 3])));
        let (read, write) = (scale.clone(), scale.clone());
        bridge.properties.write().unwrap().insert(
            "cube/scale".to_owned(),
            PropertyEntry {
                info: PropertyInfo {
                    path: "cube/scale".to_owned(),
                    owner: "cube".to_owned(),
                    label: "Scale".to_owned(),
                    kind: PropertyKind::Vector,
                    range: [0.01, 100.0],
                },
                read: Arc::new(move || *read.lock().unwrap()),
                write: Arc::new(move |value| {
                    assert_ne!(value, PropertyValue::Vector([13.0; 3]), "unlucky");
                    *write.lock().unwrap() = value;
                }),
            },
        );
        (bridge, scale)
    }

    #[test]
    fn properties_are_written_within_their_range_and_type() {
        let (bridge, scale) = bridge_with_scale();
        assert_eq!(bridge.properties().len(), 1);
        let caller = "timeline";
        bridge.running.write().unwrap().insert(caller.to_owned());
        bridge
            .write_property(caller, "cube/scale", PropertyValue::Vector([2.0, 0.0, 500.0]))
            .unwrap();
        assert_eq!(*scale.lock().unwrap(), PropertyValue::Vector([2.0, 0.01, 100.0]));
        assert_eq!(
            bridge.read_property(caller, "cube/scale"),
            Ok(PropertyValue::Vector([2.0, 0.01, 100.0]))
        );
        assert!(
            bridge
                .write_property(caller, "cube/scale", PropertyValue::Number(1.0))
                .is_err()
        );
        assert!(bridge.read_property(caller, "cube/size").is_err());
    }

    #[test]
    fn a_property_of_a_stopped_module_is_out_of_reach_and_a_panic_is_reported() {
        let (bridge, _scale) = bridge_with_scale();
        let caller = "timeline";
        bridge.running.write().unwrap().insert(caller.to_owned());
        assert!(
            bridge
                .write_property(caller, "cube/scale", PropertyValue::Vector([13.0; 3]))
                .is_err()
        );
        let reported = bridge.failures.lock().unwrap();
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].culprit, "cube");
        drop(reported);
        bridge.running.write().unwrap().remove("cube");
        assert!(bridge.properties().is_empty());
        assert!(bridge.read_property(caller, "cube/scale").is_err());
    }
}
