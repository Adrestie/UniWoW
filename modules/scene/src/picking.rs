//! Picking in the registry: the broad phase by the bounds of the pieces then of their entities, the
//! nearest box first; the narrow phase by the providers' pickers, until the nearest exact hit is
//! nearer than the next box. And the entities whose origin falls in a rectangle of the view.

use std::collections::HashMap;
use std::sync::Arc;

use uniwow_api::glam::Vec3;
use uniwow_api::scene::{EntityId, EntityRef, Hit, Kind, Locks, Picked, Picker, Snapshot};
use uniwow_api::viewport::{Ray, Seen};

/// The chain of the game parents of `id`, from the outermost down to it.
pub fn chain(snapshot: &Snapshot, id: EntityId) -> Vec<EntityId> {
    let mut chain = vec![id];
    let mut at = snapshot.get(&id).and_then(|entity| entity.parent());
    while let Some(parent) = at {
        if chain.contains(&parent) || chain.len() > 16 {
            break;
        }
        chain.push(parent);
        at = snapshot.get(&parent).and_then(|entity| entity.parent());
    }
    chain.reverse();
    chain
}

/// The nearest entity `ray` hits among those of `snapshot` whose kind is not `locked`.
pub fn pick(snapshot: &Snapshot, ray: &Ray, locks: &Locks, pickers: &HashMap<String, Arc<dyn Picker>>) -> Option<Hit> {
    let mut boxes: Vec<(f32, u32, usize)> = Vec::new();
    for (slot, placed) in snapshot.slots.iter().enumerate() {
        let Some(placed) = placed else {
            continue;
        };
        if ray.hits_box(placed.bounds).is_none() {
            continue;
        }
        for row in 0..placed.piece.len() {
            let id = placed.piece.ids[row];
            if locks.kinds.contains(&id.kind) || !snapshot.is_first(slot as u32, row as u32, &id) {
                continue;
            }
            if let Some(distance) = ray.hits_box(placed.piece.bounds[row]) {
                let entity = EntityRef { snapshot, placed, row };
                if !locks.holds(&entity) {
                    boxes.push((distance, slot as u32, row));
                }
            }
        }
    }
    boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(f32, EntityId)> = None;
    for (entered, slot, row) in boxes {
        if best.is_some_and(|(distance, _)| distance <= entered) {
            break;
        }
        let Some(placed) = snapshot.placed(slot) else {
            continue;
        };
        let entity = EntityRef { snapshot, placed, row };
        let picked = pickers
            .get(&placed.key.provider)
            .map_or(Picked::ByBox, |picker| picker.intersect(&entity, ray));
        let distance = match picked {
            Picked::Hit(distance) => distance.max(entered),
            Picked::ByBox => entered,
            Picked::Missed => continue,
        };
        if best.is_none_or(|(nearest, _)| distance < nearest) {
            best = Some((distance, entity.id()));
        }
    }
    let (distance, id) = best?;
    Some(Hit {
        id,
        distance,
        chain: chain(snapshot, id),
    })
}

/// The entities of `snapshot` not `locked`, of `kinds` when given, whose origin falls in front of
/// the eye within `rect` of `seen`.
pub fn within(snapshot: &Snapshot, seen: &Seen, rect: [[f32; 2]; 2], kinds: &[Kind], locks: &Locks) -> Vec<EntityId> {
    let [low, high] = [
        [rect[0][0].min(rect[1][0]), rect[0][1].min(rect[1][1])],
        [rect[0][0].max(rect[1][0]), rect[0][1].max(rect[1][1])],
    ];
    snapshot
        .entities()
        .filter(|entity| {
            let kind = entity.id().kind;
            !locks.kinds.contains(&kind) && (kinds.is_empty() || kinds.contains(&kind)) && !locks.holds(entity)
        })
        .filter(|entity| {
            seen.project(Vec3::from(entity.origin()))
                .is_some_and(|[x, y]| (low[0]..=high[0]).contains(&x) && (low[1]..=high[1]).contains(&y))
        })
        .map(|entity| entity.id())
        .collect()
}
