//! The checks of m2port's `validate.py`: a model the client's own spell models would not hold is
//! refused. Every index inside its table, every track's keys matching the sequences, every value
//! inside the domains found on the client's 3.3.5a spell models, the particle load under the
//! client's.

use crate::m2::{Model, Skin, Track, TrackBase};

/// What `port.py` of m2port keeps from the client's own models.
pub const PARTICLE_FLAGS: u32 = 0x000F_F7FF & !0x800;
pub const MATERIAL_FLAGS: u16 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x40 | 0x100;
pub const SEQUENCE_FLAGS: u32 = 0x1 | 0x2 | 0x20 | 0x40 | 0x80;
pub const HEADER_FLAGS: u32 = 0x1 | 0x2 | 0x4 | 0x10;
pub const BATCH_FLAGS: u8 = 0x1F;
pub const PARTICLE_BLENDS: [u8; 5] = [0, 1, 2, 4, 5];
pub const EMITTER_TYPES: [u8; 3] = [1, 2, 3];
pub const PARTICLE_LOAD: f64 = 800.0;
pub const MODEL_LOAD: f64 = 2000.0;
pub const MAX_ANIM_ID: u16 = 505;
pub const BONE_PALETTES: [u32; 4] = [21, 53, 64, 256];
pub const SUBMESH_BONES: u16 = 64;
pub const INTERPOLATIONS: [u16; 4] = [0, 1, 2, 3];

/// A track as the checks see it: its keys, and the count of values of each key set (none for a
/// track without values).
struct Keys<'a> {
    interpolation: u16,
    global_sequence: i16,
    timestamps: &'a [Vec<u32>],
    values: Option<Vec<usize>>,
}

fn keys<T>(t: &Track<T>) -> Keys<'_> {
    Keys {
        interpolation: t.interpolation,
        global_sequence: t.global_sequence,
        timestamps: &t.timestamps,
        values: Some(t.values.iter().map(Vec::len).collect()),
    }
}

fn keys_base(t: &TrackBase) -> Keys<'_> {
    Keys {
        interpolation: t.interpolation,
        global_sequence: t.global_sequence,
        timestamps: &t.timestamps,
        values: None,
    }
}

fn check_track(faults: &mut Vec<String>, tr: &Keys, n_seq: usize, n_globals: usize, what: &str) {
    let mut r = |ok: bool, text: String| {
        if !ok {
            faults.push(text);
        }
    };
    let gs = tr.global_sequence;
    r(
        INTERPOLATIONS.contains(&tr.interpolation),
        format!("{what}: interpolation {}", tr.interpolation),
    );
    let ts = tr.timestamps;
    if gs >= 0 {
        r(
            (gs as usize) < n_globals,
            format!("{what}: global sequence {gs} of {n_globals}"),
        );
        r(
            ts.len() <= 1,
            format!("{what}: {} key sets on a global sequence", ts.len()),
        );
    } else {
        r(
            ts.len() <= n_seq.max(1),
            format!("{what}: {} key sets for {n_seq} sequences", ts.len()),
        );
    }
    if let Some(vs) = &tr.values {
        r(
            vs.len() == ts.len(),
            format!("{what}: {} value sets for {} key sets", vs.len(), ts.len()),
        );
        for (i, (t, v)) in ts.iter().zip(vs).enumerate() {
            r(t.len() == *v, format!("{what}[{i}]: {} keys, {v} values", t.len()));
            r(
                t.windows(2).all(|w| w[0] <= w[1]),
                format!("{what}[{i}]: keys out of order"),
            );
        }
    }
}

