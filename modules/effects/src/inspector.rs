//! The parts of the description in a tree, and the inspector of the one chosen: each key with its
//! tooltip, its default when the description leaves it out, a reset to that default, its value
//! highlighted while it differs from what was saved. Values over time take a value, keys over the
//! loop or keys by phase; values over a particle's life up to 8 keys. What has no editor of its own
//! is edited as its JSON.

use std::collections::HashMap;

use uniwow_api::egui::{self, RichText, Ui};
use uniwow_api::scene::state_colour;
use uniwow_api::vfx::json::{self, Value, float_repr};

use crate::doc::{Doc, Node};
use crate::schema::{self, Field, Kind};

/// What the inspector keeps between frames: the texts of the keys edited as JSON, by part and key.
#[derive(Default)]
pub struct Inspector {
    texts: HashMap<(String, String), String>,
    /// A drag going on: its edits make one entry of the history.
    pub dragging: bool,
}

/// A change the panel asks for: the description's new JSON and the label of its entry.
pub struct Change {
    pub label: String,
    pub value: Value,
    /// The part to choose after it.
    pub select: Option<Node>,
}

fn names(root: &Value, key: &str) -> Vec<String> {
    root.get(key)
        .and_then(Value::as_map)
        .map(|m| m.iter().map(|(k, _)| k.clone()).collect())
        .unwrap_or_default()
}

fn count(root: &Value, key: &str) -> usize {
    root.get(key).and_then(Value::as_list).map_or(0, <[Value]>::len)
}

fn label_of(root: &Value, node: &Node) -> String {
    let get = |list: &str, i: usize, what: &str| {
        let part = root.get(list).and_then(Value::as_list).and_then(|l| l.get(i));
        let hint = part
            .and_then(|p| p.get("texture").or_else(|| p.get("shape")))
            .and_then(Value::as_str)
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        format!("{what} {i}{hint}")
    };
    match node {
        Node::Effect => root.get("name").and_then(Value::as_str).unwrap_or("effect").to_owned(),
        Node::Texture(k) => format!("texture {k}"),
        Node::Bone(k) => format!("bone {k}"),
        Node::Emitter(i) => get("emitters", *i, "emitter"),
        Node::Ribbon(i) => get("ribbons", *i, "ribbon"),
        Node::Mesh(i) => get("meshes", *i, "mesh"),
        Node::Light(i) => get("lights", *i, "light"),
    }
}

/// The tree of the parts: chooses `selected`; adds, copies and removes parts.
pub fn tree(ui: &mut Ui, doc: &Doc, selected: &mut Node) -> Option<Change> {
    let root = &doc.value;
    let mut change = None;
    let item = |ui: &mut Ui, node: Node, selected: &mut Node| {
        let text = RichText::new(label_of(root, &node));
        let text = if doc.node_changed(&node) {
            text.background_color(state_colour::highlight())
        } else {
            text
        };
        if ui.selectable_label(*selected == node, text).clicked() {
            *selected = node;
        }
    };
    item(ui, Node::Effect, selected);
    let groups: [(&str, &str, Vec<Node>); 6] = [
        (
            "Textures",
            "textures",
            names(root, "textures").into_iter().map(Node::Texture).collect(),
        ),
        (
            "Bones",
            "bones",
            names(root, "bones").into_iter().map(Node::Bone).collect(),
        ),
        (
            "Emitters",
            "emitters",
            (0..count(root, "emitters")).map(Node::Emitter).collect(),
        ),
        (
            "Ribbons",
            "ribbons",
            (0..count(root, "ribbons")).map(Node::Ribbon).collect(),
        ),
        ("Meshes", "meshes", (0..count(root, "meshes")).map(Node::Mesh).collect()),
        (
            "Lights",
            "lights",
            (0..count(root, "lights")).map(Node::Light).collect(),
        ),
    ];
    for (title, key, nodes) in groups {
        egui::CollapsingHeader::new(format!("{title} ({})", nodes.len()))
            .id_salt(("effects tree", key))
            .default_open(nodes.len() <= 12)
            .show(ui, |ui| {
                for node in nodes {
                    item(ui, node, selected);
                }
                if ui
                    .small_button("+")
                    .on_hover_text(format!("Adds one to the {key}"))
                    .clicked()
                {
                    change = add(root, key);
                }
            });
    }
    ui.horizontal(|ui| {
        let removable = *selected != Node::Effect;
        if ui.add_enabled(removable, egui::Button::new("Copy")).clicked() {
            change = copy(root, selected);
        }
        if ui.add_enabled(removable, egui::Button::new("Remove")).clicked() {
            change = remove(root, selected);
        }
    });
    change
}

