//! The vertex colours of a group as the client of 3.3.5a fixes them when it loads it
//! (`FixColorVertexAlpha`), after the public description of the format and Noggit: the vertices of
//! the batches of transition, up to the last vertex of the last of them, lose the ambient colour of
//! the building and are darkened by their alpha, which stays the blend between the light inside and
//! outside; the others lose it and are brightened by their alpha, which becomes that of the group,
//! outside or inside. The colours are halved, the shader doubling them.

use uniwow_api::formats::WmoGroup;

/// The flags of a building: lit as one, its ambient colour taken as none; its vertex colours not
/// fixed, only the alpha of those after the transition set.
const LIT_AS_ONE: u16 = 0x2;
const NOT_FIXED: u16 = 0x8;
/// The flag of a group outside.
pub const OUTSIDE: u32 = 0x8;

/// The first vertex after those of the batches of transition of `group`.
fn after_transition(group: &WmoGroup) -> usize {
    match usize::from(group.batch_counts[0]) {
        0 => 0,
        transition => group
            .batches
            .get(transition - 1)
            .map_or(0, |batch| usize::from(batch.vertices[1]) + 1),
    }
}

/// Fixes the first set of vertex colours of `group`, of a building of `flags` and `ambient`.
pub fn fix(group: &mut WmoGroup, flags: u16, ambient: [u8; 4]) {
    let begin = after_transition(group);
    let alpha = if group.flags & OUTSIDE != 0 { 255 } else { 0 };
    let Some(colours) = group.colours.first_mut() else {
        return;
    };
    if flags & NOT_FIXED != 0 {
        for colour in colours.iter_mut().skip(begin) {
            colour[3] = alpha;
        }
        return;
    }
    let ambient = if flags & LIT_AS_ONE != 0 {
        [0.0; 3]
    } else {
        ambient.map(f32::from)[..3].try_into().unwrap_or([0.0; 3])
    };
    for (index, colour) in colours.iter_mut().enumerate() {
        let weight = f32::from(colour[3]);
        for (channel, ambient) in ambient.iter().enumerate() {
            let value = f32::from(colour[channel]);
            let fixed = if index < begin {
                (value - ambient) * (1.0 - weight / 255.0) / 2.0
            } else {
                (value * weight / 64.0 + value - ambient) / 2.0
            };
            colour[channel] = fixed.round().clamp(0.0, 255.0) as u8;
        }
        if index >= begin {
            colour[3] = alpha;
        }
    }
}