/// Every track of the model, named as `validate.py` names them, in the order of the header.
fn tracks<'a>(m: &'a Model) -> Vec<(String, Keys<'a>)> {
    let mut out = Vec::new();
    let mut add = |name: &str, i: usize, field: &str, k: Keys<'a>| out.push((format!("{name}[{i}].{field}"), k));
    for (i, b) in m.bones.iter().enumerate() {
        add("bones", i, "translation", keys(&b.translation));
        add("bones", i, "rotation", keys(&b.rotation));
        add("bones", i, "scale", keys(&b.scale));
    }
    for (i, c) in m.colors.iter().enumerate() {
        add("colors", i, "color", keys(&c.color));
        add("colors", i, "alpha", keys(&c.alpha));
    }
    for (i, w) in m.texture_weights.iter().enumerate() {
        add("texture_weights", i, "weight", keys(&w.weight));
    }
    for (i, t) in m.texture_transforms.iter().enumerate() {
        add("texture_transforms", i, "translation", keys(&t.translation));
        add("texture_transforms", i, "rotation", keys(&t.rotation));
        add("texture_transforms", i, "scaling", keys(&t.scaling));
    }
    for (i, a) in m.attachments.iter().enumerate() {
        add("attachments", i, "animate_attached", keys(&a.animate_attached));
    }
    for (i, e) in m.events.iter().enumerate() {
        add("events", i, "enabled", keys_base(&e.enabled));
    }
    for (i, l) in m.lights.iter().enumerate() {
        add("lights", i, "ambient_color", keys(&l.ambient_color));
        add("lights", i, "ambient_intensity", keys(&l.ambient_intensity));
        add("lights", i, "diffuse_color", keys(&l.diffuse_color));
        add("lights", i, "diffuse_intensity", keys(&l.diffuse_intensity));
        add("lights", i, "attenuation_start", keys(&l.attenuation_start));
        add("lights", i, "attenuation_end", keys(&l.attenuation_end));
        add("lights", i, "visibility", keys(&l.visibility));
    }
    for (i, c) in m.cameras.iter().enumerate() {
        add("cameras", i, "positions", keys(&c.positions));
        add("cameras", i, "target_positions", keys(&c.target_positions));
        add("cameras", i, "roll", keys(&c.roll));
    }
    for (i, r) in m.ribbons.iter().enumerate() {
        add("ribbon_emitters", i, "color", keys(&r.color));
        add("ribbon_emitters", i, "alpha", keys(&r.alpha));
        add("ribbon_emitters", i, "height_above", keys(&r.height_above));
        add("ribbon_emitters", i, "height_below", keys(&r.height_below));
        add("ribbon_emitters", i, "tex_slot", keys(&r.tex_slot));
        add("ribbon_emitters", i, "visibility", keys(&r.visibility));
    }
    for (i, p) in m.particles.iter().enumerate() {
        for (field, k) in [
            ("emission_speed", keys(&p.emission_speed)),
            ("speed_variation", keys(&p.speed_variation)),
            ("vertical_range", keys(&p.vertical_range)),
            ("horizontal_range", keys(&p.horizontal_range)),
            ("gravity", keys(&p.gravity)),
            ("lifespan", keys(&p.lifespan)),
            ("emission_rate", keys(&p.emission_rate)),
            ("area_length", keys(&p.area_length)),
            ("area_width", keys(&p.area_width)),
            ("z_source", keys(&p.z_source)),
            ("enabled", keys(&p.enabled)),
        ] {
            add("particle_emitters", i, field, k);
        }
    }
    out
}

fn values_of(t: &Track<f32>) -> impl Iterator<Item = f64> + '_ {
    t.values.iter().flatten().map(|v| f64::from(*v))
}

/// The particles one emitter keeps alive at most: its highest rate times its longest life.
pub fn emitter_load(p: &crate::m2::Particle) -> f64 {
    let rate = values_of(&p.emission_rate).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    let life = values_of(&p.lifespan).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    match (rate, life) {
        (Some(rate), Some(life)) => rate * life.max(0.01),
        _ => 0.0,
    }
}

pub fn model_load(m: &Model) -> f64 {
    crate::py::sum(m.particles.iter().map(emitter_load))
}

fn list_repr(values: &[i64]) -> String {
    let parts: Vec<String> = values.iter().take(4).map(i64::to_string).collect();
    format!("[{}]", parts.join(", "))
}

/// The faults of a model and its skin (none: the model is taken). `skin` absent: the fault "skin
/// missing", as for a model without its file.
pub fn validate(m: &Model, skin: Option<&Skin>) -> Vec<String> {
    validate_with(m, skin, false)
}

