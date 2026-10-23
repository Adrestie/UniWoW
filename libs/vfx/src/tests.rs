//! A description through the whole chain, and against WoW-mods' own tools.

use crate::forge::{self, Options};
use crate::json::{self, Layout, Value};
use crate::{blp, m2, validate};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every part of the description at once: bones that orbit, bob and spin, bursts and windows,
/// flipbooks, ribbons, every mesh shape, a projected plane, a scrolled noise.
const EVERYTHING: &str = r##"{
 "name": "vfx_everything",
 "duration": 1500,
 "radius": 4,
 "textures": {
  "spark": {"kind": "spark"},
  "smoke": {"kind": "smoke", "size": 128, "rows": 2, "cols": 2},
  "noise": {"kind": "noise", "size": 64},
  "ring": {"kind": "ring", "color": "#ff8000"},
  "trail": {"kind": "trail"},
  "old": {"game": "Spells\\Glow_256.blp"}
 },
 "bones": {
  "spin": {"pos": [0, 0, 0.5], "spin": 0.7, "rotation": [10, 20, 30]},
  "orb": {"parent": "spin", "pos": [0, 0, 1], "orbit": {"radius": 1.5, "speed": 1.2, "phase": 0.25}, "bob": {"height": 0.2, "speed": 2}}
 },
 "emitters": [
  {"texture": "spark", "shape": "sphere", "radius": 1, "radius_min": 0.5, "bursts": [{"at": 0.1, "count": 40}, {"at": 0.6, "count": 20, "window": 200}], "life": 0.9, "speed": [[0, 2], [1, 4]], "colors": "#80c0ff", "flags": ["world_space", 16]},
  {"texture": "smoke", "shape": "plane", "area": [1, 2], "rate": 12, "until": 0.5, "life": 2, "flipbook": true, "size": [[0, [0.5, 0.7]], [1, 1.5]], "base_flags": "0x29", "bone": "orb", "blend": "alpha"},
  {"texture": "old", "rate": [[0, 5], [0.5, 30], [1, 5]], "life": 0.5, "gravity": 1, "spread": 120, "pos": [0, 0, 0.05]}
 ],
 "ribbons": [
  {"texture": "trail", "bone": "orb", "color": [[0, "#ffffff"], [1, "#ff0000"]], "alpha": [[0, 1], [1, 0.5]], "edges": 60, "life": 0.5}
 ],
 "meshes": [
  {"shape": "ring", "texture": "ring", "inner": 0.5, "outer": 1, "segments": 16, "spin": -0.3},
  {"shape": "disc", "texture": "ring", "radius": 2, "rim": 0.1, "segments": 12, "alpha": 0.5},
  {"shape": "plane", "texture": "ring", "width": 3, "length": 3, "projected": true, "scale": [[0, 0.5], [1, 1.5]]},
  {"shape": "sphere", "texture": "noise", "radius": 0.5, "segments": 10, "uv_scroll": [0.3, 0.1], "uv_tile": [2, 1], "blend": "alpha"},
  {"shape": "cone", "texture": "noise", "radius": 1, "height": 2, "segments": 8, "bone": "spin", "color": "#00ff00"},
  {"shape": "cylinder", "texture": "noise", "radius": 0.5, "height": 3, "segments": 8, "priority": 2}
 ]
}"##;

#[test]
fn a_description_makes_a_model_the_checks_take() {
    let spec = json::parse(EVERYTHING).unwrap();
    let forged = forge::forge(&spec, &Options::default()).unwrap();
    assert!(validate::validate(&forged.model, Some(&forged.skin)).is_empty());
    assert_eq!(m2::read(&forged.m2).unwrap(), forged.model);
    assert_eq!(m2::read_skin(&forged.skin_file).unwrap(), forged.skin);
    assert_eq!(forged.model.particles.len(), 3);
    assert_eq!(forged.model.ribbons.len(), 1);
    assert_eq!(forged.skin.submeshes.len(), 6);
    assert_eq!(forged.skin.batches[2].flags, forge::PROJECTED_BATCH);
    // Five textures drawn here (the game's own one is named only), each a BLP that reads back.
    assert_eq!(forged.textures.len(), 5);
    for texture in &forged.textures {
        let back = blp::decode(&texture.blp).unwrap();
        assert_eq!((back.width, back.height), (texture.image.width, texture.image.height));
    }
    assert!(
        forged.notes.iter().any(|n| n.contains("thrown downwards")),
        "{:?}",
        forged.notes
    );
}

#[test]
fn the_limits_of_the_client_refuse_a_description() {
    let mut spec = json::parse(EVERYTHING).unwrap();
    let emitters = spec.get_mut("emitters").unwrap();
    if let Value::List(list) = emitters {
        list[2].set("rate", Value::Int(2000));
    }
    let refused = forge::forge(&spec, &Options::default()).err().unwrap();
    assert!(refused.0.contains("particles alive"), "{refused}");
}

