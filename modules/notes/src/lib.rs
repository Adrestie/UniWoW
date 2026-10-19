//! The notes of the editor (milestone 15, step 15.12): a text and a colour the user gives any entity
//! of the scene, a component declared through `scene.declare` as a module of any language would,
//! read and written through its commands, each change one undo entry; kept in the project, never
//! exported. The Hierarchy colours the rows of the entities noted through `notes.colours`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::scene::EntityId;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::{CallId, Command, Context, Module, Registrar, egui, log};

/// The id of the module.
const MODULE: &str = "notes";

/// A note: its text and its colour, red, green and blue from 0 to 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub text: String,
    pub colour: Option<[f32; 3]>,
}

/// The notes by entity, and a number changed with them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub by: BTreeMap<EntityId, Note>,
    pub generation: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A colour written `#rrggbb`.
pub fn colour_of(text: &str) -> Result<[f32; 3], String> {
    let hex = text.trim().trim_start_matches('#');
    let at = |from: usize| {
        hex.get(from..from + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map(|value| f32::from(value) / 255.0)
    };
    match (hex.len(), at(0), at(2), at(4)) {
        (6, Some(r), Some(g), Some(b)) => Ok([r, g, b]),
        _ => Err(format!("'{text}' is not a colour #rrggbb")),
    }
}

/// `colour` written `#rrggbb`.
pub fn colour_text(colour: [f32; 3]) -> String {
    let [r, g, b] = colour.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl Notes {
    /// The fields of the note of `id`, as the Inspector reads them: its text, its colour or null.
    pub fn fields(&self, id: &EntityId) -> Value {
        let note = self.by.get(id);
        json!({
            "note": note.map(|note| note.text.as_str()).unwrap_or_default(),
            "colour": note.and_then(|note| note.colour),
        })
    }

    /// The notes with the `field` (note or colour) of `ids` changed to `change`: a text, a colour
    /// `#rrggbb`, nothing to take it away; a note of no text and no colour goes.
    pub fn written(&self, ids: &[EntityId], field: &str, change: &str) -> Result<Notes, String> {
        let mut after = self.clone();
        for id in ids {
            let mut note = after.by.remove(id).unwrap_or(Note {
                text: String::new(),
                colour: None,
            });
            match field {
                "note" => note.text = change.to_owned(),
                "colour" if change.trim().is_empty() => note.colour = None,
                "colour" => note.colour = Some(colour_of(change)?),
                other => return Err(format!("a note has no field '{other}'")),
            }
            if !note.text.is_empty() || note.colour.is_some() {
                after.by.insert(*id, note);
            }
        }
        Ok(after)
    }

    /// The notes as the project keeps them, by entity.
    pub fn section(&self) -> Value {
        let notes: Vec<Value> = self
            .by
            .iter()
            .map(
                |(id, note)| json!({ "id": id.to_string(), "text": note.text, "colour": note.colour.map(colour_text) }),
            )
            .collect();
        json!({ "notes": notes })
    }

    pub fn of_section(section: &Value) -> Notes {
        let by = section["notes"]
            .as_array()
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(|note| {
                        Some((
                            note["id"].as_str()?.parse().ok()?,
                            Note {
                                text: note["text"].as_str().unwrap_or_default().to_owned(),
                                colour: note["colour"].as_str().and_then(|colour| colour_of(colour).ok()),
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Notes { by, generation: 0 }
    }
}

/// The notes from and to: one undo entry.
struct SetNotes {
    label: String,
    before: BTreeMap<EntityId, Note>,
    after: BTreeMap<EntityId, Note>,
}

impl Command for SetNotes {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut NotesModule = module.downcast_mut().expect("the notes");
        module.set(self.after.clone());
    }

    fn revert(&mut self, module: &mut dyn std::any::Any) {
        let module: &mut NotesModule = module.downcast_mut().expect("the notes");
        module.set(self.before.clone());
    }
}

/// The component the module declares: every kind, a text and a colour.
pub fn declaration() -> Value {
    json!({ "components": [{
        "name": "note",
        "label": "Notes of the editor",
        "fields": [
            { "name": "note", "label": "Note", "kind": "text" },
            { "name": "colour", "label": "Colour", "kind": "colour" },
        ],
        "read": "notes.read",
        "write": "notes.write",
    }] })
}

#[derive(Default)]
struct NotesModule {
    notes: Arc<Mutex<Notes>>,
    declaring: Option<CallId>,
    declared: bool,
}

impl NotesModule {
    fn set(&mut self, by: BTreeMap<EntityId, Note>) {
        let mut notes = lock(&self.notes);
        notes.by = by;
        notes.generation += 1;
    }

    fn ids(arguments: &Value) -> Result<Vec<EntityId>, String> {
        arguments["ids"]
            .as_array()
            .ok_or("'ids' is a list of ids")?
            .iter()
            .map(|id| id.as_str().ok_or("'ids' is a list of ids as text")?.parse())
            .collect()
    }
}

impl Module for NotesModule {
    fn register(&mut self, reg: &mut Registrar) {
        let ids = json!({ "type": "object", "properties": { "ids": { "type": "array" } }, "required": ["ids"] });
        let notes = self.notes.clone();
        reg.command(
            "notes.read",
            "The notes of the entities 'ids': their text and their colour (null for none).",
            ids,
            json!({ "type": "object", "properties": { "entities": { "type": "array" } } }),
        )
        .command(
            "notes.write",
            "Changes the 'field' (note, or colour as #rrggbb, empty to take it away) of the notes of the entities \
             'ids' to 'change'; one undo entry.",
            json!({ "type": "object", "properties": { "ids": { "type": "array" }, "field": { "type": "string" }, "change": { "type": "string" } }, "required": ["ids", "field", "change"] }),
            json!({ "type": "object", "properties": { "changed": { "type": "integer" }, "refused": { "type": "array" } } }),
        )
        .command_on_caller(
            "notes.colours",
            "The colours of the entities noted, as #rrggbb by id, with the number of the notes; nothing but the \
             number when it is still 'generation'.",
            json!({ "type": "object", "properties": { "generation": { "type": "integer" } } }),
            json!({ "type": "object", "properties": { "generation": { "type": "integer" }, "colours": { "type": "object" } } }),
            Arc::new(move |arguments| {
                let notes = lock(&notes);
                if arguments["generation"].as_u64() == Some(notes.generation) {
                    return Ok(json!({ "generation": notes.generation }));
                }
                let colours: uniwow_api::serde_json::Map<String, Value> = notes
                    .by
                    .iter()
                    .filter_map(|(id, note)| Some((id.to_string(), Value::from(colour_text(note.colour?)))))
                    .collect();
                Ok(json!({ "generation": notes.generation, "colours": colours }))
            }),
        );
    }

    fn windows_ui(&mut self, _egui: &egui::Context, ctx: &mut Context) {
        if !self.declared {
            self.declared = true;
            self.declaring = Some(ctx.call("scene.declare", declaration()));
        }
    }

    fn on_reply(&mut self, call: CallId, result: Result<Value, String>, _ctx: &mut Context) {
        if self.declaring == Some(call) {
            self.declaring = None;
            if let Err(reason) = result {
                log::warn!("{MODULE}: the notes are not declared to the scene: {reason}");
            }
        }
    }

    fn on_command(&mut self, name: &str, arguments: Value, ctx: &mut Context) -> Result<Value, String> {
        let ids = Self::ids(&arguments)?;
        match name {
            "notes.read" => {
                let notes = lock(&self.notes);
                let entities: Vec<Value> = ids
                    .iter()
                    .map(|id| json!({ "id": id.to_string(), "fields": notes.fields(id) }))
                    .collect();
                Ok(json!({ "entities": entities }))
            }
            "notes.write" => {
                let field = arguments["field"].as_str().ok_or("'field' names a field")?;
                let change = arguments["change"].as_str().ok_or("'change' is a value")?;
                let (before, after) = {
                    let notes = lock(&self.notes);
                    (notes.by.clone(), notes.written(&ids, field, change)?.by)
                };
                if before != after {
                    let label = match field {
                        "colour" => "Colour the notes",
                        _ => "Write the notes",
                    };
                    ctx.execute(SetNotes {
                        label: label.to_owned(),
                        before,
                        after,
                    });
                }
                Ok(json!({ "changed": ids.len(), "refused": [] }))
            }
            _ => Err(format!("'{name}' is not handled")),
        }
    }

    fn project_section(&self) -> Option<Value> {
        Some(lock(&self.notes).section())
    }

    fn open_project(&mut self, section: Option<Value>, _ctx: &mut Context) {
        let opened = Notes::of_section(&section.unwrap_or_default());
        self.set(opened.by);
    }
}

uniwow_api::export_module!(NotesModule::default());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_written_coloured_taken_away_and_kept_in_the_project() {
        let (tree, lamp) = (EntityId::doodad(0, 1), EntityId::doodad(0, 2));
        let notes = Notes::default();
        let noted = notes.written(&[tree, lamp], "note", "to move").unwrap();
        let coloured = noted.written(&[tree], "colour", "#ff8000").unwrap();
        assert_eq!(
            coloured.fields(&tree),
            json!({ "note": "to move", "colour": [1.0, 128.0_f32 / 255.0, 0.0] })
        );
        assert_eq!(
            coloured.fields(&EntityId::doodad(0, 3)),
            json!({ "note": "", "colour": null })
        );
        assert!(coloured.written(&[tree], "colour", "orange").is_err());
        assert!(coloured.written(&[tree], "size", "2").is_err());
        let opened = Notes::of_section(&coloured.section());
        assert_eq!(opened.by, coloured.by);
        let gone = coloured.written(&[lamp], "note", "").unwrap();
        assert!(!gone.by.contains_key(&lamp), "no text and no colour: no note");
        let uncoloured = gone.written(&[tree], "colour", "").unwrap();
        assert_eq!(uncoloured.by[&tree].colour, None);
        assert_eq!(colour_text([1.0, 0.5, 0.0]), "#ff8000");
        let declared = uniwow_api::scene::components::Component::of_json(MODULE, &declaration()["components"][0]);
        assert!(declared.unwrap().fits([uniwow_api::scene::Kind::Creature]));
    }
}
