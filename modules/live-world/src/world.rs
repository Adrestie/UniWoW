//! The entities of the zone subscribed to, as the last messages of the observer left them: a
//! snapshot shared between threads, replaced whole, its entities shared between snapshots.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::glam::{Quat, Vec3};
use uniwow_api::models::Motion;
use uniwow_api::server_link::protocol::{CATMULL_ROM, CYCLIC, Entity, Kind, Spline, WALKING};

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
        match (&self.entity.spline, self.time_at(now)) {
            (Some(spline), Some(time)) => point_at(spline, time),
            (Some(spline), None) if spline.points.len() == 1 => spline.points[0].position,
            _ => self.entity.position,
        }
    }

    /// The milliseconds along its spline at `now`, from its first point to its last; none without a
    /// spline of two points.
    fn time_at(&self, now: Instant) -> Option<u32> {
        let spline = self.entity.spline.as_ref()?;
        let (first, last) = (spline.points.first()?, spline.points.last()?);
        if spline.points.len() < 2 {
            return None;
        }
        let since = now.saturating_duration_since(self.received).as_millis() as u64;
        let mut time = u64::from(spline.elapsed) + since;
        if cyclic(spline) && last.time > 0 {
            time %= u64::from(last.time);
        }
        Some(time.clamp(u64::from(first.time), u64::from(last.time)) as u32)
    }

    /// How it moves at `now`: along its spline, at the speed of the stretch it is on, walking when
    /// its flags say so; standing without one, or at its end. A creature walking along its
    /// waypoints is not flagged: AzerothCore gives it the speed of walking alone, and the spline of
    /// 3.3.5a carries no flag of walking.
    pub fn motion_at(&self, now: Instant) -> Motion {
        let (Some(spline), Some(time)) = (&self.entity.spline, self.time_at(now)) else {
            return Motion::Standing;
        };
        let points = &spline.points;
        if time >= points[points.len() - 1].time && !cyclic(spline) {
            return Motion::Standing;
        }
        let next = points
            .iter()
            .position(|point| point.time > time)
            .unwrap_or(points.len() - 1)
            .max(1);
        let (a, b) = (points[next - 1], points[next]);
        let seconds = b.time.saturating_sub(a.time) as f32 / 1000.0;
        if seconds <= 0.0 {
            return Motion::Standing;
        }
        let speed = Vec3::from(b.position).distance(Vec3::from(a.position)) / seconds;
        if self.entity.flags & WALKING != 0 {
            Motion::Walking(speed)
        } else {
            Motion::Moving(speed)
        }
    }

    /// How it is turned at `now`: a game object by its quaternion; a creature along its spline
    /// while it moves, as the client turns it, the way it came at its end; by its orientation
    /// otherwise.
    pub fn rotation_at(&self, now: Instant) -> Quat {
        if let Some([x, y, z, w]) = self.entity.rotation {
            let rotation = Quat::from_xyzw(x, y, z, w);
            if rotation.length() > 0.5 {
                return rotation.normalize();
            }
        }
        if let (Some(spline), Some(time)) = (&self.entity.spline, self.time_at(now)) {
            let (first, last) = (spline.points[0].time, spline.points[spline.points.len() - 1].time);
            let at = |time: u32| Vec3::from(point_at(spline, time));
            let here = at(time);
            let ahead = at((time + STEP).min(last));
            let behind = at(time.saturating_sub(STEP).max(first));
            for way in [ahead - here, here - behind] {
                if way.truncate().length_squared() > 1e-6 {
                    return Quat::from_rotation_z(way.y.atan2(way.x));
                }
            }
        }
        Quat::from_rotation_z(self.entity.orientation)
    }
}

/// Whether `spline` goes on round: a cyclic one sent whole, from 0. One sent by a window of its
/// points does not start at 0, and is sent again as its entity moves on, before it leaves the
/// window.
fn cyclic(spline: &Spline) -> bool {
    spline.flags & CYCLIC != 0 && spline.points.first().is_some_and(|first| first.time == 0)
}

/// The point of `spline`, of two points or more, at `time`, within its points.
fn point_at(spline: &Spline, time: u32) -> [f32; 3] {
    let points = &spline.points;
    let (first, last) = (points[0], points[points.len() - 1]);
    let next = points
        .iter()
        .position(|point| point.time > time)
        .unwrap_or(points.len() - 1)
        .max(1);
    let (a, b) = (points[next - 1], points[next]);
    let part = (time.saturating_sub(a.time)) as f32 / b.time.saturating_sub(a.time).max(1) as f32;
    if spline.flags & CATMULL_ROM == 0 {
        return std::array::from_fn(|axis| a.position[axis] + (b.position[axis] - a.position[axis]) * part);
    }
    // The points of control beyond each end, as AzerothCore makes them (`InitCatmullRom`): a
    // cyclic spline goes on round; else a yard back from the first point, the way it heads, and
    // the last point again.
    let cyclic = cyclic(spline);
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

/// The milliseconds between the two points of a spline its way is taken from.
const STEP: u32 = 50;

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
