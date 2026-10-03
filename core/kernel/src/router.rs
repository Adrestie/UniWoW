//! The catalogue of named commands (F6), the `Editor` handle given to other threads, and the
//! queue through which they reach the interface thread (T4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use uniwow_api::serde_json::Value;
use uniwow_api::{CallId, CommandHandler, CommandInfo, EditorBackend, Event, egui};

use crate::guard::guarded_as;
use crate::host::Reported;

/// A command of the catalogue.
pub struct Entry {
    pub info: CommandInfo,
    /// Set for a command running on the calling thread.
    pub handler: Option<CommandHandler>,
}

/// Where the answer of a call goes.
pub enum ReplyTo {
    /// A thread waiting in `Editor::call`.
    Thread(mpsc::Sender<Result<Value, String>>),
    /// A feature, through `Feature::on_reply`.
    Feature(String, CallId),
    /// The Commands panel of the kernel.
    Kernel(u64),
}

/// What other threads ask of the interface thread, served in the order they asked.
pub enum Request {
    Call {
        caller: String,
        name: String,
        arguments: Value,
        reply: ReplyTo,
    },
    Setting {
        caller: String,
        key: String,
        reply: mpsc::Sender<Option<Value>>,
    },
    SetSetting {
        caller: String,
        key: String,
        value: Value,
    },
    BeginGroup {
        caller: String,
        label: String,
    },
    EndGroup {
        caller: String,
    },
}

struct Subscription {
    topic: String,
    sender: mpsc::Sender<Event>,
    receiver: Arc<Mutex<mpsc::Receiver<Event>>>,
}

/// State shared between the interface thread and every `Editor` handle.
pub struct Bridge {
    pub catalogue: RwLock<BTreeMap<String, Entry>>,
    /// Ids of the running features; only their commands can be called.
    pub running: RwLock<HashSet<String>>,
    /// Events published from other threads, delivered at the next frame.
    pub events: Mutex<Vec<Event>>,
    /// Failures of commands run on other threads, applied at the next frame.
    pub failures: Mutex<Vec<Reported>>,
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
            running: RwLock::default(),
            events: Mutex::default(),
            failures: Mutex::default(),
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

    fn on_interface_thread(&self) -> bool {
        std::thread::current().id() == self.interface_thread
    }

    /// Queues a request for the interface thread.
    pub fn queue(&self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.wake();
        }
    }

    /// The command, if it exists and its feature is running.
    pub fn lookup(&self, name: &str) -> Result<(String, Option<CommandHandler>), String> {
        let catalogue = self.catalogue.read().unwrap_or_else(|e| e.into_inner());
        let entry = catalogue.get(name).ok_or_else(|| format!("unknown command '{name}'"))?;
        let owner = &entry.info.owner;
        if !self.running.read().unwrap_or_else(|e| e.into_inner()).contains(owner) {
            return Err(format!("'{name}' belongs to '{owner}', which is not running"));
        }
        Ok((owner.clone(), entry.handler.clone()))
    }

    /// Runs a command handled on the calling thread. A panic makes its feature fail.
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

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.request_repaint();
        }
    }
}

impl EditorBackend for Bridge {
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
            name: name.to_owned(),
            arguments,
            reply: ReplyTo::Thread(reply),
        });
        answer
            .recv()
            .map_err(|_| format!("'{name}' got no answer: the editor is closing"))?
    }

    fn publish(&self, source: &str, topic: &str, payload: Value) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(Event {
            topic: topic.to_owned(),
            source: source.to_owned(),
            payload,
        });
        self.wake();
    }

    fn subscribe(&self, _caller: &str, topic: &str) -> u64 {
        let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        let subscription = Subscription {
            topic: topic.to_owned(),
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        };
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, subscription);
        id
    }

    fn next_event(&self, subscription: u64, timeout: Duration) -> Option<Event> {
        // The map is released before waiting, so that other threads can publish meanwhile.
        let receiver = self
            .subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&subscription)?
            .receiver
            .clone();
        let receiver = receiver.lock().unwrap_or_else(|e| e.into_inner());
        receiver.recv_timeout(timeout).ok()
    }

    fn unsubscribe(&self, subscription: u64) {
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&subscription);
    }

    fn setting(&self, caller: &str, key: &str) -> Result<Option<Value>, String> {
        if self.on_interface_thread() {
            return Err("on the interface thread, read settings with Context::setting".to_owned());
        }
        let (reply, answer) = mpsc::channel();
        self.queue(Request::Setting {
            caller: caller.to_owned(),
            key: key.to_owned(),
            reply,
        });
        answer.recv().map_err(|_| "no answer: the editor is closing".to_owned())
    }

    fn set_setting(&self, caller: &str, key: &str, value: Value) {
        self.queue(Request::SetSetting {
            caller: caller.to_owned(),
            key: key.to_owned(),
            value,
        });
    }

    fn begin_group(&self, caller: &str, label: &str) {
        self.queue(Request::BeginGroup {
            caller: caller.to_owned(),
            label: label.to_owned(),
        });
    }

    fn end_group(&self, caller: &str) {
        self.queue(Request::EndGroup {
            caller: caller.to_owned(),
        });
    }
}