/// The checks, `corpus` leaving out those that are the converter's own policy (the global loops of
/// 0 ms, the particle load), as for the client's own models.
pub fn validate_with(m: &Model, skin: Option<&Skin>, corpus: bool) -> Vec<String> {
    let mut faults = Vec::new();
    let mut r = |ok: bool, text: String| {
        if !ok {
            faults.push(text);
        }
    };
    r(m.version == 264, format!("version {}", m.version));
    let n_seq = m.sequences.len();
    let g = &m.global_loops;
    let mut track_faults = Vec::new();
    for (what, k) in tracks(m) {
        check_track(&mut track_faults, &k, n_seq, g.len(), &what);
    }
    for fault in track_faults {
        r(false, fault);
    }
    for s in &m.sequences {
        r(
            s.flags & crate::m2::EMBEDDED != 0,
            format!("sequence {}: keys outside the model", s.id),
        );
        r(s.flags & !SEQUENCE_FLAGS == 0, format!("sequence flags {:#x}", s.flags));
        r(s.id <= MAX_ANIM_ID, format!("animation id {}", s.id));
    }
    r(
        m.flags & !(HEADER_FLAGS | crate::m2::COMBINER_FLAG) == 0,
        format!("header flags {:#x}", m.flags),
    );
    if !corpus {
        r(!g.contains(&0), "global loop of 0 ms".into());
    }

    let n_bones = m.bones.len();
    let mut idx = |name: &str, values: &[i16], target: &str, n: usize| {
        let bad: Vec<i64> = values
            .iter()
            .map(|v| i64::from(*v))
            .filter(|v| !((0..n as i64).contains(v) || *v == -1))
            .collect();
        r(
            bad.is_empty(),
            format!("{name} past {target} ({n}): {}", list_repr(&bad)),
        );
    };
    idx("texture_lookup", &m.texture_lookup, "textures", m.textures.len());
    idx(
        "transparency_lookup",
        &m.transparency_lookup,
        "texture_weights",
        m.texture_weights.len(),
    );
    idx(
        "texture_transform_lookup",
        &m.texture_transform_lookup,
        "texture_transforms",
        m.texture_transforms.len(),
    );
    idx("bone_lookup", &m.bone_lookup, "bones", n_bones);
    idx("key_bone_lookup", &m.key_bone_lookup, "bones", n_bones);
    idx(
        "attachment_lookup",
        &m.attachment_lookup,
        "attachments",
        m.attachments.len(),
    );
    let mut r = |ok: bool, text: String| {
        if !ok {
            faults.push(text);
        }
    };
    for (i, bn) in m.bones.iter().enumerate() {
        r(
            (bn.parent as i64) < n_bones as i64 && bn.parent >= -1,
            format!("bone {i} parent {}", bn.parent),
        );
    }
    for (i, mat) in m.materials.iter().enumerate() {
        r(mat.blend <= 6, format!("material {i} blend {}", mat.blend));
        r(
            mat.flags & !MATERIAL_FLAGS == 0,
            format!("material {i} flags {:#x}", mat.flags),
        );
    }
    for (i, t) in m.textures.iter().enumerate() {
        r(
            t.kind != 0 || t.filename.last() == Some(&0),
            format!("texture {i} without a path"),
        );
    }
    let n_textures = m.textures.len();
    for (i, p) in m.particles.iter().enumerate() {
        r((p.bone as usize) < n_bones, format!("particle {i} bone {}", p.bone));
        r(
            (p.texture as usize) < n_textures,
            format!("particle {i} texture {}", p.texture),
        );
        r(
            PARTICLE_BLENDS.contains(&p.blend),
            format!("particle {i} blend {}", p.blend),
        );
        r(
            EMITTER_TYPES.contains(&p.emitter_type),
            format!("particle {i} emitter type {}", p.emitter_type),
        );
        r(
            p.flags & !PARTICLE_FLAGS == 0,
            format!("particle {i} flags {:#x}", p.flags),
        );
        r(
            p.rows >= 1 && p.cols >= 1,
            format!("particle {i} cells {}x{}", p.rows, p.cols),
        );
        r(
            p.geometry_model.is_empty() || corpus,
            format!("particle {i} geometry model"),
        );
        if !corpus && !p.emission_rate.values.iter().all(Vec::is_empty) && !p.lifespan.values.iter().all(Vec::is_empty)
        {
            let rate = values_of(&p.emission_rate).fold(f64::NEG_INFINITY, f64::max);
            let life = values_of(&p.lifespan).fold(f64::NEG_INFINITY, f64::max);
            r(rate * life <= PARTICLE_LOAD * 1.01, format!("particle {i} load"));
        }
    }
    if !corpus {
        let load = model_load(m);
        r(
            load <= MODEL_LOAD * 1.01,
            format!("particle load of the model {load:.0}"),
        );
    }
    for (i, rb) in m.ribbons.iter().enumerate() {
        r((rb.bone as usize) < n_bones, format!("ribbon {i} bone"));
        r(
            rb.textures.iter().all(|t| (*t as usize) < n_textures),
            format!("ribbon {i} textures"),
        );
        r(
            rb.materials.iter().all(|t| (*t as usize) < m.materials.len()),
            format!("ribbon {i} materials"),
        );
        r(
            rb.texture_rows >= 1 && rb.texture_cols >= 1,
            format!("ribbon {i} cells"),
        );
    }
    for (i, a) in m.attachments.iter().enumerate() {
        r((a.bone as usize) < n_bones, format!("attachment {i} bone"));
    }
    for (i, e) in m.events.iter().enumerate() {
        r(
            (e.bone as usize) < n_bones || e.bone == 0xFFFF_FFFF,
            format!("event {i} bone"),
        );
    }

    let Some(s) = skin else {
        r(false, "skin missing".into());
        return faults;
    };
    let most = s.submeshes.iter().map(|sm| sm.bone_count).max().unwrap_or(0);
    r(
        BONE_PALETTES.contains(&s.bone_count_max),
        format!("bone palette {} (client: (21, 53, 64, 256))", s.bone_count_max),
    );
    r(
        s.bone_count_max >= u32::from(most),
        format!("bone palette {} under {most} bones", s.bone_count_max),
    );
    r(
        corpus || most <= SUBMESH_BONES,
        format!("a submesh animated by {most} bones (at most {SUBMESH_BONES})"),
    );
    for (i, sm) in s.submeshes.iter().enumerate() {
        let start = sm.level as usize * 65536 + sm.index_start as usize;
        let (lo, hi) = (
            sm.vertex_start as usize,
            sm.vertex_start as usize + sm.vertex_count as usize,
        );
        let end = (start + sm.index_count as usize).min(s.indices.len());
        let inside = s.indices.get(start.min(end)..end).unwrap_or_default();
        r(
            inside.iter().all(|v| (lo..hi).contains(&(*v as usize))),
            format!("submesh {i}: triangles outside its vertices"),
        );
        let palette = sm.bone_count;
        let named =
            (lo..hi.min(s.bones.len())).all(|v| u16::from(*s.bones[v].iter().max().expect("four")) < palette.max(1));
        r(
            named || palette == 0,
            format!("submesh {i}: a vertex names a bone past its {palette}"),
        );
    }
    let nv = m.vertices.len();
    r(
        s.vertices.iter().all(|v| (*v as usize) < nv),
        "skin vertex past the model vertices".into(),
    );
    r(
        s.indices.iter().all(|i| (*i as usize) < s.vertices.len()),
        "skin index past the skin vertices".into(),
    );
    r(
        s.bones.len() == s.vertices.len(),
        "skin bones and vertices differ in count".into(),
    );
    for (i, sm) in s.submeshes.iter().enumerate() {
        let base = sm.level as usize * 65536;
        r(
            sm.vertex_start as usize + sm.vertex_count as usize <= s.vertices.len(),
            format!("submesh {i} vertices"),
        );
        r(
            base + sm.index_start as usize + sm.index_count as usize <= s.indices.len(),
            format!("submesh {i} indices"),
        );
        r(
            sm.bone_combo as usize + sm.bone_count as usize <= m.bone_lookup.len() || sm.bone_count == 0,
            format!("submesh {i} bones"),
        );
    }
    let combos = &m.texture_combiner_combos;
    for (i, bt) in s.batches.iter().enumerate() {
        let tc = bt.texture_count as usize;
        r(tc == 1 || tc == 2, format!("batch {i} {tc} textures"));
        r(bt.flags & !BATCH_FLAGS == 0, format!("batch {i} flags {:#x}", bt.flags));
        r((bt.submesh as usize) < s.submeshes.len(), format!("batch {i} submesh"));
        r(
            (bt.material as usize) < m.materials.len(),
            format!("batch {i} material"),
        );
        r(
            bt.color == -1 || (bt.color as i64) < m.colors.len() as i64,
            format!("batch {i} color {}", bt.color),
        );
        r(
            bt.texture_combo as usize + tc <= m.texture_lookup.len(),
            format!("batch {i} texture combo"),
        );
        r(
            bt.coord_combo as usize + tc <= m.texture_unit_lookup.len(),
            format!("batch {i} UV combo"),
        );
        r(
            bt.weight_combo as usize + tc <= m.transparency_lookup.len() || corpus,
            format!("batch {i} weight combo"),
        );
        r(
            bt.transform_combo as usize + tc <= m.texture_transform_lookup.len() || corpus,
            format!("batch {i} transform combo"),
        );
        if m.flags & crate::m2::COMBINER_FLAG != 0 {
            r(
                bt.shader as usize + tc <= combos.len(),
                format!("batch {i} shader {} past the combos", bt.shader),
            );
        } else {
            r(tc == 1, format!("batch {i}: two textures without combos"));
        }
    }
    faults
}
