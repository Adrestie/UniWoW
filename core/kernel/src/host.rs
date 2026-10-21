use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use uniwow_api::curve::{self, CurveEditor};
use uniwow_api::dopesheet::{self, Dopesheet};
use uniwow_api::property_grid::{self, PropertyGrid};
use uniwow_api::scene_view::{self, SceneView};
use uniwow_api::theme::Level;
use uniwow_api::ui::{self, SharedUi};
use uniwow_api::{CallId, Command, Editor, Event, Host, JobFn, JobId, Priority, egui, egui_wgpu, serde_json};

use crate::draw::PanelView;
use crate::jobs::Pool;
use crate::router::{Bridge, ReplyTo, Request};
use crate::settings::Settings;

pub struct Service {
    pub provider: String,
    pub value: Box<dyn Any + Send + Sync>,
}

/// A failure of `culprit` noticed by `reporter`, handled like a panic of `culprit`.
pub struct Reported {
    pub reporter: String,
    pub culprit: String,
    pub message: String,
}

/// A gesture of the user open: its label, the module that opened it, its changes so far, whether
/// its module ended it, its calls not yet answered, and the frames it stayed open with none.
pub struct Gesture {
    pub label: String,
    pub owner: String,
    pub parts: Vec<crate::history::Part>,
    pub ended: bool,
    pub waiting: usize,
    pub idle: u32,
}

/// Kernel state reachable from modules through `Context`.
pub struct KernelHost {
    /// Published this frame, delivered at the end of it.
    pub events: Vec<Event>,
    /// Queued commands with the id of their module and the gesture they join, if any.
    pub pending: Vec<(String, Box<dyn Command>, Option<u64>)>,
    /// The gestures open, by number; the one each module has open in its call, the one of the call
    /// being served, and the gesture of each call of a module made within one.
    pub gestures: HashMap<u64, Gesture>,
    pub opened: HashMap<String, u64>,
    pub serving: Option<u64>,
    pub call_gestures: HashMap<(String, u64), u64>,
    next_gesture: u64,
    /// Documents closed without saving, with the id of their module, whose changes the history
    /// forgets.
    pub forgotten: Vec<(String, String)>,
    pub services: HashMap<String, Service>,
    pub gpu: Option<egui_wgpu::RenderState>,
    /// The memory of its own of the GPU, when the system tells it.
    pub gpu_memory: Option<u64>,
    pub settings: Settings,
    pub settings_changed: bool,
    pub reported: Vec<Reported>,
    pub pool: Pool,
    pub bridge: Arc<Bridge>,
    /// What the interface keeps between frames of the interface objects each module draws, by
    /// module and set of objects.
    pub views: HashMap<(String, usize), PanelView>,
    /// Whether a modal window was drawn this frame: it takes the keyboard from the editor.
    pub modal_shown: bool,
    /// The layers of every modal window the kernel has drawn, to tell them from those of modules.
    pub kernel_modals: HashSet<egui::LayerId>,
    /// The interface objects of each module that handed them over, whose players the kernel moves
    /// on and whose changes it records.
    pub adopted: Vec<(String, Weak<std::sync::Mutex<ui::Ui>>)>,
    /// The notices the modules asked for this frame, by module, shown at the next one.
    pub notices: Vec<(String, Level, String)>,
    next_call: u64,
}

/// The views of a module's set of interface objects.
fn view_key(owner: &str, objects: &SharedUi) -> (String, usize) {
    (owner.to_owned(), Arc::as_ptr(objects) as usize)
}

impl KernelHost {
    /// The curve editor, which draws the curve views, when a running module provides it.
    fn curve_editor(&self) -> Option<Arc<dyn CurveEditor>> {
        self.services
            .get(curve::SERVICE.id())?
            .value
            .downcast_ref::<Arc<dyn CurveEditor>>()
            .cloned()
    }

    /// The dopesheet, which draws the dopesheet views, when a running module provides it.
    fn dopesheet(&self) -> Option<Arc<dyn Dopesheet>> {
        self.services
            .get(dopesheet::SERVICE.id())?
            .value
            .downcast_ref::<Arc<dyn Dopesheet>>()
            .cloned()
    }

    /// The property grid, which draws the property grids, when a running module provides it.
    fn property_grid(&self) -> Option<Arc<dyn PropertyGrid>> {
        self.services
            .get(property_grid::SERVICE.id())?
            .value
            .downcast_ref::<Arc<dyn PropertyGrid>>()
            .cloned()
    }