fn first_texture(root: &Value) -> Value {
    names(root, "textures")
        .first()
        .map_or(Value::from("glow"), |k| Value::from(k.as_str()))
}

fn fresh_name(root: &Value, map: &str, stem: &str) -> String {
    let taken = names(root, map);
    (1..)
        .map(|n| format!("{stem}{n}"))
        .find(|n| !taken.contains(n))
        .expect("a free name")
}

fn add(root: &Value, key: &str) -> Option<Change> {
    let mut value = root.clone();
    let parse = |text: &str| json::parse(text).expect("a part written here");

    let select = match key {
        "textures" | "bones" => {
            let (stem, part) = if key == "textures" {
                ("tex", parse(r#"{"kind": "glow"}"#))
            } else {
                ("bone", parse(r#"{"pos": [0, 0, 0]}"#))
            };
            let name = fresh_name(root, key, stem);
            if value.get(key).is_none() {
                value.set(key, Value::Map(Vec::new()));
            }
            value.get_mut(key)?.set(&name, part);
            if key == "textures" {
                Node::Texture(name)
            } else {
                Node::Bone(name)
            }
        }
        _ => {
            let mut part = match key {
                "emitters" => parse(r#"{"rate": 20, "life": 1.0}"#),
                "ribbons" => parse(r#"{"edges": 30, "life": 0.5}"#),
                "meshes" => parse(r#"{"shape": "plane"}"#),
                _ => parse(r##"{"color": "#ffffff"}"##),
            };
            if key != "lights" {
                let mut entries = vec![("texture".to_owned(), first_texture(root))];
                entries.extend(part.as_map().unwrap_or_default().iter().cloned());
                part = Value::Map(entries);
            }
            if value.get(key).is_none() {
                value.set(key, Value::List(Vec::new()));
            }
            let Value::List(items) = value.get_mut(key)? else {
                return None;
            };
            items.push(part);
            let i = items.len() - 1;
            match key {
                "emitters" => Node::Emitter(i),
                "ribbons" => Node::Ribbon(i),
                "meshes" => Node::Mesh(i),
                _ => Node::Light(i),
            }
        }
    };
    Some(Change {
        label: format!("effect: {} added", label_of(&value, &select)),
        value,
        select: Some(select),
    })
}

fn copy(root: &Value, node: &Node) -> Option<Change> {
    let mut value = root.clone();
    let part = node.get(root)?.clone();
    let select = if let Some((list, _)) = node.list() {
        let Value::List(items) = value.get_mut(list)? else {
            return None;
        };
        items.push(part);
        let i = items.len() - 1;
        match node {
            Node::Emitter(_) => Node::Emitter(i),
            Node::Ribbon(_) => Node::Ribbon(i),
            Node::Mesh(_) => Node::Mesh(i),
            _ => Node::Light(i),
        }
    } else {
        let (map, key) = node.map()?;
        let name = fresh_name(root, map, &format!("{key}_"));
        value.get_mut(map)?.set(&name, part);
        if map == "textures" {
            Node::Texture(name)
        } else {
            Node::Bone(name)
        }
    };
    Some(Change {
        label: format!("effect: {} copied", label_of(root, node)),
        value,
        select: Some(select),
    })
}

fn remove(root: &Value, node: &Node) -> Option<Change> {
    let mut value = root.clone();
    if let Some((list, i)) = node.list() {
        let Value::List(items) = value.get_mut(list)? else {
            return None;
        };
        items.remove(i);
    } else {
        let (map, key) = node.map()?;
        value.get_mut(map)?.remove(key);
    }
    Some(Change {
        label: format!("effect: {} removed", label_of(root, node)),
        value,
        select: Some(Node::Effect),
    })
}

/// The fields of a part, as its kind and shape choose them.
fn fields(node: &Node, part: &Value) -> Vec<Field> {
    let all: &[Field] = match node {
        Node::Effect => schema::EFFECT,
        Node::Texture(_) => schema::TEXTURE,
        Node::Bone(_) => schema::BONE,
        Node::Emitter(_) => schema::EMITTER,
        Node::Ribbon(_) => schema::RIBBON,
        Node::Mesh(_) => schema::MESH,
        Node::Light(_) => schema::LIGHT,
    };
    let default_shape = if matches!(node, Node::Mesh(_)) {
        "plane"
    } else {
        "point"
    };
    let shape = part.get("shape").and_then(Value::as_str).unwrap_or(default_shape);
    all.iter()
        .filter(|f| f.shapes.is_empty() || f.shapes.contains(&shape))
        .copied()
        .collect()
}

/// The inspector of `node`: its new JSON when the user changed it.
pub fn inspector(ui: &mut Ui, state: &mut Inspector, doc: &Doc, node: &Node) -> Option<Change> {
    let root = &doc.value;
    let Some(part) = node.get(root) else {
        ui.weak("This part is no longer in the description.");
        return None;
    };
    let mut work = part.clone();
    let textures = names(root, "textures");
    let mut bones = vec!["root".to_owned()];
    bones.extend(names(root, "bones"));
    let phases: Vec<String> = root
        .get("sequences")
        .and_then(Value::as_map)
        .map(|m| m.iter().map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    let context = Context {
        textures: &textures,
        bones: &bones,
        phases: &phases,
    };
    let node_id = format!("{node:?}");
    ui.heading(label_of(root, node));
    egui::Grid::new(("effects inspector", &node_id))
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let mut list: Vec<Field> = fields(node, &work);
            // a texture's own parameters, from its kind
            if matches!(node, Node::Texture(_)) {
                let kind = work.get("kind").and_then(Value::as_str).unwrap_or("glow").to_owned();
                for (key, default) in schema::texture_parameters(&kind) {
                    let (kind, default) = match default {
                        uniwow_api::vfx::texgen::Param::Num(v) => (
                            Kind::Num {
                                min: 0.0,
                                max: 4096.0,
                                speed: if v.abs() >= 8.0 { 1.0 } else { 0.01 },
                            },
                            leak(float_repr(v)),
                        ),
                        uniwow_api::vfx::texgen::Param::Color(c) => (Kind::Color, leak(format!("\"{c}\""))),
                    };
                    list.push(Field {
                        key: leak(key.to_owned()),
                        label: leak(key.to_owned()),
                        tip: "A parameter of this kind of texture (texgen).",
                        kind,
                        default,
                        shapes: &[],
                    });
                }
            }
            for field in list {
                let changed = doc.changed(node, field.key);
                let behind = ui.painter().add(egui::Shape::Noop);
                let label = ui.label(field.label).on_hover_text(field.tip);
                let given = work.get(field.key).is_some();
                let response = ui.horizontal(|ui| {
                    let mut value = work
                        .get(field.key)
                        .cloned()
                        .unwrap_or_else(|| json::parse(field.default).unwrap_or(Value::Null));
                    let key = (node_id.clone(), field.key.to_owned());
                    let edited = editor(ui, state, &key, &field, &mut value, &context, given);
                    // forge needs these: no default to go back to
                    let required = matches!(field.key, "name" | "texture")
                        || (field.key == "shape" && matches!(node, Node::Mesh(_)));
                    if given && !required && ui.small_button("↺").on_hover_text("Back to forge's default").clicked() {
                        work.remove(field.key);
                    } else if edited {
                        work.set(field.key, value);
                    }
                });
                if changed {
                    let rect = label.rect.union(response.response.rect).expand(1.0);
                    ui.painter()
                        .set(behind, egui::Shape::rect_filled(rect, 2.0, state_colour::highlight()));
                }
                ui.end_row();
            }
        });
    (work != *part).then(|| {
        let mut value = root.clone();
        if let Some(slot) = node.get_mut(&mut value) {
            *slot = work;
        }
        Change {
            label: format!("effect: {} changed", label_of(root, node)),
            value,
            select: None,
        }
    })
}

/// A text kept for the life of the program: the labels of the texture kinds' parameters, few.
fn leak(text: String) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static KEPT: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = kept.get(&text) {
        return found;
    }
    let leaked: &'static str = Box::leak(text.clone().into_boxed_str());
    kept.insert(text, leaked);
    leaked
}

struct Context<'a> {
    textures: &'a [String],
    bones: &'a [String],
    phases: &'a [String],
}

/// A number written back as it was: an integer stays one while it is whole.
fn number(old: &Value, v: f64, integer: bool) -> Value {
    if integer || (matches!(old, Value::Int(_)) && v.fract() == 0.0) {
        Value::Int(v.round() as i64)
    } else {
        Value::Float(v)
    }
}

fn drag(ui: &mut Ui, state: &mut Inspector, value: &mut Value, min: f64, max: f64, speed: f64, integer: bool) -> bool {
    let mut v = value.as_f64().unwrap_or(0.0);
    let response = ui.add(
        egui::DragValue::new(&mut v)
            .range(min..=max)
            .speed(speed)
            .max_decimals(if integer { 0 } else { 4 }),
    );
    if response.dragged() {
        state.dragging = true;
    }
    if response.changed() {
        *value = number(value, v, integer);
        return true;
    }
    false
}

fn color(ui: &mut Ui, value: &mut Value) -> bool {
    let rgb = match value {
        Value::Str(text) => uniwow_api::vfx::texgen::parse_color(text).unwrap_or([255; 3]),
        Value::List(items) if items.len() == 3 => {
            std::array::from_fn(|k| items[k].as_f64().unwrap_or(255.0).clamp(0.0, 255.0) as u8)
        }
        _ => [255; 3],
    };
    let mut edited = rgb;
    if egui::color_picker::color_edit_button_srgb(ui, &mut edited).changed() && edited != rgb {
        *value = Value::Str(format!("#{:02x}{:02x}{:02x}", edited[0], edited[1], edited[2]));
        return true;
    }
    false
}

fn numbers(ui: &mut Ui, state: &mut Inspector, value: &mut Value, n: usize, speed: f64) -> bool {
    let mut items: Vec<Value> = value.as_list().map(<[Value]>::to_vec).unwrap_or_default();
    items.resize(n, Value::Int(0));
    let mut changed = false;
    for item in &mut items {
        changed |= drag(ui, state, item, -1e6, 1e6, speed, false);
    }
    if changed {
        *value = Value::List(items);
    }
    changed
}

/// The editor of one value of a key, by its kind.
fn editor(
    ui: &mut Ui,
    state: &mut Inspector,
    key: &(String, String),
    field: &Field,
    value: &mut Value,
    context: &Context,
    given: bool,
) -> bool {
    if !given {
        ui.visuals_mut().override_text_color = Some(ui.visuals().weak_text_color());
    }
    match field.kind {
        Kind::Num { min, max, speed } => drag(ui, state, value, min, max, speed, false),
        Kind::Int { min, max } => drag(ui, state, value, min as f64, max as f64, 0.2, true),
        Kind::Text => {
            let mut text = value.as_str().unwrap_or_default().to_owned();
            let response = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0));
            if response.changed() {
                *value = Value::Str(text);
                return true;
            }
            false
        }
        Kind::Bool => {
            let mut on = value.truthy();
            if ui.checkbox(&mut on, "").changed() {
                *value = Value::Bool(on);
                return true;
            }
            false
        }
        Kind::Color => color(ui, value),
        Kind::Vec3 => numbers(ui, state, value, 3, 0.01),
        Kind::Pair => numbers(ui, state, value, 2, 0.01),
        Kind::Choice(options) => choice(ui, key, value, options.iter().map(|o| (*o).to_owned()).collect()),
        Kind::Texture => choice(ui, key, value, context.textures.to_vec()),
        Kind::Bone => choice(ui, key, value, context.bones.to_vec()),
        Kind::Flags(table) => flags(ui, value, table),
        Kind::Animated => animated(ui, state, key, value, context, false),
        Kind::AnimatedColor => animated(ui, state, key, value, context, true),
        Kind::Life => life(ui, state, value, false),
        Kind::LifeColor => life(ui, state, value, true),
        Kind::Rotation => match value {
            Value::Map(_) => raw(ui, state, key, value),
            _ => numbers(ui, state, value, 3, 0.5),
        },
        Kind::Json => raw(ui, state, key, value),
    }
}

fn choice(ui: &mut Ui, key: &(String, String), value: &mut Value, options: Vec<String>) -> bool {
    let current = value.as_str().unwrap_or_default().to_owned();
    let mut chosen = current.clone();
    egui::ComboBox::from_id_salt(("effects choice", key))
        .selected_text(&current)
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(&mut chosen, option.clone(), option);
            }
        });
    if chosen != current {
        *value = Value::Str(chosen);
        return true;
    }
    false
}

