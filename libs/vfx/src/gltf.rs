//! The meshes of a glTF 2.0 file (.glb, or .gltf with its buffers beside it), as forge reads a mesh
//! modelled in Blender: positions, normals, the first set of texture coordinates, triangles;
//! glTF's Y up turned back to Blender's Z up.

use crate::json::{self, Value};
use std::path::Path;

pub struct Mesh {
    pub positions: Vec<[f64; 3]>,
    pub normals: Vec<[f64; 3]>,
    pub uvs: Vec<[f64; 2]>,
    pub triangles: Vec<[u32; 3]>,
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return Err("a data URI not in base64".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn index(v: &Value, key: &str) -> Result<usize, String> {
    match v.get(key) {
        Some(Value::Int(i)) if *i >= 0 => Ok(*i as usize),
        _ => Err(format!("{key}: an index expected")),
    }
}

fn field(v: &Value, key: &str, default: usize) -> usize {
    match v.get(key) {
        Some(Value::Int(i)) if *i >= 0 => *i as usize,
        _ => default,
    }
}

/// The values of an accessor, each as floats (or integers as floats).
fn accessor(doc: &Value, bins: &[Vec<u8>], at: usize) -> Result<Vec<Vec<f64>>, String> {
    let a = doc
        .get("accessors")
        .and_then(Value::as_list)
        .and_then(|l| l.get(at))
        .ok_or_else(|| format!("accessor {at} missing"))?;
    let view_at = index(a, "bufferView")?;
    let v = doc
        .get("bufferViews")
        .and_then(Value::as_list)
        .and_then(|l| l.get(view_at))
        .ok_or_else(|| format!("buffer view {view_at} missing"))?;
    let component = index(a, "componentType")?;
    let size = match component {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        other => return Err(format!("component type {other}")),
    };
    let k = match a.get("type").and_then(Value::as_str) {
        Some("SCALAR") => 1,
        Some("VEC2") => 2,
        Some("VEC3") => 3,
        Some("VEC4") => 4,
        other => return Err(format!("accessor type {other:?}")),
    };
    let stride = match field(v, "byteStride", 0) {
        0 => size * k,
        s => s,
    };
    let base = field(v, "byteOffset", 0) + field(a, "byteOffset", 0);
    let buf = bins.get(index(v, "buffer")?).ok_or("buffer missing")?;
    let count = index(a, "count")?;
    let mut out = Vec::with_capacity(count);
    for j in 0..count {
        let at = base + j * stride;
        let bytes = buf.get(at..at + size * k).ok_or("an accessor past its buffer")?;
        out.push(
            bytes
                .chunks_exact(size)
                .map(|b| match component {
                    5120 => f64::from(b[0] as i8),
                    5121 => f64::from(b[0]),
                    5122 => f64::from(i16::from_le_bytes([b[0], b[1]])),
                    5123 => f64::from(u16::from_le_bytes([b[0], b[1]])),
                    5125 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    _ => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                })
                .collect(),
        );
    }
    Ok(out)
}

/// The mesh named (or the first) of a glTF file, every primitive of it merged.
pub fn read(path: &Path, mesh_name: Option<&str>) -> Result<Mesh, String> {
    let shown = path.display();
    let raw = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let mut bins = Vec::new();
    let doc = if raw.starts_with(b"glTF") {
        // binary: the JSON chunk, then the BIN chunk
        let n = u32::from_le_bytes(raw.get(12..16).ok_or("a truncated .glb")?.try_into().expect("four")) as usize;
        let text = std::str::from_utf8(raw.get(20..20 + n).ok_or("a truncated .glb")?).map_err(|e| e.to_string())?;
        let doc = json::parse(text).map_err(|e| format!("{shown}: {e}"))?;
        let o = 20 + n;
        if raw.len() > o {
            let m = u32::from_le_bytes(raw.get(o..o + 4).ok_or("a truncated .glb")?.try_into().expect("four")) as usize;
            bins.push(raw.get(o + 8..o + 8 + m).ok_or("a truncated .glb")?.to_vec());
        }
        doc
    } else {
        let text = std::str::from_utf8(&raw).map_err(|e| e.to_string())?;
        json::parse(text).map_err(|e| format!("{shown}: {e}"))?
    };
    let buffers = doc.get("buffers").and_then(Value::as_list).unwrap_or_default();
    for b in buffers.iter().skip(bins.len()) {
        let uri = b.get("uri").and_then(Value::as_str).unwrap_or("");
        if let Some(data) = uri.strip_prefix("data:") {
            bins.push(base64(data.split_once(',').map_or("", |(_, d)| d))?);
        } else {
            let file = path.parent().unwrap_or(Path::new(".")).join(uri);
            bins.push(std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?);
        }
    }
    let meshes = doc.get("meshes").and_then(Value::as_list).unwrap_or_default();
    if meshes.is_empty() {
        return Err(format!("{shown}: no mesh"));
    }
    let mesh = match mesh_name {
        Some(name) => meshes
            .iter()
            .find(|m| m.get("name").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| {
                let names: Vec<&str> = meshes
                    .iter()
                    .map(|m| m.get("name").and_then(Value::as_str).unwrap_or("?"))
                    .collect();
                format!("{shown}: no mesh {name} (has {})", names.join(", "))
            })?,
        None => &meshes[0],
    };
    let mut out = Mesh {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        triangles: Vec::new(),
    };
    for prim in mesh.get("primitives").and_then(Value::as_list).unwrap_or_default() {
        if field(prim, "mode", 4) != 4 {
            return Err(format!("{shown}: only triangles"));
        }
        let at = prim.get("attributes").ok_or("a primitive without attributes")?;
        let base = out.positions.len() as u32;
        let pos = accessor(&doc, &bins, index(at, "POSITION")?)?;
        let nor = match at.get("NORMAL") {
            Some(_) => accessor(&doc, &bins, index(at, "NORMAL")?)?,
            None => vec![vec![0.0, 1.0, 0.0]; pos.len()],
        };
        let uvs = match at.get("TEXCOORD_0") {
            Some(_) => accessor(&doc, &bins, index(at, "TEXCOORD_0")?)?,
            None => vec![vec![0.0, 0.0]; pos.len()],
        };
        // glTF's Y up, back to Z up (Blender's axes)
        let turn = |p: &Vec<f64>| [p[0], -p[2], p[1]];
        out.positions.extend(pos.iter().map(turn));
        out.normals.extend(nor.iter().map(turn));
        out.uvs.extend(uvs.iter().map(|uv| [uv[0], uv[1]]));
        let idx: Vec<u32> = match prim.get("indices") {
            Some(_) => accessor(&doc, &bins, index(prim, "indices")?)?
                .iter()
                .map(|i| i[0] as u32)
                .collect(),
            None => (0..pos.len() as u32).collect(),
        };
        for j in (0..idx.len().saturating_sub(2)).step_by(3) {
            out.triangles
                .push([base + idx[j], base + idx[j + 1], base + idx[j + 2]]);
        }
    }
    Ok(out)
}