    /// A view of the scene, which draws the objects `HierarchyView` or `InspectorView`, when a
    /// running module provides it.
    fn scene_view(&self, key: uniwow_api::ServiceKey<Arc<dyn SceneView>>) -> Option<Arc<dyn SceneView>> {
        self.services
            .get(key.id())?
            .value
            .downcast_ref::<Arc<dyn SceneView>>()
            .cloned()
    }

    /// The view of `owner`'s objects, with the services and the editor it draws with.
    fn view(&mut self, owner: &str, objects: &SharedUi) -> &mut PanelView {
        let (curve_editor, sheet, grid) = (self.curve_editor(), self.dopesheet(), self.property_grid());
        let scene_views = [
            self.scene_view(scene_view::HIERARCHY),
            self.scene_view(scene_view::INSPECTOR),
        ];
        let (editor, backend) = (self.editor(owner), self.bridge.clone());
        let view = self.views.entry(view_key(owner, objects)).or_default();
        view.set_services(curve_editor, sheet, grid, editor, backend);
        view.set_scene_views(scene_views);
        view
    }

    /// A panic of a service while drawing is its provider's fault, not the fault of the module
    /// drawing (F5).
    fn report_service_failures(&mut self, owner: &str, objects: &SharedUi) {
        let failures = self
            .views
            .get_mut(&view_key(owner, objects))
            .map(PanelView::take_failures)
            .unwrap_or_default();
        for (service, message) in failures {
            if let Some(provider) = self.services.get(service).map(|s| s.provider.clone()) {
                self.report_failure(owner, &provider, &message);
            }
        }
    }

    /// The settings of the modules move to the bridge, where every thread reads them.
    pub fn new(gpu: Option<egui_wgpu::RenderState>, mut settings: Settings, pool: Pool, bridge: Arc<Bridge>) -> Self {
        *bridge.settings.write().unwrap_or_else(|e| e.into_inner()) = std::mem::take(&mut settings.modules);
        let gpu_memory = gpu
            .as_ref()
            .and_then(|gpu| crate::gpu_memory::dedicated(&gpu.adapter.get_info()));
        Self {
            events: Vec::new(),
            pending: Vec::new(),
            gestures: HashMap::new(),
            opened: HashMap::new(),
            serving: None,
            call_gestures: HashMap::new(),
            next_gesture: 0,
            forgotten: Vec::new(),
            views: HashMap::new(),
            modal_shown: false,
            kernel_modals: HashSet::new(),
            adopted: Vec::new(),
            notices: Vec::new(),
            services: HashMap::new(),
            gpu,
            gpu_memory,
            settings,
            settings_changed: false,
            reported: Vec::new(),
            pool,
            bridge,
            next_call: 0,
        }
    }

    /// The gesture `owner`'s changes and calls join now: the one it opened in its call, else the one
    /// of the call being served.
    pub fn gesture_of(&self, owner: &str) -> Option<u64> {
        self.opened.get(owner).copied().or(self.serving)
    }

    /// A call of the gesture `gesture` answered.
    pub fn call_answered(&mut self, gesture: u64) {
        if let Some(open) = self.gestures.get_mut(&gesture) {
            open.waiting = open.waiting.saturating_sub(1);
        }
    }

    /// Writes the settings, the modules' ones back from the bridge.
    pub fn save_settings(&mut self) {
        self.settings.modules = self.bridge.settings.read().unwrap_or_else(|e| e.into_inner()).clone();
        self.settings.save();
    }
}