fn flags(ui: &mut Ui, value: &mut Value, table: &[(&str, u32)]) -> bool {
    let items: Vec<Value> = value.as_list().map(<[Value]>::to_vec).unwrap_or_default();
    let mut changed = false;
    let mut kept = items.clone();
    ui.vertical(|ui| {
        for (name, bits) in table {
            let mut on = items.iter().any(|v| v.as_str() == Some(name));
            if ui
                .checkbox(&mut on, *name)
                .on_hover_text(format!("{bits:#x}"))
                .changed()
            {
                changed = true;
                if on {
                    kept.push(Value::from(*name));
                } else {
                    kept.retain(|v| v.as_str() != Some(name));
                }
            }
        }
        for other in items.iter().filter(|v| matches!(v, Value::Int(_))) {
            ui.weak(format!("and {:#x}", other.as_f64().unwrap_or(0.0) as u32));
        }
    });
    if changed {
        *value = Value::List(kept);
    }
    changed
}

/// A key's JSON, kept as typed until it reads as JSON.
fn raw(ui: &mut Ui, state: &mut Inspector, key: &(String, String), value: &mut Value) -> bool {
    let shown = json::write(value, json::Layout::default());
    let text = state.texts.entry(key.clone()).or_insert_with(|| shown.clone());
    let response = ui.add(
        egui::TextEdit::multiline(text)
            .code_editor()
            .desired_rows(1)
            .desired_width(220.0),
    );
    let parsed = json::parse(text);
    if parsed.is_err() {
        ui.colored_label(ui.visuals().error_fg_color, "✗")
            .on_hover_text("Not JSON yet");
    }
    if !response.has_focus() {
        // follows the value when it is not being typed (undo, another part)
        if parsed.as_ref().ok() != Some(value) {
            *text = shown;
        }
        return false;
    }
    match parsed {
        Ok(new) if new != *value => {
            *value = new;
            true
        }
        _ => false,
    }
}

