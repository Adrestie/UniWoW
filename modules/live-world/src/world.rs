//! The entities of the zone subscribed to, as the last messages of the observer left them: a
//! snapshot shared between threads, replaced whole, its entities shared between snapshots.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::server_link::protocol::{CYCLIC, Entity, Kind};

/// An entity and when it was received, from which the time along its spline counts on.
#[derive(Debug)]
pub struct Tracked {
    pub entity: Entity,
    pub received: Instant,
}

impl Tracked {
    /// Where it stands at `now`: along its spline, linearly between its points, from the time
    /// gone at the reading and the time since it was received; where it was read otherwise.
    pub fn position_at(&self, now: Instant) -> [f32; 3] {
        let Some(spline) = &self.entity.spline else {
            return self.entity.position;
        };
        let (Some(first), Some(last)) = (spline.points.first(), spline.points.last()) else {
            return self.entity.position;
        };
        let since = now.saturating_duration_since(self.received).as_millis() as u64;
        let mut time = u64::from(spline.elapsed) + since;
        if spline.flags & CYCLIC != 0 && last.time > 0 {
            time %= u64::from(last.time);
        }
        let time = time.clamp(u64::from(first.time), u64::from(last.time)) as u32;
        let next = spline.points.iter().position(|point| point.time >= time).unwrap_or(0);
        if next == 0 {
            return spline.points[0].position;
        }
        let (a, b) = (spline.points[next - 1], spline.points[next]);
        let span = b.time.saturating_sub(a.time).max(1) as f32;
        let part = (time - a.time) as f32 / span;
        std::array::from_fn(|axis| a.position[axis] + (b.position[axis] - a.position[axis]) * part)
    }
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
