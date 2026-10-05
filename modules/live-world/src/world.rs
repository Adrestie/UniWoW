//! The entities of the zone subscribed to, as the last messages of the observer left them: a
//! snapshot shared between threads, replaced whole, its entities shared between snapshots.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::glam::Vec3;
use uniwow_api::server_link::protocol::{CATMULL_ROM, CYCLIC, Entity, Kind};

/// An entity and when it was received, from which the time along its spline counts on.
#[derive(Debug)]
pub struct Tracked {
    pub entity: Entity,
    pub received: Instant,
}

impl Tracked {
    /// Where it stands at `now`: along its spline as the client computes it, linearly or by
    /// Catmull-Rom between its points, from the time gone at the reading and the time since it was
    /// received; where it was read otherwise.
    pub fn position_at(&self, now: Instant) -> [f32; 3] {
        let Some(spline) = &self.entity.spline else {
            return self.entity.position;
        };
        let points = &spline.points;
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return self.entity.position;
        };
        if points.len() == 1 {
            return first.position;
        }
        // A spline sent whole starts at 0. One sent by a window of its points does not, and is sent
        // again as its entity moves on, before it leaves the window.
        let whole = first.time == 0;
        let cyclic = spline.flags & CYCLIC != 0 && whole;
        let since = now.saturating_duration_since(self.received).as_millis() as u64;
        let mut time = u64::from(spline.elapsed) + since;
        if cyclic && last.time > 0 {
            time %= u64::from(last.time);
        }
        let time = time.clamp(u64::from(first.time), u64::from(last.time)) as u32;
        let next = points
            .iter()
            .position(|point| point.time > time)
            .unwrap_or(points.len() - 1)
            .max(1);
        let (a, b) = (points[next - 1], points[next]);
        let part = (time - a.time) as f32 / b.time.saturating_sub(a.time).max(1) as f32;
        if spline.flags & CATMULL_ROM == 0 {
            return std::array::from_fn(|axis| a.position[axis] + (b.position[axis] - a.position[axis]) * part);
        }
        // The points of control beyond each end, as AzerothCore makes them (`InitCatmullRom`): a
        // cyclic spline goes on round; else a yard back from the first point, the way it heads, and
        // the last point again.
        let before = match next {
            1 if cyclic && points.len() > 2 => points[points.len() - 2].position,
            1 => {
                let (p0, p1) = (Vec3::from(first.position), Vec3::from(points[1].position));
                (p0 - (p1 - p0).normalize_or_zero()).to_array()
            }
            _ => points[next - 2].position,
        };
        let after = match points.get(next + 1) {
            Some(point) => point.position,
            None if cyclic && points.len() > 2 => points[1].position,
            None => last.position,
        };
        catmull_rom([before, a.position, b.position, after], part)
    }
}

/// The point at `t`, from 0 to 1, of the Catmull-Rom segment between the second and the third of
/// `points`, with the weights of AzerothCore (`s_catmullRomCoeffs`).
pub fn catmull_rom(points: [[f32; 3]; 4], t: f32) -> [f32; 3] {
    let (t2, t3) = (t * t, t * t * t);
    let weights = [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ];
    std::array::from_fn(|axis| (0..4).map(|i| points[i][axis] * weights[i]).sum())
}

/// The entities of a zone of a map.
#[derive(Debug, Default)]
pub struct World {
    pub map: u32,
    pub instance: u32,
    pub sequence: u64,
    pub entities: HashMap<u64, Arc<Tracked>>,
}

/// What a message changed: the GUIDs of the entities that appeared, changed and left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub map: u32,
    pub instance: u32,
    pub sequence: u64,
    pub appeared: Vec<u64>,
    pub changed: Vec<u64>,
    pub left: Vec<u64>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.appeared.is_empty() && self.changed.is_empty() && self.left.is_empty()
    }
}

impl World {
    /// The world a SNAPSHOT makes of this one: every entity replaced.
    pub fn snapshot(
        &self,
        map: u32,
        instance: u32,
        sequence: u64,
        entities: Vec<Entity>,
        now: Instant,
    ) -> (World, Change) {
        let mut world = World {
            map,
            instance,
            sequence,
            entities: HashMap::with_capacity(entities.len()),
        };
        let mut change = Change {
            map,
            instance,
            sequence,
            ..Change::default()
        };
        for entity in entities {
            let guid = entity.guid;
            if self.entities.contains_key(&guid) && self.map == map && self.instance == instance {
                change.changed.push(guid);
            } else {
                change.appeared.push(guid);
            }
            world.entities.insert(guid, Arc::new(Tracked { entity, received: now }));
        }
        change.left = self
            .entities
            .keys()
            .filter(|guid| !world.entities.contains_key(guid) || self.map != map || self.instance != instance)
            .copied()
            .collect();
        (world, change)
    }

    /// The world a CHANGES makes of this one.
    pub fn changes(&self, sequence: u64, entities: Vec<Entity>, left: &[u64], now: Instant) -> (World, Change) {
        let mut world = World {
            map: self.map,
            instance: self.instance,
            sequence,
            entities: self.entities.clone(),
        };
        let mut change = Change {
            map: self.map,
            instance: self.instance,
            sequence,
            ..Change::default()
        };
        for entity in entities {
            let guid = entity.guid;
            match world.entities.insert(guid, Arc::new(Tracked { entity, received: now })) {
                Some(_) => change.changed.push(guid),
                None => change.appeared.push(guid),
            }
        }
        for guid in left {
            if world.entities.remove(guid).is_some() {
                change.left.push(*guid);
            }
        }
        (world, change)
    }

    /// The world once the zone is gone: no entity, every one left.
    pub fn emptied(&self) -> (World, Change) {
        let change = Change {
            map: self.map,
            instance: self.instance,
            sequence: self.sequence,
            left: self.entities.keys().copied().collect(),
            ..Change::default()
        };
        (World::default(), change)
    }

    /// The count of its entities of `kind`.
    pub fn count(&self, kind: Kind) -> usize {
        self.entities
            .values()
            .filter(|tracked| tracked.entity.kind == kind)
            .count()
    }
}