#[test]
fn a_mesh_of_a_gltf_file_is_turned_to_z_up() {
    // one triangle, its buffer inside the file: positions (0,0,0), (1,0,0), (0,1,0) in glTF's axes
    let mut buffer = Vec::new();
    for v in [0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        buffer.extend_from_slice(&v.to_le_bytes());
    }
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in buffer.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (k, b)| n | u32::from(*b) << (16 - 8 * k));
        for k in 0..=chunk.len() {
            encoded.push(TABLE[(n >> (18 - 6 * k) & 63) as usize] as char);
        }
    }
    let gltf = format!(
        r#"{{"asset": {{"version": "2.0"}}, "buffers": [{{"byteLength": 36, "uri": "data:application/octet-stream;base64,{encoded}"}}],
        "bufferViews": [{{"buffer": 0, "byteLength": 36}}],
        "accessors": [{{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}}],
        "meshes": [{{"name": "leaf", "primitives": [{{"attributes": {{"POSITION": 0}}}}]}}]}}"#
    );
    let dir = std::env::temp_dir().join(format!("uniwow-vfx-gltf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("leaf.gltf"), gltf).unwrap();
    let spec = json::parse(r#"{"shape": "gltf", "file": "leaf.gltf", "mesh": "leaf", "size": 2}"#).unwrap();
    let shape = forge::mesh_shape(&spec, &dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        shape.positions,
        vec![[0.0, -0.0, 0.0], [2.0, -0.0, 0.0], [0.0, -0.0, 2.0]]
    );
    assert_eq!(shape.triangles, vec![[0, 1, 2]]);
    assert_eq!(shape.normals[0], [0.0, -0.0, 1.0]);
}

/// WoW-mods' tools, when they are on this machine.
fn reference() -> Option<PathBuf> {
    let dir = std::env::var_os("UNIWOW_VFX_TOOLS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\WoW-mods\tools\vfx"));
    dir.join("forge.py").exists().then_some(dir)
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or((a.len() != b.len()).then(|| a.len().min(b.len())))
}

fn examples(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir.join("examples"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    out.sort();
    out
}

/// Each example of WoW-mods' tools: the model made here is forge.py's to the byte, and
/// validate.py takes it. Needs Python with numpy and Pillow, and the tools (`UNIWOW_VFX_TOOLS`, by
/// default E:\WoW-mods\tools\vfx): `cargo test -p uniwow-vfx -- --ignored`.
#[test]
#[ignore = "needs Python and WoW-mods' tools"]
fn the_examples_are_forge_py_s_models() {
    let dir = reference().expect("WoW-mods' tools/vfx");
    let m2port = dir.join(r"..\..\mod-limit-break\tools\m2port");
    let work = std::env::temp_dir().join(format!("uniwow-vfx-{}", std::process::id()));
    let mut compared = 0;
    for example in examples(&dir) {
        let text = std::fs::read_to_string(&example).unwrap();
        let spec = json::parse(&text).unwrap();
        // the file written again is the same file
        assert_eq!(json::write(&spec, Layout::of(&text)), text, "{}", example.display());
        if !spec.has("name") || spec.has("effects") {
            continue; // a sheet, not a model
        }
        let name = spec.get("name").unwrap().as_str().unwrap().to_owned();
        let python = work.join("python");
        let output = Command::new("python")
            .arg(dir.join("forge.py"))
            .arg(&example)
            .arg(&python)
            .output()
            .unwrap();
        assert!(output.status.success(), "forge.py on {}", example.display());
        let options = Options {
            dir: example.parent().unwrap().to_owned(),
            ..Options::default()
        };
        let forged = forge::forge(&spec, &options).unwrap();
        let notes: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("note: ").map(str::to_owned))
            .collect();
        assert_eq!(forged.notes, notes, "{name}: notes");
        for (file, ours) in [
            (format!("{name}.m2"), &forged.m2),
            (format!("{name}00.skin"), &forged.skin_file),
        ] {
            let theirs = std::fs::read(python.join(&file)).unwrap();
            assert!(
                first_difference(ours, &theirs).is_none(),
                "{file}: differs from forge.py's at byte {:?} ({} and {} bytes)",
                first_difference(ours, &theirs),
                ours.len(),
                theirs.len()
            );
        }
        // the same textures, by name
        let mut ours: Vec<&str> = forged.textures.iter().map(|t| t.file_name.as_str()).collect();
        ours.sort();
        let mut theirs: Vec<String> = std::fs::read_dir(&python)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("{name}_")) && n.ends_with(".blp"))
            .collect();
        theirs.sort();
        assert_eq!(ours, theirs, "{name}: textures");
        // the analytic shapes are texgen's to one level (its palette files are exact), the random
        // ones only of the same size
        for texture in &forged.textures {
            let theirs = blp::decode(&std::fs::read(python.join(&texture.file_name)).unwrap()).unwrap();
            assert_eq!(
                (theirs.width, theirs.height),
                (texture.image.width, texture.image.height)
            );
            let kind = spec
                .get("textures")
                .and_then(|t| t.get(&texture.key))
                .and_then(|t| t.get("kind"));
            let analytic = !matches!(
                kind.and_then(Value::as_str),
                Some("noise" | "smoke" | "rune" | "lightning" | "petal")
            );
            let theirs_file = std::fs::read(python.join(&texture.file_name)).unwrap();
            if analytic && theirs_file[8] == 1 {
                let worst = texture
                    .image
                    .rgba
                    .iter()
                    .zip(&theirs.rgba)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(worst <= 1, "{}: {worst} levels from texgen's", texture.file_name);
            }
        }
        // validate.py takes the model written here
        let rust = work.join("rust");
        std::fs::create_dir_all(&rust).unwrap();
        std::fs::write(rust.join(format!("{name}.m2")), &forged.m2).unwrap();
        std::fs::write(rust.join(format!("{name}00.skin")), &forged.skin_file).unwrap();
        let status = Command::new("python")
            .current_dir(&m2port)
            .arg("validate.py")
            .arg(rust.join(format!("{name}.m2")))
            .status()
            .unwrap();
        assert!(status.success(), "validate.py refuses {name}");
        compared += 1;
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(compared >= 4, "{compared} models compared");
}
