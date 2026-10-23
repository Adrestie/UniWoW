//! What each part of a description holds, as forge reads it: its keys, their kind, their default,
//! the shapes they belong to, and what they do (the tooltips). Nothing here the client cannot hold:
//! these are the keys forge writes into the model.

use uniwow_api::vfx::forge;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A number, dragged by `speed`, within a range.
    Num {
        min: f64,
        max: f64,
        speed: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Text,
    Bool,
    /// `#rrggbb`.
    Color,
    Vec3,
    /// Two numbers.
    Pair,
    Choice(&'static [&'static str]),
    /// Names of bits (or their numbers).
    Flags(&'static [(&'static str, u32)]),
    /// One of the description's textures.
    Texture,
    /// One of its bones (`root` made by forge).
    Bone,
    /// A number over the sequence: one value, keys `[[t, v], ...]`, or keys by phase.
    Animated,
    /// A colour over the sequence, the same ways.
    AnimatedColor,
    /// A number over a particle's life: one value or up to 8 keys.
    Life,
    /// A colour over a particle's life.
    LifeColor,
    /// A rotation [x, y, z] degrees, or keys of rotations by phase.
    Rotation,
    /// Anything else, edited as its JSON.
    Json,
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub tip: &'static str,
    pub kind: Kind,
    /// Its default when the description does not give it, as JSON.
    pub default: &'static str,
    /// The shapes it belongs to (all when empty).
    pub shapes: &'static [&'static str],
}

const fn f(key: &'static str, label: &'static str, kind: Kind, default: &'static str, tip: &'static str) -> Field {
    Field {
        key,
        label,
        tip,
        kind,
        default,
        shapes: &[],
    }
}

const fn s(field: Field, shapes: &'static [&'static str]) -> Field {
    Field { shapes, ..field }
}

const YARDS: Kind = Kind::Num {
    min: -100.0,
    max: 100.0,
    speed: 0.01,
};
const POSITIVE: Kind = Kind::Num {
    min: 0.0,
    max: 1000.0,
    speed: 0.01,
};
const SHARE: Kind = Kind::Num {
    min: 0.0,
    max: 1.0,
    speed: 0.005,
};
const DEGREES: Kind = Kind::Num {
    min: -3600.0,
    max: 3600.0,
    speed: 1.0,
};

const BLENDS: &[&str] = &["opaque", "key", "alpha", "add_opaque", "add", "mod", "mod2x"];
const PARTICLE_BLENDS: &[&str] = &["opaque", "key", "alpha", "add", "mod"];
const EMITTER_SHAPES: &[&str] = &["point", "plane", "sphere"];
const ENCODINGS: &[&str] = &["auto", "palette", "dxt5"];
pub const TEXTURE_KINDS: &[&str] = &[
    "petal",
    "glow",
    "spark",
    "star",
    "ring",
    "shockwave",
    "streak",
    "trail",
    "smoke",
    "noise",
    "swirl",
    "rune",
    "lightning",
    "solid",
];

pub const EFFECT: &[Field] = &[
    f(
        "name",
        "Name",
        Kind::Text,
        "\"vfx\"",
        "The file name of the model (<name>.m2, <name>00.skin): ASCII.",
    ),
    f(
        "duration",
        "Duration (ms)",
        Kind::Int { min: 1, max: 600_000 },
        "2000",
        "The length of the looping sequence (Stand), or of the hold of a state model.",
    ),
    f(
        "radius",
        "Radius",
        POSITIVE,
        "3.0",
        "The bounding radius: the client culls the model by it.",
    ),
    f(
        "sequences",
        "Phases",
        Kind::Json,
        "{}",
        "For a state model: {\"birth\": ms, \"hold\": ms, \"decay\": ms}, hold as long as the duration. The birth (Stand, 0) and the decay (159) play once, the hold (158) loops while the aura lasts.",
    ),
];

