//! Modal windows for the other modules, as `QDialog`. The module that opens one, with the command
//! `ui.dialog`, gives its title, its text and its buttons; the button chosen is published as the
//! event `ui.dialog_answered`, and that module does what it decided for it. Built with the interface
//! objects of the core.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use uniwow_api::serde_json::{Value, json};
use uniwow_api::ui::{self, Handle, Kind, Property, SharedUi, Signal, Ui};
use uniwow_api::{Context, DIALOG_ANSWERED_TOPIC, DIALOG_COMMAND, Module, Registrar, egui, log};

/// A window asked for.
#[derive(Debug, PartialEq)]
struct Request {
    number: u64,
    /// Who asked for it.
    caller: String,
    title: String,
    text: String,
    /// Id and label of each button, left to right.
    buttons: Vec<(String, String)>,
    /// The button Escape and the close button stand for.
    escape: String,
}

type Job = Box<dyn FnOnce() + Send>;

/// The most windows one caller may have waiting.
const MAX_WAITING: usize = 8;

struct DialogsModule {
    ui: SharedUi,
    /// The slots of the objects, run once the windows are drawn.
    jobs: Arc<Mutex<Vec<Job>>>,
    /// The buttons chosen: the window's number and the button's id.
    answers: Arc<Mutex<Vec<(u64, String)>>>,
    waiting: VecDeque<Request>,
    /// The window shown: its number and its object.
    shown: Option<(u64, Handle)>,
    next: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Default for DialogsModule {
    fn default() -> Self {
        let jobs: Arc<Mutex<Vec<Job>>> = Arc::default();
        let queue = jobs.clone();
        Self {
            ui: Ui::new(Arc::new(move |job| lock(&queue).push(job))),
            jobs,
            answers: Arc::default(),
            waiting: VecDeque::new(),
            shown: None,
            next: 1,
        }
    }
}

impl Module for DialogsModule {
    fn register(&mut self, reg: &mut Registrar) {
        reg.command(
            DIALOG_COMMAND,
            "Opens a modal window with its caller's title, text and buttons; the button chosen comes back as the event ui.dialog_answered.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "text": { "type": "string" },
                    "buttons": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "id": { "type": "string" }, "label": { "type": "string" } },
                            "required": ["id", "label"]
                        },
                        "minItems": 1
                    },
                    "escape": { "type": "string", "description": "The id of the button Escape stands for; the last one by default" }
                },
                "required": ["text", "buttons"]
            }),
            json!({ "type": "object", "properties": { "dialog": { "type": "integer" } } }),
        );
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        if name != DIALOG_COMMAND {
            return Err(format!("'{name}' is not a command of the dialogs"));
        }
        let caller = ctx.command_caller().unwrap_or("unknown");
        let first = arguments["first"].as_bool() == Some(true);
        let number = self.ask(request(self.next, caller, &arguments)?, first)?;
        Self::answer_unshown(self.show_next(), ctx);
        Ok(json!({ "dialog": number }))
    }

    fn windows_ui(&mut self, egui: &egui::Context, ctx: &mut Context) {
        ctx.draw_dialogs(&self.ui, egui);
        let jobs = std::mem::take(&mut *lock(&self.jobs));
        for job in jobs {
            job();
        }
        let answers = std::mem::take(&mut *lock(&self.answers));
        for (number, button) in answers {
            let Some((_, dialog)) = self.shown.filter(|(shown, _)| *shown == number) else {
                continue;
            };
            self.shown = None;
            let _ = ui::lock(&self.ui).destroy(dialog);
            ctx.publish(DIALOG_ANSWERED_TOPIC, json!({ "dialog": number, "button": button }));
        }
        Self::answer_unshown(self.show_next(), ctx);
    }
}

impl DialogsModule {
    /// Puts a window in the queue, at its head when `first`; returns its number.
    fn ask(&mut self, request: Request, first: bool) -> Result<u64, String> {
        let waiting = self.waiting.iter().filter(|r| r.caller == request.caller).count();
        if waiting >= MAX_WAITING {
            return Err(format!(
                "'{}' has {MAX_WAITING} windows waiting already",
                request.caller
            ));
        }
        self.next += 1;
        let number = request.number;
        if first {
            self.waiting.push_front(request);
        } else {
            self.waiting.push_back(request);
        }
        Ok(number)
    }

    /// Shows the next window waiting, unless one is shown. Returns the windows that could not be
    /// built, each with the button Escape stands for; the next one is tried after each.
    fn show_next(&mut self) -> Vec<(u64, String)> {
        let mut unshown = Vec::new();
        while self.shown.is_none() {
            let Some(request) = self.waiting.pop_front() else {
                break;
            };
            let (number, escape) = (request.number, request.escape.clone());
            if let Err(error) = self.build(request) {
                log::error!("window {number} could not be shown: {error}");
                unshown.push((number, escape));
            }
        }
        unshown
    }

    /// Answers each window of `unshown` with the button Escape stands for, so that the module that
    /// asked for it waits no more.
    fn answer_unshown(unshown: Vec<(u64, String)>, ctx: &mut Context) {
        for (number, escape) in unshown {
            ctx.publish(DIALOG_ANSWERED_TOPIC, json!({ "dialog": number, "button": escape }));
        }
    }