fn scalar(ui: &mut Ui, state: &mut Inspector, value: &mut Value, is_color: bool) -> bool {
    if is_color {
        color(ui, value)
    } else {
        drag(ui, state, value, -1e6, 1e6, 0.01, false)
    }
}

/// Keys `[[t, v], ...]`: their times (0 to 1) and values; added after the last, removed.
fn keys(ui: &mut Ui, state: &mut Inspector, value: &mut Value, is_color: bool, most: usize) -> bool {
    let mut items: Vec<Value> = value.as_list().map(<[Value]>::to_vec).unwrap_or_default();
    let mut changed = false;
    let mut remove = None;
    ui.vertical(|ui| {
        for (index, item) in items.iter_mut().enumerate() {
            let Value::List(pair) = item else { continue };
            if pair.len() < 2 {
                continue;
            }
            ui.horizontal(|ui| {
                changed |= drag(ui, state, &mut pair[0], 0.0, 1.0, 0.005, false);
                changed |= match &mut pair[1] {
                    Value::List(_) if !is_color => numbers(ui, state, &mut pair[1], 2, 0.01),
                    v => scalar(ui, state, v, is_color),
                };
                if ui.small_button("−").on_hover_text("Removes this key").clicked() {
                    remove = Some(index);
                }
            });
        }
        if items.len() < most && ui.small_button("+ key").clicked() {
            let last = items
                .last()
                .cloned()
                .unwrap_or(Value::List(vec![Value::Int(0), Value::Int(0)]));
            let mut next = last.as_list().map(<[Value]>::to_vec).unwrap_or_default();
            if let Some(t) = next.first().and_then(Value::as_f64) {
                next[0] = Value::Float(((t + 1.0) / 2.0).min(1.0));
            }
            items.push(Value::List(next));
            changed = true;
        }
    });
    if let Some(index) = remove {
        items.remove(index);
        changed = true;
    }
    if changed {
        *value = Value::List(items);
    }
    changed
}