pub const TEXTURE: &[Field] = &[
    f(
        "kind",
        "Kind",
        Kind::Choice(TEXTURE_KINDS),
        "\"glow\"",
        "The shape drawn (white, the shape in the alpha: particles, ribbons and meshes tint it).",
    ),
    f(
        "color",
        "Colour",
        Kind::Color,
        "\"#ffffff\"",
        "A colour baked into the texture (white by default).",
    ),
    f(
        "encoding",
        "Encoding",
        Kind::Choice(ENCODINGS),
        "\"auto\"",
        "BLP2 encoding: a palette of 256 colours with 8-bit alpha (exact for white shapes), DXT5, or chosen by the colours.",
    ),
    f(
        "wrap",
        "Repeat",
        Kind::Bool,
        "false",
        "Repeat the texture (on by default for the noise only): a clamped edge keeps a glow or a trail from bleeding.",
    ),
    f(
        "game",
        "Game file",
        Kind::Text,
        "\"\"",
        "A texture of the client used as it is (Spells\\X.blp).",
    ),
    f(
        "file",
        "Image file",
        Kind::Text,
        "\"\"",
        "An image beside the description, made into the texture.",
    ),
    f(
        "cells",
        "Cells",
        Kind::Json,
        "[1, 1]",
        "[rows, cols] of a flipbook given as a file.",
    ),
];

/// The parameters of a texture kind, from texgen's table.
pub fn texture_parameters(kind: &str) -> Vec<(&'static str, uniwow_api::vfx::texgen::Param)> {
    uniwow_api::vfx::texgen::KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, params)| params.to_vec())
        .unwrap_or_default()
}

pub const BONE: &[Field] = &[
    f("pos", "Pivot", Kind::Vec3, "[0, 0, 0]", "The bone's pivot, in yards."),
    f("parent", "Parent", Kind::Bone, "\"root\"", "The bone it follows."),
    f(
        "rotation",
        "Rotation",
        Kind::Rotation,
        "[0, 0, 0]",
        "A fixed orientation [x, y, z] in degrees (Z, then Y, then X), or keys by phase (a petal opening).",
    ),
    f(
        "spin",
        "Spin (turns/s)",
        Kind::Num {
            min: -50.0,
            max: 50.0,
            speed: 0.01,
        },
        "0",
        "Turns a second about the bone's Z, rounded to whole turns a loop (no seam).",
    ),
    f(
        "orbit",
        "Orbit",
        Kind::Json,
        "{}",
        "{\"radius\", \"speed\" (turns/s), \"phase\" (share of a turn)}: a circle about the pivot.",
    ),
    f(
        "bob",
        "Bob",
        Kind::Json,
        "{}",
        "{\"height\", \"speed\" (turns/s)}: up and down.",
    ),
    f(
        "scale",
        "Scale",
        Kind::Animated,
        "1",
        "Its scale: a value, keys over the loop, or keys by phase.",
    ),
    f(
        "billboard",
        "Faces the eye",
        Kind::Bool,
        "false",
        "The bone turns to face the camera (bone flag 0x8).",
    ),
];

