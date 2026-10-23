//! The description being edited: its JSON as the file holds it (keys in their order), what was
//! last saved, and where. Each change is a command of the history (undo, redo); the model is made
//! again from it after each.

use std::any::Any;
use std::path::PathBuf;

use uniwow_api::Command;
use uniwow_api::vfx::json::{self, Layout, Value};

/// A part of the description.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Node {
    /// The model itself: name, duration, radius, phases.
    #[default]
    Effect,
    Texture(String),
    Bone(String),
    Emitter(usize),
    Ribbon(usize),
    Mesh(usize),
    Light(usize),
}

impl Node {
    /// The list of the description a part of it is in, and its place in it.
    pub fn list(&self) -> Option<(&'static str, usize)> {
        match self {
            Node::Emitter(i) => Some(("emitters", *i)),
            Node::Ribbon(i) => Some(("ribbons", *i)),
            Node::Mesh(i) => Some(("meshes", *i)),
            Node::Light(i) => Some(("lights", *i)),
            _ => None,
        }
    }

    pub fn map(&self) -> Option<(&'static str, &str)> {
        match self {
            Node::Texture(k) => Some(("textures", k)),
            Node::Bone(k) => Some(("bones", k)),
            _ => None,
        }
    }

    /// Its part of `root`.
    pub fn get<'a>(&self, root: &'a Value) -> Option<&'a Value> {
        if let Some((list, i)) = self.list() {
            return root.get(list)?.as_list()?.get(i);
        }
        if let Some((map, key)) = self.map() {
            return root.get(map)?.get(key);
        }
        Some(root)
    }

    /// Its part of `root`, to change.
    pub fn get_mut<'a>(&self, root: &'a mut Value) -> Option<&'a mut Value> {
        if let Some((list, i)) = self.list() {
            return match root.get_mut(list)? {
                Value::List(items) => items.get_mut(i),
                _ => None,
            };
        }
        if let Some((map, key)) = self.map() {
            return root.get_mut(map)?.get_mut(key);
        }
        Some(root)
    }

    /// The part a message of forge names ("emitter 3", "mesh 0", "bone orb", "texture glow").
    pub fn named_in(text: &str) -> Option<Node> {
        let words: Vec<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .collect();
        for pair in words.windows(2) {
            let index = pair[1].parse::<usize>();
            match (pair[0], index) {
                ("emitter", Ok(i)) => return Some(Node::Emitter(i)),
                ("ribbon", Ok(i)) => return Some(Node::Ribbon(i)),
                ("mesh", Ok(i)) => return Some(Node::Mesh(i)),
                ("bone", _) => return Some(Node::Bone(pair[1].to_owned())),
                ("texture", _) => return Some(Node::Texture(pair[1].to_owned())),
                _ => {}
            }
        }
        None
    }
}

/// The description edited.
pub struct Doc {
    pub path: Option<PathBuf>,
    pub value: Value,
    /// As last saved (or opened): what differs from it is shown changed.
    pub saved: Value,
    pub layout: Layout,
    /// Changed since the model was last made from it.
    pub stale: bool,
}

impl Doc {
    pub fn new(path: Option<PathBuf>, value: Value, layout: Layout) -> Self {
        Self {
            path,
            saved: value.clone(),
            value,
            layout,
            stale: true,
        }
    }

    pub fn unsaved(&self) -> bool {
        self.value != self.saved
    }

    /// The file's text, as Python's json.dump(indent=1) writes it.
    pub fn text(&self) -> String {
        json::write(&self.value, self.layout)
    }

    pub fn save(&mut self, path: PathBuf) -> Result<(), String> {
        std::fs::write(&path, self.text()).map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path);
        self.saved = self.value.clone();
        Ok(())
    }

    /// Whether `key` of the part `node` differs from what was saved.
    pub fn changed(&self, node: &Node, key: &str) -> bool {
        let now = node.get(&self.value).and_then(|v| v.get(key));
        let then = node.get(&self.saved).and_then(|v| v.get(key));
        now != then
    }

    /// Whether the part `node` differs from what was saved.
    pub fn node_changed(&self, node: &Node) -> bool {
        node.get(&self.value) != node.get(&self.saved)
    }
}

/// A change of the description: its whole JSON before and after (small files: a few ten kB).
pub struct Edit {
    pub label: String,
    pub before: Value,
    pub after: Value,
}

fn doc(module: &mut dyn Any) -> Option<&mut Doc> {
    crate::module(module).doc.as_mut()
}

impl Command for Edit {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, module: &mut dyn Any) {
        if let Some(doc) = doc(module) {
            doc.value = self.after.clone();
            doc.stale = true;
        }
    }

    fn revert(&mut self, module: &mut dyn Any) {
        if let Some(doc) = doc(module) {
            doc.value = self.before.clone();
            doc.stale = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parts_forge_names_are_found() {
        assert_eq!(
            Node::named_in("emitter 3: about 900 particles alive"),
            Some(Node::Emitter(3))
        );
        assert_eq!(Node::named_in("ribbon 0: 80 edges alive"), Some(Node::Ribbon(0)));
        assert_eq!(
            Node::named_in("emitter 1 (billboard): lowest point"),
            Some(Node::Emitter(1))
        );
        assert_eq!(Node::named_in("bone orb not described"), Some(Node::Bone("orb".into())));
        assert_eq!(
            Node::named_in("texture glow: size 3"),
            Some(Node::Texture("glow".into()))
        );
        assert_eq!(Node::named_in("about 2100 particles alive in the model"), None);
    }

    #[test]
    fn a_part_is_read_and_changed_in_place() {
        let mut root = json::parse(r#"{"name": "a", "emitters": [{"rate": 2}], "bones": {"o": {"spin": 1}}}"#).unwrap();
        assert_eq!(Node::Emitter(0).get(&root).unwrap().get("rate"), Some(&Value::Int(2)));
        Node::Bone("o".into())
            .get_mut(&mut root)
            .unwrap()
            .set("spin", Value::Float(0.5));
        assert_eq!(
            root.get("bones").unwrap().get("o").unwrap().get("spin"),
            Some(&Value::Float(0.5))
        );
        let doc = Doc::new(None, root.clone(), Layout::default());
        assert!(!doc.changed(&Node::Bone("o".into()), "spin"));
    }
}
