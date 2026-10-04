//! The property grids: their rows made and read before the objects are locked, those around the
//! rows in sight only; drawn by the service of the module `properties`; the values changed written
//! once the objects are unlocked, and each change done recorded as one undo entry of the property's
//! module.

use std::collections::HashMap;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use uniwow_api::property_grid::{self, GridChange, GridInput, GridRow, PropertyGrid};
use uniwow_api::ui::{Handle, Object};
use uniwow_api::{AppliedChange, Editor, EditorBackend, PropertyInfo, PropertyKind, PropertyValue, egui, log};

use super::{idle, panic_text};

/// The rows made and read before and after those in sight.
const AROUND: usize = 32;
/// The rows made and read of a grid not drawn yet.
const FIRST: usize = 64;

/// A value a grid has being changed.
struct Edit {
    path: String,
    owner: String,
    label: String,
    kind: PropertyKind,
    /// The value before the change began, and the last one written.
    before: PropertyValue,
    last: PropertyValue,
}

/// A value to write once the objects are unlocked, with the undo entry to record after it.
struct Write {
    path: String,
    value: PropertyValue,
    entry: Option<Entry>,
}

struct Entry {
    owner: String,
    label: String,
    before: PropertyValue,
}

/// A value changed by hand in a property grid, which undo and redo write back.
struct PropertySet {
    editor: Editor,
    path: String,
    before: PropertyValue,
    after: PropertyValue,
}

impl PropertySet {
    fn set(&self, value: PropertyValue) {
        if let Err(error) = self.editor.write_property(&self.path, value) {
            log::warn!("'{}' could not be set back: {error}", self.path);
        }
    }
}

impl AppliedChange for PropertySet {
    fn undo(&mut self) {
        self.set(self.before);
    }

    fn redo(&mut self) {
        self.set(self.after);
    }
}

/// What the interface thread keeps of the property grids of a module.
#[derive(Default)]
pub(super) struct Grids {
    /// The rows of each grid made for this frame, from the first one made on, and the module of
    /// each property read.
    rows: HashMap<Handle, (usize, Vec<GridRow>)>,
    owners: HashMap<String, String>,
    /// The rows each grid had in sight.
    shown: HashMap<Handle, Range<usize>>,
    edits: HashMap<Handle, Edit>,
    /// The id each grid was drawn under, for the service to forget it once it is gone.
    ids: HashMap<Handle, egui::Id>,
    writes: Vec<Write>,
}

impl Grids {
    /// Makes and reads, before the objects are locked, the rows of each grid around those it had
    /// in sight: reading a property runs its module's code, which may lock objects.
    pub(super) fn read(
        &mut self,
        grids: &[(Handle, Arc<Vec<String>>)],
        infos: &HashMap<String, PropertyInfo>,
        editor: Option<&Editor>,
    ) {
        self.rows.clear();
        self.owners.clear();
        for (handle, paths) in grids {
            let around = match self.shown.get(handle) {
                Some(shown) => shown.start.saturating_sub(AROUND)..shown.end.saturating_add(AROUND),
                None => 0..FIRST,
            };
            let (first, end) = (around.start.min(paths.len()), around.end.min(paths.len()));
            let rows = paths[first..end]
                .iter()
                .map(|path| {
                    let info = infos.get(path);
                    if let Some(info) = info {
                        self.owners.insert(path.clone(), info.owner.clone());
                    }
                    GridRow {
                        path: path.clone(),
                        label: info.map_or_else(|| path.clone(), |info| info.label.clone()),
                        kind: info.map(|info| info.kind),
                        range: info.map_or([f64::MIN, f64::MAX], |info| info.range),
                        value: info.and(editor).and_then(|editor| editor.read_property(path).ok()),
                    }
                })
                .collect();
            self.rows.insert(*handle, (first, rows));
        }
    }