pub const EMITTER: &[Field] = &[
    f(
        "texture",
        "Texture",
        Kind::Texture,
        "\"\"",
        "The texture of its particles.",
    ),
    f("bone", "Bone", Kind::Bone, "\"root\"", "The bone it follows."),
    f(
        "pos",
        "Place",
        Kind::Vec3,
        "[0, 0, 0]",
        "Its place on the bone, in yards.",
    ),
    f(
        "shape",
        "Shape",
        Kind::Choice(EMITTER_SHAPES),
        "\"point\"",
        "Where particles are born: a point, a plane (length × width), or a sphere (between the inner and the outer radius).",
    ),
    s(
        f(
            "area",
            "Area",
            Kind::Pair,
            "[1, 1]",
            "[length, width] of the plane, in yards.",
        ),
        &["plane", "point"],
    ),
    s(
        f("radius", "Radius", POSITIVE, "0.5", "The outer radius of the sphere."),
        &["sphere"],
    ),
    s(
        f(
            "radius_min",
            "Inner radius",
            POSITIVE,
            "0.5",
            "The inner radius of the sphere.",
        ),
        &["sphere"],
    ),
    f(
        "rate",
        "Rate (/s)",
        Kind::Animated,
        "20",
        "Particles born a second: a value, keys over the loop, or keys by phase. The client keeps about 800 alive an emitter, 2,000 a model.",
    ),
    f(
        "until",
        "Until",
        SHARE,
        "1",
        "Births stop at this share of the loop (a window).",
    ),
    f(
        "bursts",
        "Bursts",
        Kind::Json,
        "[]",
        "[{\"at\": share of the loop, \"count\", \"window\" ms}]: particles at once; the window widens to stay under the client's load.",
    ),
    f("life", "Life (s)", POSITIVE, "1", "How long a particle lives."),
    f("life_vary", "Life ±", SHARE, "0", "Its life varies by this share."),
    f("speed", "Speed", Kind::Animated, "1", "Yards a second at birth."),
    f("speed_vary", "Speed ±", SHARE, "0", "Its speed varies by this share."),
    f(
        "spread",
        "Spread (°)",
        DEGREES,
        "0",
        "The cone of directions about up (180: every direction).",
    ),
    f(
        "around",
        "Around (°)",
        DEGREES,
        "360",
        "The directions turned about up.",
    ),
    f(
        "gravity",
        "Gravity",
        Kind::Animated,
        "0",
        "Pulls the particles down (negative: up), yards a second².",
    ),
    f(
        "drag",
        "Drag",
        POSITIVE,
        "0",
        "Slows the particles: a share of their speed a second.",
    ),
    f(
        "z_source",
        "Source below",
        POSITIVE,
        "0",
        "Particles fly away from a point this far below the emitter.",
    ),
    f(
        "colors",
        "Colour (life)",
        Kind::LifeColor,
        "[[0, \"#ffffff\"], [1, \"#ffffff\"]]",
        "The colour over a particle's life, 8 keys at most.",
    ),
    f(
        "alpha",
        "Alpha (life)",
        Kind::Life,
        "[[0, 0], [0.15, 1], [1, 0]]",
        "The opacity over a particle's life, 8 keys at most.",
    ),
    f(
        "size",
        "Size (life)",
        Kind::Life,
        "0.5",
        "The size of the quad over a particle's life (its whole side, yards), 8 keys at most.",
    ),
    f("size_vary", "Size ±", SHARE, "0", "Its size varies by this share."),
    f("spin", "Spin (°/s)", DEGREES, "0", "Degrees a second the quad turns."),
    f("spin_vary", "Spin ±", DEGREES, "0", "Its spin varies by this much."),
    f(
        "base_spin",
        "Start angle (°)",
        DEGREES,
        "0",
        "The quad's angle at birth.",
    ),
    f(
        "base_spin_vary",
        "Start angle ±",
        DEGREES,
        "0",
        "Its angle at birth varies by this much.",
    ),
    f(
        "blend",
        "Blending",
        Kind::Choice(PARTICLE_BLENDS),
        "\"add\"",
        "How the particles mix with what is behind (the client's particles: opaque, key, alpha, add, mod).",
    ),
    f(
        "flipbook",
        "Flipbook",
        Kind::Bool,
        "false",
        "Plays the texture's cells over the particle's life (smoke, lightning).",
    ),
    f("tail", "Tail", POSITIVE, "0.1", "The length of a trailing particle."),
    f(
        "flags",
        "Flags",
        Kind::Flags(&forge::PARTICLE_FLAGS),
        "[]",
        "Bits added to the base flags (0x20029 by default, the client's most common set).",
    ),
    f(
        "base_flags",
        "Base flags",
        Kind::Text,
        "\"0x20029\"",
        "The flags the named ones are added to.",
    ),
    f("rate_vary", "Rate ±", SHARE, "0", "Its rate varies by this share."),
];