/// A value over the sequence: one value, keys over the loop, or keys by phase.
fn animated(
    ui: &mut Ui,
    state: &mut Inspector,
    key: &(String, String),
    value: &mut Value,
    context: &Context,
    is_color: bool,
) -> bool {
    let mut changed = false;
    ui.vertical(|ui| match value {
        Value::Map(_) => {
            ui.weak("by phase");
            let mut map = value.clone();
            for phase in context.phases {
                let Some(slot) = map.get_mut(phase) else {
                    ui.horizontal(|ui| {
                        ui.label(phase.as_str());
                        ui.weak("holds its neighbour's value");
                    });
                    continue;
                };
                ui.horizontal(|ui| {
                    ui.label(phase.as_str());
                    changed |= if slot.is_keys() {
                        keys(ui, state, slot, is_color, 64)
                    } else {
                        scalar(ui, state, slot, is_color)
                    };
                });
            }
            if changed {
                *value = map;
            }
            if ui.small_button("One value").clicked() {
                let first = value.as_map().and_then(|m| m.first()).map(|(_, v)| v.clone());
                let first = match first {
                    Some(v) if v.is_keys() => v.as_list().and_then(|l| l[0].as_list()).map(|p| p[1].clone()),
                    other => other,
                };
                *value = first.unwrap_or(Value::Int(0));
                changed = true;
            }
        }
        v if v.is_keys() => {
            changed |= keys(ui, state, v, is_color, 64);
            if ui.small_button("One value").clicked() {
                let first = v.as_list().and_then(|l| l[0].as_list()).map(|p| p[1].clone());
                *v = first.unwrap_or(Value::Int(0));
                changed = true;
            }
        }
        v => {
            ui.horizontal(|ui| {
                changed |= scalar(ui, state, v, is_color);
                if ui.small_button("Keys").on_hover_text("Keys over the loop").clicked() {
                    *v = Value::List(vec![
                        Value::List(vec![Value::Int(0), v.clone()]),
                        Value::List(vec![Value::Int(1), v.clone()]),
                    ]);
                    changed = true;
                }
                if !context.phases.is_empty()
                    && ui
                        .small_button("By phase")
                        .on_hover_text("A value or keys for each phase")
                        .clicked()
                {
                    *v = Value::Map(context.phases.iter().map(|p| (p.clone(), v.clone())).collect());
                    changed = true;
                }
            });
        }
    });
    let _ = key;
    changed
}