/// Serves queued calls for at most `budget`. After a call, it waits up to `idle` for the next one,
/// so that a thread calling in a loop gets many answers within one frame. Returns how many calls
/// were served.
pub fn serve(
    receiver: &mpsc::Receiver<Request>,
    budget: Duration,
    idle: Duration,
    mut handle: impl FnMut(Request),
) -> usize {
    let deadline = Instant::now() + budget;
    let mut served = 0;
    loop {
        let request = match receiver.try_recv() {
            Ok(request) => request,
            Err(mpsc::TryRecvError::Empty) if served > 0 => {
                let left = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(idle.min(left)) {
                    Ok(request) => request,
                    Err(_) => return served,
                }
            }
            Err(_) => return served,
        };
        handle(request);
        served += 1;
        if Instant::now() >= deadline {
            return served;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use uniwow_api::serde_json::{Value, json};
    use uniwow_api::{CommandInfo, EditorBackend};

    use super::{Bridge, Entry, ReplyTo, Request, serve};

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
    fn an_unknown_command_is_an_error() {
        let (bridge, _receiver) = bridge();
        let error = bridge.call("test", "cube.fly", json!({})).unwrap_err();
        assert_eq!(error, "unknown command 'cube.fly'");
    }

    #[test]
    fn a_command_of_a_stopped_feature_is_an_error() {
        let (bridge, _receiver) = bridge();
        bridge.running.write().unwrap().clear();
        let error = bridge.call("test", "cube.color", json!({})).unwrap_err();
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
                (bridge.call("test", "cube.color", json!({})).unwrap(), there)
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
            std::thread::spawn(move || bridge.call("test", "cube.paint", json!({ "n": 1 })))
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
        let error = bridge.call("test", "cube.paint", json!({})).unwrap_err();
        assert!(error.contains("Context::call"), "{error}");
    }

    #[test]
    fn successive_calls_are_served_within_one_budget() {
        let (bridge, receiver) = bridge();
        let worker = {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                for n in 0..50 {
                    bridge.call("test", "cube.paint", json!(n)).unwrap();
                }
            })
        };
        // Wait for the first call, then serve: the 49 others arrive within the same budget.
        let (mut served, mut rounds) = (0, 0);
        while served < 50 {
            let count = serve(
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
        let paints = bridge.subscribe("test", "cube.painted");
        let everything = bridge.subscribe("test", "*");
        for topic in ["cube.painted", "other"] {
            bridge.deliver(&uniwow_api::Event {
                topic: topic.to_owned(),
                source: "cube".to_owned(),
                payload: json!({}),
            });
        }
        let short = Duration::from_millis(50);
        assert_eq!(
            bridge.next_event(paints, short).map(|e| e.topic).as_deref(),
            Some("cube.painted")
        );
        assert!(bridge.next_event(paints, short).is_none());
        assert_eq!(
            bridge.next_event(everything, short).map(|e| e.topic).as_deref(),
            Some("cube.painted")
        );
        assert_eq!(
            bridge.next_event(everything, short).map(|e| e.topic).as_deref(),
            Some("other")
        );
        bridge.unsubscribe(paints);
        assert!(bridge.next_event(paints, short).is_none());
    }

    #[test]
    fn a_setting_cannot_be_awaited_on_the_interface_thread() {
        let (bridge, _receiver) = bridge();
        assert!(bridge.setting("test", "key").is_err());
    }

    #[test]
    fn the_catalogue_lists_only_running_features() {
        let (bridge, _receiver) = bridge();
        assert_eq!(bridge.commands().len(), 2);
        bridge.running.write().unwrap().clear();
        assert!(bridge.commands().is_empty());
    }
}