pub const RIBBON: &[Field] = &[
    f(
        "texture",
        "Texture",
        Kind::Texture,
        "\"\"",
        "The texture along the ribbon (u along, v across).",
    ),
    f("bone", "Bone", Kind::Bone, "\"root\"", "The moving bone it trails."),
    f("pos", "Place", Kind::Vec3, "[0, 0, 0]", "Its place on the bone."),
    f(
        "color",
        "Colour",
        Kind::AnimatedColor,
        "\"#ffffff\"",
        "Its colour over the sequence.",
    ),
    f("alpha", "Alpha", Kind::Animated, "1", "Its opacity over the sequence."),
    f(
        "above",
        "Above",
        Kind::Animated,
        "0.2",
        "Its height above the bone, yards.",
    ),
    f(
        "below",
        "Below",
        Kind::Animated,
        "0.2",
        "Its height below the bone, yards.",
    ),
    f(
        "edges",
        "Edges (/s)",
        POSITIVE,
        "30",
        "Edges laid a second; edges × life stays under 64 (the client's).",
    ),
    f(
        "life",
        "Life (s)",
        POSITIVE,
        "0.5",
        "How long an edge lives: the length of the trail.",
    ),
    f("gravity", "Gravity", YARDS, "0", "Pulls the trail down."),
    f(
        "blend",
        "Blending",
        Kind::Choice(BLENDS),
        "\"add\"",
        "How it mixes with what is behind.",
    ),
    f(
        "priority",
        "Priority",
        Kind::Int { min: -128, max: 127 },
        "0",
        "Its place in the order of drawing.",
    ),
    f(
        "material_flags",
        "Material",
        Kind::Flags(&forge::MATERIAL_FLAGS),
        "[\"unlit\", \"no_fog\", \"two_sided\", \"no_depth_write\"]",
        "The material's flags (0x17 by default, common in the client's spells).",
    ),
];

const ALL_SHAPES: &[&str] = &["gltf", "petal", "ring", "disc", "plane", "sphere", "cone", "cylinder"];