impl Host for KernelHost {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value) {
        self.events.push(Event {
            topic: topic.to_owned(),
            source: source.to_owned(),
            payload,
        });
    }

    fn execute(&mut self, owner: &str, command: Box<dyn Command>) {
        let gesture = self.gesture_of(owner);
        self.pending.push((owner.to_owned(), command, gesture));
    }

    fn begin_gesture(&mut self, owner: &str, label: &str) -> u64 {
        self.next_gesture += 1;
        let id = self.next_gesture;
        self.gestures.insert(
            id,
            Gesture {
                label: label.to_owned(),
                owner: owner.to_owned(),
                parts: Vec::new(),
                ended: false,
                waiting: 0,
                idle: 0,
            },
        );
        self.opened.insert(owner.to_owned(), id);
        id
    }

    fn resume_gesture(&mut self, owner: &str, gesture: u64) -> bool {
        let Some(open) = self
            .gestures
            .get_mut(&gesture)
            .filter(|open| open.owner == owner && !open.ended)
        else {
            return false;
        };
        open.idle = 0;
        self.opened.insert(owner.to_owned(), gesture);
        true
    }

    fn end_gesture(&mut self, owner: &str, gesture: u64) {
        if self.opened.get(owner) == Some(&gesture) {
            self.opened.remove(owner);
        }
        if let Some(open) = self.gestures.get_mut(&gesture) {
            open.ended = true;
        }
    }

    fn forget_document(&mut self, owner: &str, document: &str) {
        self.forgotten.push((owner.to_owned(), document.to_owned()));
    }

    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)> {
        self.services.get(id).map(|s| s.value.as_ref())
    }

    fn service_provider(&self, id: &str) -> Option<String> {
        self.services.get(id).map(|s| s.provider.clone())
    }

    fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        self.gpu.as_ref()
    }

    fn gpu_memory(&self) -> Option<u64> {
        self.gpu_memory
    }

    fn draw_panel(&mut self, owner: &str, objects: &SharedUi, panel: &str, ui: &mut egui::Ui) {
        let gpu = self.gpu.clone();
        self.view(owner, objects).show(objects, panel, ui, gpu.as_ref());
        self.report_service_failures(owner, objects);
    }

    fn draw_object(&mut self, _owner: &str, module: &str, handle: u64, ui: &mut egui::Ui) -> bool {
        let Some(objects) = self
            .adopted
            .iter()
            .filter(|(owner, _)| owner == module)
            .find_map(|(_, kept)| kept.upgrade())
        else {
            return false;
        };
        let gpu = self.gpu.clone();
        let drawn = self
            .view(module, &objects)
            .show_object(&objects, handle as ui::Handle, ui, gpu.as_ref());
        self.report_service_failures(module, &objects);
        drawn
    }

    fn draw_dialogs(&mut self, owner: &str, objects: &SharedUi, egui: &egui::Context) {
        let gpu = self.gpu.clone();
        let layers = self.view(owner, objects).dialogs(objects, egui, gpu.as_ref());
        self.modal_shown |= !layers.is_empty();
        self.kernel_modals.extend(layers);
        self.report_service_failures(owner, objects);
    }

    fn adopt_objects(&mut self, owner: &str, objects: &SharedUi) {
        let pointer = Arc::as_ptr(objects);
        if self
            .adopted
            .iter()
            .any(|(module, kept)| module == owner && kept.as_ptr() == pointer)
        {
            return;
        }
        self.adopted.push((owner.to_owned(), Arc::downgrade(objects)));
        let editor = self.editor(owner);
        ui::lock(objects).set_recorder(Arc::new(move |label, change| {
            // Undo does not wait for a property's write nor a player's timeChanged.
            if let Some(reason) = crate::capi::unrecorded() {
                return Err(reason.to_owned());
            }
            editor.record_change(label, change)
        }));
    }

    fn setting(&self, module: &str, key: &str) -> Option<serde_json::Value> {
        let settings = self.bridge.settings.read().unwrap_or_else(|e| e.into_inner());
        settings.get(module)?.get(key).cloned()
    }

    fn set_setting(&mut self, module: &str, key: &str, value: serde_json::Value) {
        self.bridge
            .settings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(module.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        self.settings_changed = true;
    }

    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str) {
        self.reported.push(Reported {
            reporter: reporter.to_owned(),
            culprit: culprit.to_owned(),
            message: message.to_owned(),
        });
    }

    fn spawn(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn(owner, label, job, editor)
    }

    fn spawn_with_priority(&mut self, owner: &str, label: &str, priority: Priority, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn_with_priority(owner, label, priority, job, editor)
    }

    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn_thread(owner, label, job, editor)
    }

    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId {
        self.next_call += 1;
        let id = CallId(self.next_call);
        if let Some(gesture) = self.gesture_of(caller)
            && let Some(open) = self.gestures.get_mut(&gesture)
        {
            open.waiting += 1;
            self.call_gestures.insert((caller.to_owned(), id.0), gesture);
        }
        self.bridge.queue(Request::Call {
            caller: caller.to_owned(),
            thread: std::thread::current().id(),
            name: name.to_owned(),
            arguments,
            reply: ReplyTo::Module(caller.to_owned(), id),
        });
        id
    }

    fn cancel(&mut self, owner: &str, job: JobId) {
        self.pool.cancel(owner, job);
    }

    fn editor(&self, caller: &str) -> Editor {
        Editor::new(self.bridge.clone(), caller)
    }

    fn notify(&mut self, owner: &str, level: Level, text: &str) {
        self.notices.push((owner.to_owned(), level, text.to_owned()));
    }
}