/// A value over a particle's life: one value or up to 8 keys.
fn life(ui: &mut Ui, state: &mut Inspector, value: &mut Value, is_color: bool) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        if value.is_keys() {
            changed |= keys(ui, state, value, is_color, 8);
        } else {
            ui.horizontal(|ui| {
                changed |= match value {
                    Value::List(_) if !is_color => numbers(ui, state, value, 2, 0.01),
                    _ => scalar(ui, state, value, is_color),
                };
                if ui
                    .small_button("Keys")
                    .on_hover_text("Keys over the particle's life")
                    .clicked()
                {
                    let v = value.clone();
                    *value = Value::List(vec![
                        Value::List(vec![Value::Int(0), v.clone()]),
                        Value::List(vec![Value::Int(1), v]),
                    ]);
                    changed = true;
                }
            });
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_are_added_copied_and_removed() {
        let root = json::parse(r#"{"name": "a", "textures": {"g": {"kind": "glow"}}, "emitters": [{"texture": "g"}]}"#)
            .unwrap();
        let added = add(&root, "emitters").unwrap();
        assert_eq!(added.select, Some(Node::Emitter(1)));
        assert_eq!(
            Node::Emitter(1).get(&added.value).unwrap().get("texture"),
            Some(&Value::from("g"))
        );
        let copied = copy(&root, &Node::Texture("g".into())).unwrap();
        assert_eq!(copied.select, Some(Node::Texture("g_1".into())));
        let removed = remove(&root, &Node::Emitter(0)).unwrap();
        assert_eq!(removed.value.get("emitters").unwrap().as_list().unwrap().len(), 0);
    }

    #[test]
    fn a_number_stays_an_integer_while_it_is_whole() {
        assert_eq!(number(&Value::Int(2), 3.0, false), Value::Int(3));
        assert_eq!(number(&Value::Int(2), 2.5, false), Value::Float(2.5));
        assert_eq!(number(&Value::Float(2.0), 3.0, false), Value::Float(3.0));
    }

    #[test]
    fn every_part_of_the_examples_has_its_fields() {
        // each key the examples give is one the inspector knows
        for text in [
            r#"{"texture": "g", "shape": "sphere", "radius": 1, "radius_min": 0.5, "bursts": [], "life": 1}"#,
            r#"{"shape": "petal", "texture": "p", "span": [0, 0.5], "bend": [], "bend_soft": 0.1, "cup": 0.1}"#,
        ] {
            let part = json::parse(text).unwrap();
            let node = if part.has("span") {
                Node::Mesh(0)
            } else {
                Node::Emitter(0)
            };
            let known: Vec<&str> = fields(&node, &part).iter().map(|f| f.key).collect();
            for (key, _) in part.as_map().unwrap() {
                assert!(known.contains(&key.as_str()), "{key} of {text}");
            }
        }
    }
}