pub const MESH: &[Field] = &[
    f(
        "shape",
        "Shape",
        Kind::Choice(ALL_SHAPES),
        "\"plane\"",
        "A generated shape, or a mesh from Blender (glTF 2.0, Z up).",
    ),
    f("texture", "Texture", Kind::Texture, "\"\"", "Its texture."),
    f("bone", "Bone", Kind::Bone, "\"root\"", "The bone its own bone follows."),
    f("pos", "Place", Kind::Vec3, "[0, 0, 0]", "Its pivot, in yards."),
    f(
        "rotation",
        "Rotation",
        Kind::Rotation,
        "[0, 0, 0]",
        "Its orientation, or keys by phase.",
    ),
    f(
        "spin",
        "Spin (turns/s)",
        Kind::Num {
            min: -50.0,
            max: 50.0,
            speed: 0.01,
        },
        "0",
        "Turns a second about its Z, whole turns a loop.",
    ),
    f("scale", "Scale", Kind::Animated, "1", "Its scale over the sequence."),
    f(
        "orbit",
        "Orbit",
        Kind::Json,
        "{}",
        "{\"radius\", \"speed\", \"phase\"}.",
    ),
    f("bob", "Bob", Kind::Json, "{}", "{\"height\", \"speed\"}."),
    f(
        "billboard",
        "Faces the eye",
        Kind::Bool,
        "false",
        "Turns to face the camera.",
    ),
    s(
        f(
            "segments",
            "Segments",
            Kind::Int { min: 3, max: 256 },
            "48",
            "Segments around.",
        ),
        &["ring", "disc", "sphere", "cone", "cylinder"],
    ),
    s(
        f("inner", "Inner radius", POSITIVE, "0.8", "The ring's inner radius."),
        &["ring"],
    ),
    s(
        f("outer", "Outer radius", POSITIVE, "1.0", "The ring's outer radius."),
        &["ring"],
    ),
    s(
        f("radius", "Radius", POSITIVE, "1.0", "Its radius."),
        &["disc", "sphere", "cone", "cylinder"],
    ),
    s(
        f(
            "rim",
            "Rim",
            YARDS,
            "0",
            "Lifts the disc's edge into a shallow bowl, so that a ground circle stays above a slope.",
        ),
        &["disc"],
    ),
    s(
        f("rows", "Rows", Kind::Int { min: 1, max: 64 }, "4", "Rows along it."),
        &["disc", "cone", "cylinder", "petal"],
    ),
    s(
        f(
            "rings",
            "Rings",
            Kind::Int { min: 2, max: 128 },
            "24",
            "Rings from pole to pole.",
        ),
        &["sphere"],
    ),
    s(f("width", "Width", POSITIVE, "2.0", "Its width."), &["plane", "petal"]),
    s(
        f("length", "Length", POSITIVE, "2.0", "Its length."),
        &["plane", "petal"],
    ),
    s(
        f("height", "Height", POSITIVE, "2.0", "Its height."),
        &["cone", "cylinder"],
    ),
    s(
        f("top", "Top radius", POSITIVE, "0", "The radius at its top."),
        &["cone", "cylinder"],
    ),
    s(
        f("cup", "Cup", YARDS, "0.15", "How much the petal cups across."),
        &["petal"],
    ),
    s(
        f(
            "curl",
            "Curl",
            YARDS,
            "0.1",
            "How much its tip bends up (down when negative).",
        ),
        &["petal"],
    ),
    s(
        f("cols", "Columns", Kind::Int { min: 1, max: 64 }, "6", "Columns across."),
        &["petal"],
    ),
    s(
        f(
            "span",
            "Span",
            Kind::Pair,
            "[0, 1]",
            "[u0, u1]: only that stretch of the petal (0 base, 1 tip), base and tip on bones of their own.",
        ),
        &["petal"],
    ),
    s(
        f(
            "file",
            "File",
            Kind::Text,
            "\"\"",
            "The .glb or .gltf beside the description.",
        ),
        &["gltf"],
    ),
    s(
        f(
            "mesh",
            "Mesh",
            Kind::Text,
            "\"\"",
            "Its mesh's name in the file (the first by default).",
        ),
        &["gltf"],
    ),
    s(f("size", "Size", POSITIVE, "1", "Its scale."), &["gltf"]),
    f(
        "bend",
        "Bend",
        Kind::Json,
        "[]",
        "[{\"at\": share of the length along +X, \"rotation\": value or keys by phase}]: up to 4 joints, each a bone; the vertices about a joint shared between its two bones.",
    ),
    f(
        "bend_soft",
        "Bend softness",
        SHARE,
        "0.08",
        "The share of the length about each joint whose vertices are shared.",
    ),
    f(
        "uv_tile",
        "Texture repeat",
        Kind::Pair,
        "[1, 1]",
        "How many times the texture repeats along u and v.",
    ),
    f(
        "uv_scroll",
        "Texture scroll",
        Kind::Pair,
        "[0, 0]",
        "Scroll a second along u and v, rounded to whole turns a loop.",
    ),
    f(
        "color",
        "Colour",
        Kind::AnimatedColor,
        "\"#ffffff\"",
        "Its colour over the sequence.",
    ),
    f("alpha", "Alpha", Kind::Animated, "1", "Its opacity over the sequence."),
    f(
        "blend",
        "Blending",
        Kind::Choice(BLENDS),
        "\"add\"",
        "How it mixes with what is behind.",
    ),
    f(
        "priority",
        "Priority",
        Kind::Int { min: -128, max: 127 },
        "0",
        "Its place in the order of drawing.",
    ),
    s(
        f(
            "projected",
            "On the ground",
            Kind::Bool,
            "false",
            "Laid by the client on the terrain under it (batch flags 0x1C), as Blizzard's ground circles; its texture's edge faded.",
        ),
        &["plane"],
    ),
    f(
        "material_flags",
        "Material",
        Kind::Flags(&forge::MATERIAL_FLAGS),
        "[\"unlit\", \"no_fog\", \"two_sided\", \"no_depth_write\"]",
        "The material's flags.",
    ),
];

pub const LIGHT: &[Field] = &[
    f("bone", "Bone", Kind::Bone, "\"root\"", "The bone it follows."),
    f("pos", "Place", Kind::Vec3, "[0, 0, 0]", "Its place on the bone."),
    f("color", "Colour", Kind::AnimatedColor, "\"#ffffff\"", "Its colour."),
    f(
        "intensity",
        "Intensity",
        Kind::Animated,
        "1",
        "Its strength (the client's: 1 to 3).",
    ),
    f(
        "ambient",
        "Ambient",
        Kind::AnimatedColor,
        "\"#000000\"",
        "Its ambient colour.",
    ),
    f(
        "ambient_intensity",
        "Ambient intensity",
        Kind::Animated,
        "0",
        "Its ambient strength.",
    ),
    f(
        "start",
        "Fade from",
        Kind::Animated,
        "0.5",
        "The distance where it starts to fade, yards.",
    ),
    f("end", "Fade to", Kind::Animated, "4", "The distance where it is gone."),
    f(
        "visible",
        "Visible",
        Kind::Animated,
        "1",
        "1 lit, 0 off: keys make a flash.",
    ),
];
