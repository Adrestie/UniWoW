use std::any::Any;

use crate::theme::Level;
use crate::ui::SharedUi;
use crate::{
    CallId, Command, Editor, JobContext, JobFn, JobId, Priority, PropertyInfo, PropertyValue, ServiceKey, egui,
    egui_wgpu,
};

/// What the kernel offers to modules. Implemented by the kernel only.
pub trait Host {
    fn publish(&mut self, source: &str, topic: &str, payload: serde_json::Value);
    fn execute(&mut self, owner: &str, command: Box<dyn Command>);
    fn forget_document(&mut self, owner: &str, document: &str);
    fn service(&self, id: &str) -> Option<&(dyn Any + Send + Sync)>;
    /// The module providing the service `id`.
    fn service_provider(&self, id: &str) -> Option<String>;
    fn gpu(&self) -> Option<&egui_wgpu::RenderState>;
    fn gpu_memory(&self) -> Option<u64>;
    fn draw_panel(&mut self, owner: &str, objects: &SharedUi, panel: &str, ui: &mut egui::Ui);
    fn draw_dialogs(&mut self, owner: &str, objects: &SharedUi, egui: &egui::Context);
    fn adopt_objects(&mut self, owner: &str, objects: &SharedUi);
    fn setting(&self, module: &str, key: &str) -> Option<serde_json::Value>;
    fn set_setting(&mut self, module: &str, key: &str, value: serde_json::Value);
    fn report_failure(&mut self, reporter: &str, culprit: &str, message: &str);
    fn spawn(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    /// A job of the pool run in the order of `priority` among those waiting; by default in the
    /// order of `spawn`.
    fn spawn_with_priority(&mut self, owner: &str, label: &str, priority: Priority, job: JobFn) -> JobId {
        let _ = priority;
        self.spawn(owner, label, job)
    }
    fn spawn_thread(&mut self, owner: &str, label: &str, job: JobFn) -> JobId;
    fn cancel(&mut self, owner: &str, job: JobId);
    fn call(&mut self, caller: &str, name: &str, arguments: serde_json::Value) -> CallId;
    fn editor(&self, caller: &str) -> Editor;
    /// Shows `text` of `owner` in a notice over the editor's window for a few seconds.
    fn notify(&mut self, _owner: &str, _level: Level, _text: &str) {}
    /// Opens a gesture of `owner` (`Context::begin_gesture`); its number. None by default.
    fn begin_gesture(&mut self, _owner: &str, _label: &str) -> u64 {
        0
    }
    /// Ends the gesture `gesture` of `owner`.
    fn end_gesture(&mut self, _owner: &str, _gesture: u64) {}
    /// Opens the panel `panel` of `owner`, closed or not, and puts it in front of its part of the
    /// dock.
    fn show_panel(&mut self, _owner: &str, _panel: &str) {}
    /// Lays the dock out as `arrangement` says, the layout before kept to come back to; with none,
    /// puts back the layout kept.
    fn arrange(&mut self, _owner: &str, _arrangement: Option<Arrangement>) {}
    /// Makes the gesture `gesture` of `owner`, opened at an earlier frame and not ended, the one its
    /// calls and changes join again, as `begin_gesture` made it; whether it still is open.
    fn resume_gesture(&mut self, _owner: &str, _gesture: u64) -> bool {
        false
    }
    /// Draws the object `handle` of the interface objects the module `module` handed the kernel
    /// (`adopt_objects`; a compiled module's own), a layout and what it holds, inside a window of
    /// `owner`; its signals go to `module`. Whether it was drawn: false by default.
    /// Draws in `ui` the sections other modules declared in the panel `panel` of `owner`.
    fn draw_sections(&mut self, _owner: &str, _panel: &str, _ui: &mut egui::Ui) {}

    fn draw_object(&mut self, _owner: &str, _module: &str, _handle: u64, _ui: &mut egui::Ui) -> bool {
        false
    }
}

/// A gesture of the user open (`Context::begin_gesture`), by its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Gesture(pub u64);

/// A layout of the dock for a task: the panels of each part, each a (module, panel) pair.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub center: Vec<(String, String)>,
    pub left: Vec<(String, String)>,
    pub right: Vec<(String, String)>,
    pub bottom: Vec<(String, String)>,
}

/// Access to the kernel for one module, passed to every `Module` method after `register`.
pub struct Context<'a> {
    host: &'a mut dyn Host,
    module: &'a str,
    /// Who called the command `Module::on_command` runs.
    caller: Option<&'a str>,
}

impl<'a> Context<'a> {
    pub fn new(host: &'a mut dyn Host, module: &'a str) -> Self {
        Self {
            host,
            module,
            caller: None,
        }
    }

