use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use uniwow_api::curve::{self, CurveEditor};
use uniwow_api::dopesheet::{self, Dopesheet};
use uniwow_api::ui::{self, SharedUi};
use uniwow_api::{CallId, Command, Editor, Event, Host, JobFn, JobId, egui, egui_wgpu, serde_json};

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

/// Kernel state reachable from modules through `Context`.
pub struct KernelHost {
    /// Published this frame, delivered at the end of it.
    pub events: Vec<Event>,
    /// Queued commands with the id of their module.
    pub pending: Vec<(String, Box<dyn Command>)>,
    /// Documents closed without saving, with the id of their module, whose changes the history
    /// forgets.
    pub forgotten: Vec<(String, String)>,
    pub services: HashMap<String, Service>,
    pub gpu: Option<egui_wgpu::RenderState>,
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
    next_call: u64,
}

/// The views of a module's set of interface objects.
fn view_key(owner: &str, objects: &SharedUi) -> (String, usize) {
    (owner.to_owned(), Arc::as_ptr(objects) as usize)
}

impl KernelHost {
    /// The curve editor of the module `curves`, which draws the curve views, when it runs.
    fn curve_editor(&self) -> Option<Arc<dyn CurveEditor>> {
        self.services
            .get(curve::SERVICE.id())?
            .value
            .downcast_ref::<Arc<dyn CurveEditor>>()
            .cloned()
    }

    /// The dopesheet of the module `dopesheet`, which draws the dopesheet views, when it runs.
    fn dopesheet(&self) -> Option<Arc<dyn Dopesheet>> {
        self.services
            .get(dopesheet::SERVICE.id())?
            .value
            .downcast_ref::<Arc<dyn Dopesheet>>()
            .cloned()
    }

    /// The view of `owner`'s objects, with the services and the editor it draws with.
    fn view(&mut self, owner: &str, objects: &SharedUi) -> &mut PanelView {
        let (curve_editor, sheet, editor) = (self.curve_editor(), self.dopesheet(), self.editor(owner));
        let view = self.views.entry(view_key(owner, objects)).or_default();
        view.set_services(curve_editor, sheet, editor);
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
        Self {
            events: Vec::new(),
            pending: Vec::new(),
            forgotten: Vec::new(),
            views: HashMap::new(),
            modal_shown: false,
            kernel_modals: HashSet::new(),
            adopted: Vec::new(),
            services: HashMap::new(),
            gpu,
            settings,
            settings_changed: false,
            reported: Vec::new(),
            pool,
            bridge,
            next_call: 0,
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
        self.pending.push((owner.to_owned(), command));
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

    fn draw_panel(&mut self, owner: &str, objects: &SharedUi, panel: &str, ui: &mut egui::Ui) {
        let gpu = self.gpu.clone();
        self.view(owner, objects).show(objects, panel, ui, gpu.as_ref());
        self.report_service_failures(owner, objects);
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

    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId {
        let editor = self.editor(owner);
        self.pool.spawn_thread(owner, label, job, editor)
    }

    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId {
        self.next_call += 1;
        let id = CallId(self.next_call);
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
}