    /// Draws a property grid with the service, or says it is not running; makes what the user did.
    pub(super) fn show(
        &mut self,
        service: Option<Arc<dyn PropertyGrid>>,
        handle: Handle,
        object: &Object,
        ui: &mut egui::Ui,
        failures: &mut Vec<(&'static str, String)>,
    ) -> egui::Response {
        let size = egui::vec2(
            ui.available_width(),
            ui.available_height().max(object.minimum_height as f32),
        );
        let paths = object.paths.clone().unwrap_or_default();
        // A value being changed whose property the grid no longer shows is done.
        if self.edits.get(&handle).is_some_and(|edit| !paths.contains(&edit.path)) {
            self.finish(handle);
        }
        let Some(service) = service else {
            self.drop_edit(handle);
            return ui
                .allocate_ui(size, |ui| {
                    ui.weak("No property grid: the module properties is not running.")
                })
                .response;
        };
        let (first, rows) = self.rows.remove(&handle).unwrap_or_default();
        let id = ui.id().with(("uniwow-property-grid", handle));
        self.ids.insert(handle, id);
        let input = GridInput {
            count: paths.len(),
            first,
            rows: &rows,
        };
        let inner = ui.allocate_ui(size, |ui| {
            catch_unwind(AssertUnwindSafe(|| service.show(ui, id, &input)))
        });
        match inner.inner {
            Ok(output) => {
                self.shown.insert(handle, output.shown);
                let idle = idle(ui);
                self.change(handle, &rows, output.change, idle);
            }
            Err(panic) => {
                let message = format!("the property grid panicked: {}", panic_text(&*panic));
                failures.push((property_grid::SERVICE.id(), message));
                self.drop_edit(handle);
            }
        }
        inner.response
    }

    /// Makes what the user did to a value: a change under way written, recording nothing; a change
    /// done written and recorded as one undo entry from the value before it began; a change no
    /// longer under way, nothing being done by the user any more, put back.
    fn change(&mut self, handle: Handle, rows: &[GridRow], change: GridChange, idle: bool) {
        let (path, value, done) = match change {
            GridChange::None => {
                if idle {
                    self.drop_edit(handle);
                }
                return;
            }
            GridChange::Changing { path, value } => (path, value, false),
            GridChange::Finished { path, value } => (path, value, true),
        };
        let under_way = self.edits.get(&handle).filter(|edit| edit.path == path);
        let known = match under_way {
            Some(edit) => Some((edit.owner.clone(), edit.label.clone(), edit.kind, edit.before)),
            None => rows.iter().find(|row| row.path == path).and_then(|row| {
                let owner = self.owners.get(&path)?.clone();
                Some((owner, row.label.clone(), row.kind?, row.value?))
            }),
        };
        let Some((owner, label, kind, before)) = known else {
            log::warn!("a property grid changed '{path}', which it does not show with a value");
            return;
        };
        if value.kind() != kind || value.components().iter().any(|number| !number.is_finite()) {
            log::warn!("a property grid gave '{path}' a value that is not one of a {kind:?}");
            return;
        }
        // A change of another value under way ends that one.
        if self.edits.get(&handle).is_some_and(|edit| edit.path != path) {
            self.finish(handle);
        }
        let entry = if done {
            self.edits.remove(&handle);
            (value != before).then(|| Entry {
                owner,
                label: format!("Set {label}"),
                before,
            })
        } else {
            let edit = Edit {
                path: path.clone(),
                owner,
                label,
                kind,
                before,
                last: value,
            };
            self.edits.insert(handle, edit);
            None
        };
        self.writes.push(Write { path, value, entry });
    }

    /// Ends the change a grid has under way as done, with the last value written.
    fn finish(&mut self, handle: Handle) {
        if let Some(edit) = self.edits.remove(&handle)
            && edit.last != edit.before
        {
            self.writes.push(Write {
                path: edit.path,
                value: edit.last,
                entry: Some(Entry {
                    owner: edit.owner,
                    label: format!("Set {}", edit.label),
                    before: edit.before,
                }),
            });
        }
    }

    /// Puts back the value a grid was changing, when the grid can no longer end the change: gone,
    /// drawn without its service, or nothing being done by the user any more.
    fn drop_edit(&mut self, handle: Handle) {
        if let Some(edit) = self.edits.remove(&handle) {
            self.writes.push(Write {
                path: edit.path,
                value: edit.before,
                entry: None,
            });
        }
    }

    /// Forgets what it kept of the grids that are gone, a change under way put back.
    pub(super) fn forget_gone(&mut self, alive: impl Fn(&Handle) -> bool, service: Option<&Arc<dyn PropertyGrid>>) {
        let gone: Vec<Handle> = self.edits.keys().filter(|handle| !alive(handle)).copied().collect();
        for handle in gone {
            self.drop_edit(handle);
        }
        self.shown.retain(|handle, _| alive(handle));
        self.ids.retain(|handle, id| {
            let kept = alive(handle);
            if !kept && let Some(service) = service {
                service.forget(*id);
            }
            kept
        });
    }

    /// Writes the values changed, once the objects are unlocked: writing a property runs its
    /// module's code, which may lock objects. A change done is then recorded as one undo entry of
    /// the property's module.
    pub(super) fn write(&mut self, editor: Option<&Editor>, backend: Option<&Arc<dyn EditorBackend>>) {
        let writes = std::mem::take(&mut self.writes);
        let Some(editor) = editor else {
            return;
        };
        for write in writes {
            if let Err(error) = editor.write_property(&write.path, write.value) {
                log::warn!("'{}' could not be set: {error}", write.path);
                continue;
            }
            let (Some(entry), Some(backend)) = (write.entry, backend) else {
                continue;
            };
            let owner = Editor::new(backend.clone(), &entry.owner);
            let change = PropertySet {
                editor: owner.clone(),
                path: write.path,
                before: entry.before,
                after: write.value,
            };
            if let Err(error) = owner.record_change(&entry.label, Box::new(change)) {
                log::warn!("'{}' was not recorded: {error}", entry.label);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ops::Range;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use uniwow_api::property_grid::{GridChange, GridInput, GridOutput, PropertyGrid};
    use uniwow_api::serde_json::Value;
    use uniwow_api::ui::{Handle, Kind, SharedUi, Ui, lock};
    use uniwow_api::{
        AppliedChange, CommandInfo, Editor, EditorBackend, Event, PropertyInfo, PropertyKind, PropertyValue, egui,
    };

    use crate::draw::PanelView;

    type Recorded = Vec<(String, String, Box<dyn AppliedChange>)>;

    /// Numbers `grid/p<n>` labelled `P<n>`, of the module `grid`, each 0.25; tells what was read and
    /// written, whether the objects were free then, and what was recorded, by whom.
    struct Properties {
        objects: SharedUi,
        count: usize,
        reads: Mutex<Vec<(String, bool)>>,
        writes: Mutex<Vec<(String, PropertyValue, bool)>>,
        recorded: Mutex<Recorded>,
    }

    impl EditorBackend for Properties {
        fn commands(&self) -> Vec<CommandInfo> {
            Vec::new()
        }

        fn call(&self, _caller: &str, _name: &str, _arguments: Value) -> Result<Value, String> {
            Err("no command".to_owned())
        }

        fn publish(&self, _source: &str, _topic: &str, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn subscribe(&self, _caller: &str, _topic: &str) -> Result<u64, String> {
            Err("no event".to_owned())
        }

        fn next_event(&self, _caller: &str, _subscription: u64, _timeout: Duration) -> Result<Option<Event>, String> {
            Ok(None)
        }

        fn unsubscribe(&self, _subscription: u64) {}

        fn setting(&self, _caller: &str, _space: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn set_setting(&self, _caller: &str, _space: &str, _key: &str, _value: Value) -> Result<(), String> {
            Ok(())
        }

        fn begin_group(&self, _caller: &str, _label: &str) -> Result<(), String> {
            Ok(())
        }

        fn end_group(&self, _caller: &str) -> Result<(), String> {
            Ok(())
        }

        fn record_change(&self, caller: &str, label: &str, change: Box<dyn AppliedChange>) -> Result<(), String> {
            self.recorded
                .lock()
                .unwrap()
                .push((caller.to_owned(), label.to_owned(), change));
            Ok(())
        }

        fn properties(&self) -> Vec<PropertyInfo> {
            (0..self.count)
                .map(|number| PropertyInfo {
                    path: format!("grid/p{number}"),
                    owner: "grid".to_owned(),
                    label: format!("P{number}"),
                    kind: PropertyKind::Number,
                    range: [0.0, 1.0],
                })
                .collect()
        }

        fn read_property(&self, _caller: &str, path: &str) -> Result<PropertyValue, String> {
            let free = self.objects.try_lock().is_ok();
            self.reads.lock().unwrap().push((path.to_owned(), free));
            Ok(PropertyValue::Number(0.25))
        }

        fn write_property(&self, _caller: &str, path: &str, value: PropertyValue) -> Result<(), String> {
            let free = self.objects.try_lock().is_ok();
            self.writes.lock().unwrap().push((path.to_owned(), value, free));
            Ok(())
        }
    }

    /// A grid giving one change a frame, in order, then none, with these rows in sight; it keeps
    /// how many rows each input had, from which first, of how many.
    #[derive(Default)]
    struct Scripted {
        changes: Mutex<VecDeque<GridChange>>,
        shown: Mutex<Range<usize>>,
        inputs: Mutex<Vec<(usize, usize, usize)>>,
    }

    impl PropertyGrid for Scripted {
        fn show(&self, _ui: &mut egui::Ui, _id: egui::Id, input: &GridInput) -> GridOutput {
            self.inputs
                .lock()
                .unwrap()
                .push((input.count, input.first, input.rows.len()));
            GridOutput {
                change: self.changes.lock().unwrap().pop_front().unwrap_or(GridChange::None),
                shown: self.shown.lock().unwrap().clone(),
            }
        }

        fn forget(&self, _id: egui::Id) {}
    }

    struct Broken;

    impl PropertyGrid for Broken {
        fn show(&self, _ui: &mut egui::Ui, _id: egui::Id, _input: &GridInput) -> GridOutput {
            panic!("broken grid")
        }

        fn forget(&self, _id: egui::Id) {}
    }

    struct Fixture {
        shared: SharedUi,
        layout: Handle,
        backend: Arc<Properties>,
        service: Arc<Scripted>,
        panels: PanelView,
        ctx: egui::Context,
    }

    /// A panel holding a grid of `count` properties.
    fn fixture(count: usize) -> Fixture {
        let shared = Ui::new(Arc::new(|_job| {}));
        let layout = {
            let mut store = lock(&shared);
            let panel = store.panel("p");
            let layout = store.create(Kind::VBoxLayout, None).unwrap();
            store.add_to(panel, layout, [0, 0, 1, 1]).unwrap();
            let grid = store.create(Kind::PropertyGrid, None).unwrap();
            store.add_to(layout, grid, [0, 0, 1, 1]).unwrap();
            let paths = (0..count).map(|number| format!("grid/p{number}")).collect();
            store.set_paths(grid, paths).unwrap();
            layout
        };
        let backend = Arc::new(Properties {
            objects: shared.clone(),
            count,
            reads: Mutex::default(),
            writes: Mutex::default(),
            recorded: Mutex::default(),
        });
        let service = Arc::new(Scripted::default());
        let panels = PanelView {
            property_grid: Some(service.clone()),
            editor: Some(Editor::new(backend.clone(), "test")),
            backend: Some(backend.clone()),
            ..PanelView::default()
        };
        Fixture {
            shared,
            layout,
            backend,
            service,
            panels,
            ctx: egui::Context::default(),
        }
    }

    impl Fixture {
        /// A frame in which the grid gives `change`.
        fn frame(&mut self, change: GridChange) {
            self.service.changes.lock().unwrap().push_back(change);
            let mut output = self.ctx.run_ui(egui::RawInput::default(), |ui| {
                self.panels.show(&self.shared, "p", ui, None)
            });
            output.textures_delta.clear();
        }

        fn writes(&self) -> Vec<(String, f64)> {
            self.backend
                .writes
                .lock()
                .unwrap()
                .iter()
                .map(|(path, value, _)| (path.clone(), value.components()[0]))
                .collect()
        }

        fn labels(&self) -> Vec<(String, String)> {
            self.backend
                .recorded
                .lock()
                .unwrap()
                .iter()
                .map(|(owner, label, _)| (owner.clone(), label.clone()))
                .collect()
        }
    }

    fn changing(path: &str, value: f64) -> GridChange {
        GridChange::Changing {
            path: path.to_owned(),
            value: PropertyValue::Number(value),
        }
    }

    fn finished(path: &str, value: f64) -> GridChange {
        GridChange::Finished {
            path: path.to_owned(),
            value: PropertyValue::Number(value),
        }
    }

    fn written(writes: &[(&str, f64)]) -> Vec<(String, f64)> {
        writes
            .iter()
            .map(|(path, value)| ((*path).to_owned(), *value))
            .collect()
    }

    #[test]
    fn a_value_changed_in_a_grid_is_written_at_once_then_recorded_once_for_its_module() {
        let mut grid = fixture(2);
        grid.frame(changing("grid/p0", 0.5));
        grid.frame(changing("grid/p0", 0.6));
        assert!(grid.labels().is_empty(), "nothing recorded while it changes");
        grid.frame(finished("grid/p0", 0.7));
        grid.frame(finished("grid/p1", 0.25));
        assert_eq!(
            grid.writes(),
            written(&[("grid/p0", 0.5), ("grid/p0", 0.6), ("grid/p0", 0.7), ("grid/p1", 0.25)])
        );
        assert!(
            grid.backend.writes.lock().unwrap().iter().all(|(_, _, free)| *free),
            "written once the objects are unlocked"
        );
        assert_eq!(
            grid.labels(),
            vec![("grid".to_owned(), "Set P0".to_owned())],
            "one entry, of the property's module; none for a value left as it was"
        );
        let mut recorded = grid.backend.recorded.lock().unwrap().pop().unwrap().2;
        recorded.undo();
        assert_eq!(
            grid.writes().last(),
            Some(&("grid/p0".to_owned(), 0.25)),
            "undone to the value before"
        );
        recorded.redo();
        assert_eq!(grid.writes().last(), Some(&("grid/p0".to_owned(), 0.7)));
    }

    #[test]
    fn only_the_rows_around_those_in_sight_are_made_and_read_while_the_objects_are_free() {
        let mut grid = fixture(1000);
        *grid.service.shown.lock().unwrap() = 400..420;
        grid.frame(GridChange::None);
        let first = grid.backend.reads.lock().unwrap().len();
        grid.frame(GridChange::None);
        let reads = grid.backend.reads.lock().unwrap().clone();
        assert_eq!(first, 64, "a grid not drawn yet: its first rows");
        assert_eq!(reads.len() - first, 84, "the rows in sight and 32 around");
        assert_eq!(reads[first].0, "grid/p368");
        assert!(reads.iter().all(|(_, free)| *free));
        assert_eq!(
            *grid.service.inputs.lock().unwrap(),
            vec![(1000, 0, 64), (1000, 368, 84)]
        );
    }

    #[test]
    fn a_change_under_way_is_put_back_when_its_grid_goes_its_service_stops_or_the_user_leaves_it() {
        let mut gone = fixture(1);
        gone.frame(changing("grid/p0", 0.5));
        lock(&gone.shared).destroy(gone.layout).unwrap();
        gone.frame(GridChange::None);
        assert_eq!(gone.writes(), written(&[("grid/p0", 0.5), ("grid/p0", 0.25)]));

        let mut stopped = fixture(1);
        stopped.frame(changing("grid/p0", 0.5));
        stopped.panels.property_grid = None;
        stopped.frame(GridChange::None);
        assert_eq!(stopped.writes(), written(&[("grid/p0", 0.5), ("grid/p0", 0.25)]));

        let mut left = fixture(1);
        left.frame(changing("grid/p0", 0.5));
        left.frame(GridChange::None);
        assert_eq!(
            left.writes(),
            written(&[("grid/p0", 0.5), ("grid/p0", 0.25)]),
            "nothing held"
        );
        for fixture in [gone, stopped, left] {
            assert!(fixture.labels().is_empty());
        }
    }

    #[test]
    fn a_change_of_another_value_ends_the_one_under_way() {
        let mut grid = fixture(2);
        grid.frame(changing("grid/p0", 0.5));
        grid.frame(changing("grid/p1", 0.6));
        assert_eq!(grid.labels(), vec![("grid".to_owned(), "Set P0".to_owned())]);
    }

    #[test]
    fn a_value_the_grid_may_not_give_is_not_written() {
        let mut grid = fixture(1);
        grid.frame(GridChange::Changing {
            path: "grid/p0".to_owned(),
            value: PropertyValue::Vector([0.0; 3]),
        });
        grid.frame(changing("grid/p0", f64::NAN));
        grid.frame(changing("other/x", 0.5));
        assert!(grid.writes().is_empty());
    }

    #[test]
    fn a_panic_of_the_grid_is_kept_for_its_provider_to_be_reported() {
        let mut grid = fixture(1);
        grid.panels.property_grid = Some(Arc::new(Broken));
        grid.frame(GridChange::None);
        let failures = grid.panels.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "property-grid");
        assert!(failures[0].1.contains("broken grid"), "{}", failures[0].1);
    }
}