    /// The context of `Module::on_command`, for a command `caller` called.
    pub fn for_command(host: &'a mut dyn Host, module: &'a str, caller: &'a str) -> Self {
        Self {
            host,
            module,
            caller: Some(caller),
        }
    }

    /// In `Module::on_command`, who called the command: a module id, `kernel`, or
    /// `<module>#<name>` for a script run of that module.
    pub fn command_caller(&self) -> Option<&str> {
        self.caller
    }

    /// Id of the module this context belongs to.
    pub fn module_id(&self) -> &str {
        self.module
    }

    pub fn publish(&mut self, topic: &str, payload: serde_json::Value) {
        self.host.publish(self.module, topic, payload);
    }

    /// Publishes `payload` serialised to JSON. Subscribers read it back with `Event::decode` into a
    /// type of their own: modules share no Rust type, only the shape of the JSON.
    pub fn publish_as<T: serde::Serialize>(&mut self, topic: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => self.publish(topic, value),
            Err(error) => log::error!("'{topic}' not published: {error}"),
        }
    }

    /// Queues an undoable command on this module.
    pub fn execute(&mut self, command: impl Command + 'static) {
        self.host.execute(self.module, Box::new(command));
    }

    /// Forgets from the undo history, done or undone, the changes of this module whose
    /// `Command::document` is `document`: for a document closed without saving, which they would
    /// otherwise bring back.
    pub fn forget_document(&mut self, document: &str) {
        self.host.forget_document(self.module, document);
    }

    /// Returns a clone of the service, if a running module provides it.
    pub fn service<T: Any + Clone + Send + Sync>(&self, key: ServiceKey<T>) -> Option<T> {
        let service = self.host.service(key.id())?;
        let typed = service.downcast_ref::<T>().cloned();
        if typed.is_none() {
            log::error!(
                "the service '{}' is not provided as {}: provider and consumer use different keys",
                key.id(),
                std::any::type_name::<T>()
            );
        }
        typed
    }

    /// The GPU device used by the editor window.
    pub fn gpu(&self) -> Option<&egui_wgpu::RenderState> {
        self.host.gpu()
    }

    /// The memory of its own of the GPU the editor draws with, in bytes, when the system tells it.
    pub fn gpu_memory(&self) -> Option<u64> {
        self.host.gpu_memory()
    }

    /// Draws the panel `panel` of interface objects (section 3) in `ui`, as the kernel draws those
    /// of compiled modules, and turns what the user does into their signals.
    pub fn draw_objects(&mut self, objects: &SharedUi, panel: &str, ui: &mut egui::Ui) {
        self.host.draw_panel(self.module, objects, panel, ui);
    }

    /// Draws the object `handle` of the module `module`'s interface objects in `ui`: a drawing of
    /// its own inside a window of this module, such as a component's in the Inspector (step
    /// 15.12b). Whether it was drawn: the module runs and has handed the kernel its objects.
    /// Draws in `ui`, where it stands in this module's panel `panel`, the sections other modules
    /// declared there (`Registrar::section`), each by its module with its own context.
    pub fn draw_sections(&mut self, panel: &str, ui: &mut egui::Ui) {
        self.host.draw_sections(self.module, panel, ui);
    }

    pub fn draw_object_of(&mut self, module: &str, handle: u64, ui: &mut egui::Ui) -> bool {
        self.host.draw_object(self.module, module, handle, ui)
    }

    /// Draws the dialogs shown among interface objects, each in a modal window over the editor.
    pub fn draw_dialogs(&mut self, objects: &SharedUi, egui: &egui::Context) {
        self.host.draw_dialogs(self.module, objects, egui);
    }

    /// Hands the kernel this module's interface objects (section 3), as it has those of a compiled
    /// module: it moves their players on at each frame and records each change of the tracks of
    /// their sequences as an undo entry of this module.
    pub fn adopt_objects(&mut self, objects: &SharedUi) {
        self.host.adopt_objects(self.module, objects);
    }

    /// Reads a setting of this module, kept between sessions.
    pub fn setting(&self, key: &str) -> Option<serde_json::Value> {
        self.host.setting(self.module, key)
    }

    pub fn set_setting(&mut self, key: &str, value: serde_json::Value) {
        self.host.set_setting(self.module, key, value);
    }

    /// The module providing a service, to report it when its code fails (F5).
    pub fn service_provider<T>(&self, key: ServiceKey<T>) -> Option<String> {
        self.host.service_provider(key.id())
    }

    /// Reports that the module `culprit` misbehaved in code this module runs on its behalf, such
    /// as a viewport layer. The kernel disables it as if it had panicked, naming this module.
    pub fn report_failure(&mut self, culprit: &str, message: &str) {
        self.host.report_failure(self.module, culprit, message);
    }

    /// Runs `job` on a worker thread (rule T2), before the jobs given a distance
    /// (`Priority::FIRST`). Its value, cancellation or panic comes back to this module on the
    /// interface thread through `Module::on_job`.
    pub fn spawn<T: Any + Send>(&mut self, label: &str, job: impl FnOnce(&JobContext) -> T + Send + 'static) -> JobId {
        self.host
            .spawn(self.module, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Like `spawn`, run in the order of `priority` among the jobs waiting, whatever their module:
    /// for a job reading or putting on the GPU what lies somewhere, `Priority::distance` of it
    /// from the camera, so that the nearest comes first.
    pub fn spawn_with_priority<T: Any + Send>(
        &mut self,
        label: &str,
        priority: Priority,
        job: impl FnOnce(&JobContext) -> T + Send + 'static,
    ) -> JobId {
        self.host.spawn_with_priority(
            self.module,
            label,
            priority,
            Box::new(move |context| Box::new(job(context))),
        )
    }

    /// Like `spawn`, on a thread of its own instead of the pool: for work that waits, such as a
    /// script, so that the pool's threads stay free for computations (rule T2).
    pub fn spawn_thread<T: Any + Send>(
        &mut self,
        label: &str,
        job: impl FnOnce(&JobContext) -> T + Send + 'static,
    ) -> JobId {
        self.host
            .spawn_thread(self.module, label, Box::new(move |context| Box::new(job(context))))
    }

    /// Opens a gesture of the user, `label` in the Edit menu, on the interface thread: every undoable
    /// change applied while it is open, by this module's own commands and by the commands its calls
    /// (`Context::call`) run in any module, those calls' own calls included, joins one entry of the
    /// history, its parts owned each by the module whose data it changed (rule E2). It closes once
    /// ended and every one of its calls answered; one left open is closed at the end of the frames
    /// after its last answer, or when its module fails.
    pub fn begin_gesture(&mut self, label: &str) -> Gesture {
        Gesture(self.host.begin_gesture(self.module, label))
    }

    /// Opens this module's panel `panel`, closed or not, and puts it in front of its part of the
    /// dock, at the end of the frame: a quest clicked in the Inspector shown in the panel *Quests*.
    pub fn show_panel(&mut self, panel: &str) {
        self.host.show_panel(self.module, panel);
    }

    /// Lays the dock out for a task (the effects: their view in the middle, their tree on the
    /// left...), at the end of the frame; the layout before is kept for `restore_arrangement`.
    pub fn arrange(&mut self, arrangement: Arrangement) {
        self.host.arrange(self.module, Some(arrangement));
    }

    /// Puts back the layout kept by `arrange`.
    pub fn restore_arrangement(&mut self) {
        self.host.arrange(self.module, None);
    }

    /// Ends the gesture `gesture`: its entry is made once its calls are answered.
    pub fn end_gesture(&mut self, gesture: Gesture) {
        self.host.end_gesture(self.module, gesture.0);
    }

    /// Takes up again the gesture `gesture` this module opened at an earlier frame and has not
    /// ended: a drag held over many frames, its changes made as the pointer moves, one undo entry
    /// once let go. Called at each frame of the drag, still or not, for the kernel not to take it
    /// for one left open. Whether it still is open.
    pub fn resume_gesture(&mut self, gesture: Gesture) -> bool {
        self.host.resume_gesture(self.module, gesture.0)
    }

    /// Asks one of this module's jobs to stop (as the Jobs panel's Cancel button does).
    pub fn cancel(&mut self, job: JobId) {
        self.host.cancel(self.module, job);
    }

    /// Calls a named command from the interface thread. The answer comes back at the end of the
    /// frame through `Module::on_reply`, with the returned id.
    pub fn call(&mut self, name: &str, arguments: serde_json::Value) -> CallId {
        self.host.call(self.module, name, arguments)
    }

    /// Shows `text` in a notice over the editor's window for a few seconds, under the name of this
    /// module: the outcome of what the user asked, or a problem. A notice of the same module and
    /// text shown again counts the times instead of showing twice.
    pub fn notify(&mut self, level: Level, text: &str) {
        self.host.notify(self.module, level, text);
    }

    /// A handle to the editor for other threads, acting for this module.
    pub fn editor(&self) -> Editor {
        self.host.editor(self.module)
    }

    /// Every animatable property of running modules.
    pub fn properties(&self) -> Vec<PropertyInfo> {
        self.editor().properties()
    }

    pub fn read_property(&self, path: &str) -> Result<PropertyValue, String> {
        self.editor().read_property(path)
    }

    /// Writes an animatable property, without the history.
    pub fn write_property(&self, path: &str, value: PropertyValue) -> Result<(), String> {
        self.editor().write_property(path, value)
    }
}