    /// Builds the window of `request` and shows it; on failure, the objects made for it go.
    fn build(&mut self, request: Request) -> Result<(), String> {
        let mut store = ui::lock(&self.ui);
        let dialog = store.create(Kind::Dialog, None)?;
        let built = Self::fill(&mut store, dialog, request, &self.answers);
        match built {
            Ok(number) => {
                self.shown = Some((number, dialog));
                Ok(())
            }
            Err(error) => {
                let _ = store.destroy(dialog);
                Err(error)
            }
        }
    }

    /// Fills the window `dialog` with the text and the buttons of `request`, each button telling
    /// `answers`, and shows it; returns its number.
    fn fill(
        store: &mut Ui,
        dialog: Handle,
        request: Request,
        answers: &Arc<Mutex<Vec<(u64, String)>>>,
    ) -> Result<u64, String> {
        let cell = [0, 0, 1, 1];
        store.set_text(dialog, Property::Title, &request.title)?;
        let layout = store.create(Kind::VBoxLayout, None)?;
        store.add_to(dialog, layout, cell)?;
        let text = store.create(Kind::Label, None)?;
        store.add_to(layout, text, cell)?;
        store.set_text(text, Property::Text, &request.text)?;
        let row = store.create(Kind::HBoxLayout, None)?;
        store.add_to(layout, row, cell)?;
        let number = request.number;
        for (id, label) in request.buttons {
            let button = store.create(Kind::PushButton, None)?;
            store.add_to(row, button, cell)?;
            store.set_text(button, Property::Text, &label)?;
            let answers = answers.clone();
            store.connect(
                button,
                Signal::Clicked,
                Arc::new(move |_| lock(&answers).push((number, id.clone()))),
            )?;
        }
        let answers = answers.clone();
        let escape = request.escape;
        store.connect(
            dialog,
            Signal::Rejected,
            Arc::new(move |_| lock(&answers).push((number, escape.clone()))),
        )?;
        store.set_numbers(dialog, Property::Visible, &[1.0])?;
        Ok(number)
    }
}

/// Reads the arguments of `ui.dialog`.
fn request(number: u64, caller: &str, arguments: &Value) -> Result<Request, String> {
    let text = |key: &str| arguments[key].as_str().unwrap_or_default().to_owned();
    let buttons: Vec<(String, String)> = arguments["buttons"]
        .as_array()
        .ok_or("'buttons' must be a list")?
        .iter()
        .map(|button| match (button["id"].as_str(), button["label"].as_str()) {
            (Some(id), Some(label)) => Ok((id.to_owned(), label.to_owned())),
            _ => Err("each button has an 'id' and a 'label'".to_owned()),
        })
        .collect::<Result<_, _>>()?;
    let last = buttons.last().ok_or("a window needs a button")?.0.clone();
    let escape = arguments["escape"].as_str().map_or(last, str::to_owned);
    if !buttons.iter().any(|(id, _)| *id == escape) {
        return Err(format!("'escape' names no button: '{escape}'"));
    }
    Ok(Request {
        number,
        caller: caller.to_owned(),
        title: text("title"),
        text: text("text"),
        buttons,
        escape,
    })
}

uniwow_api::export_module!(DialogsModule::default());

#[cfg(test)]
mod tests {
    use uniwow_api::serde_json::json;
    use uniwow_api::ui;

    use super::{DialogsModule, MAX_WAITING, request};

    #[test]
    fn a_request_names_its_buttons_and_the_one_escape_stands_for() {
        let asked = json!({
            "title": "Unsaved changes",
            "text": "Save?",
            "buttons": [{ "id": "save", "label": "Save" }, { "id": "cancel", "label": "Cancel" }],
        });
        let read = request(7, "timeline", &asked).unwrap();
        assert_eq!(
            (read.number, read.escape.as_str(), read.buttons.len()),
            (7, "cancel", 2)
        );
        assert!(request(1, "timeline", &json!({ "text": "x", "buttons": [] })).is_err());
        assert!(
            request(
                1,
                "timeline",
                &json!({ "buttons": [{ "id": "a", "label": "A" }], "escape": "b" })
            )
            .is_err()
        );
    }

    #[test]
    fn windows_asked_for_at_once_are_shown_one_after_the_other() {
        let mut dialogs = DialogsModule::default();
        let asked = json!({ "text": "x", "buttons": [{ "id": "ok", "label": "OK" }] });
        dialogs.waiting.push_back(request(1, "timeline", &asked).unwrap());
        dialogs.waiting.push_back(request(2, "timeline", &asked).unwrap());
        assert!(dialogs.show_next().is_empty());
        assert!(dialogs.show_next().is_empty());
        assert_eq!(ui::lock(&dialogs.ui).dialogs().len(), 1);
        assert_eq!(dialogs.shown.map(|(number, _)| number), Some(1));
        assert_eq!(dialogs.waiting.len(), 1);
    }

    #[test]
    fn the_kernel_question_goes_first_and_a_caller_cannot_flood_the_queue() {
        let mut dialogs = DialogsModule::default();
        let asked = json!({ "text": "x", "buttons": [{ "id": "ok", "label": "OK" }] });
        for _ in 0..MAX_WAITING {
            let next = request(dialogs.next, "lua", &asked).unwrap();
            dialogs.ask(next, false).unwrap();
        }
        let next = request(dialogs.next, "lua", &asked).unwrap();
        assert!(dialogs.ask(next, false).is_err());
        let next = request(dialogs.next, "kernel", &asked).unwrap();
        let kernel = dialogs.ask(next, true).unwrap();
        assert_eq!(dialogs.waiting.front().map(|r| r.number), Some(kernel));
    }
}
