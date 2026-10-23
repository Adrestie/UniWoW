//! The changes not applied yet seen in the game at once: the spawn points of the creatures moved and
//! the patrols changed set in the memory of the server running through its bridge
//! (`world-db.set_live`), nothing written to its database until *Apply to the server*. What the
//! server holds follows the changes as they are made, undone and discarded: a change taken back, the
//! server loads again what its database has. Lost when the server starts again: set again then.

use std::collections::BTreeMap;

use uniwow_api::scene::Kind;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::theme::Level;
use uniwow_api::world_db::Reload;
use uniwow_api::{Context, egui};

use crate::SpawnsModule;
use crate::data::Changes;
use crate::patrols::{Member, Patrols, Point, Walker};

/// What the server holds in its memory that its database does not: the spawn points of creatures
/// (a place and an orientation), the points of paths, the movements of creatures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Held {
    pub homes: BTreeMap<u64, ([f32; 3], f32)>,
    pub paths: BTreeMap<u32, Vec<Point>>,
    pub walkers: BTreeMap<u64, Walker>,
    /// The groups by leader: their members and behaviour, none for a group broken.
    pub groups: BTreeMap<u64, Option<Group>>,
}

/// A group: its members, its leader left out, and its behaviour.
pub type Group = (Vec<Member>, u16);

impl Held {
    pub fn is_empty(&self) -> bool {
        self.homes.is_empty() && self.paths.is_empty() && self.walkers.is_empty() && self.groups.is_empty()
    }
}

/// What `changes` make the server hold, its database's patrols `base`: the creatures of the database
/// moved, the paths changed, the movements changed; those made or deleted in the editor wait for
/// Apply.
pub fn wanted(changes: &Changes, base: &Patrols) -> Held {
    let of_database = |kind: Kind, guid: u64| {
        let same = |id: &&uniwow_api::scene::EntityId| id.kind == kind && id.key == guid;
        !changes.added.keys().any(|id| same(&id)) && !changes.deleted.iter().any(|id| same(&id))
    };
    Held {
        homes: changes
            .moved
            .iter()
            .filter(|(id, _)| id.kind == Kind::Creature && of_database(Kind::Creature, id.key))
            .map(|(id, place)| (id.key, *place))
            .collect(),
        paths: changes
            .patrols
            .paths
            .iter()
            .filter(|(id, points)| base.paths.get(id) != points.as_ref())
            .map(|(id, points)| (*id, points.clone().unwrap_or_default()))
            .collect(),
        walkers: changes
            .patrols
            .walkers
            .iter()
            .filter(|(guid, walker)| {
                base.walkers.get(guid).copied().unwrap_or_default() != **walker && of_database(Kind::Creature, **guid)
            })
            .map(|(guid, walker)| (*guid, *walker))
            .collect(),
        groups: changes
            .patrols
            .groups
            .iter()
            .filter(|(leader, group)| base.groups.get(leader) != group.as_ref())
            .map(|(leader, group)| (*leader, group.clone()))
            .collect(),
    }
}

/// The group of `leader` as the bridge sets it: its members, the leader first, none breaking it.
fn group_json(leader: u64, group: Option<&Group>) -> Value {
    let members: Vec<Value> = group
        .into_iter()
        .flat_map(|(members, ai)| {
            std::iter::once(json!({ "guid": leader, "distance": 0.0, "angle": 0.0, "ai": ai })).chain(members.iter().map(
                move |member| json!({ "guid": member.guid, "distance": member.distance, "angle": member.angle, "ai": ai }),
            ))
        })
        .collect();
    json!({ "group": leader, "members": members })
}

fn point_json(point: &Point) -> Value {
    json!({ "position": point.position, "orientation": point.orientation, "delay": point.delay, "run": point.run })
}

/// What the server is sent for it to hold `now` in place of `before`, its database's patrols `base`:
/// the changes to set, then what it loads again from its database.
pub fn sent(before: &Held, now: &Held, base: &Patrols) -> (Vec<Value>, Vec<Reload>) {
    let (mut sets, mut reloads) = (Vec::new(), Vec::new());
    for (guid, (position, orientation)) in &now.homes {
        if before.homes.get(guid) != Some(&(*position, *orientation)) {
            sets.push(json!({ "home": guid, "position": position, "orientation": orientation }));
        }
    }
    reloads.extend(
        before
            .homes
            .keys()
            .filter(|guid| !now.homes.contains_key(guid))
            .map(|guid| Reload::Home(*guid)),
    );
    for (id, points) in &now.paths {
        if before.paths.get(id) != Some(points) {
            sets.push(json!({ "path": id, "points": points.iter().map(point_json).collect::<Vec<_>>() }));
        }
    }
    reloads.extend(
        before
            .paths
            .keys()
            .filter(|id| !now.paths.contains_key(id))
            .map(|id| Reload::Path(*id)),
    );
    // The movements after the paths they name.
    // A movement, and how far it wanders once that changed, apart: a bridge without the set of the
    // distance takes the movement still.
    let walker_sets = |sets: &mut Vec<Value>, guid: u64, walker: Walker, before: Option<Walker>| {
        // What the server holds now: what was set, else the database's.
        let held = before.unwrap_or_else(|| base.walkers.get(&guid).copied().unwrap_or_default());
        if before.is_none() || held.movement != walker.movement || held.path != walker.path {
            sets.push(json!({ "walker": guid, "movement": walker.movement, "path": walker.path }));
        }
        if held.wander != walker.wander {
            sets.push(json!({ "wander": guid, "distance": walker.wander_yards() }));
        }
    };
    for (guid, walker) in &now.walkers {
        if before.walkers.get(guid) != Some(walker) {
            walker_sets(&mut sets, *guid, *walker, before.walkers.get(guid).copied());
        }
    }
    for (guid, held) in before
        .walkers
        .iter()
        .filter(|(guid, _)| !now.walkers.contains_key(guid))
    {
        let walker = base.walkers.get(guid).copied().unwrap_or_default();
        walker_sets(&mut sets, *guid, walker, Some(*held));
    }
    // The groups after the movements: their members stand, following.
    for (leader, group) in &now.groups {
        if before.groups.get(leader) != Some(group) {
            sets.push(group_json(*leader, group.as_ref()));
        }
    }
    for leader in before.groups.keys().filter(|leader| !now.groups.contains_key(leader)) {
        sets.push(group_json(*leader, base.groups.get(leader)));
    }
    (sets, reloads)
}

/// What the module keeps of the server's memory: what it holds, and the changes it was set for.
#[derive(Default)]
pub struct Mirror {
    pub held: Held,
    pub synced: Option<u64>,
}

impl SpawnsModule {
    /// The server set to hold the changes as they stand, once they changed and no button of the
    /// pointer is held: a gizmo let go, a point added, a change undone.
    pub(crate) fn mirror(&mut self, egui: &egui::Context, ctx: &mut Context) {
        if self.mirror.synced == Some(self.generation) || egui.input(|i| i.pointer.any_down()) {
            return;
        }
        self.mirror.synced = Some(self.generation);
        let now = wanted(&self.changes, &self.patrols);
        if now == self.mirror.held {
            return;
        }
        let (sets, reloads) = sent(&self.mirror.held, &now, &self.patrols);
        ctx.call(
            "world-db.set_live",
            json!({
                "sets": sets,
                "reloads": reloads.iter().map(Reload::to_json).collect::<Vec<_>>(),
                "watch": !now.is_empty(),
            }),
        );
        self.mirror.held = now;
    }

    /// The server's memory as its database has it: the changes applied, or another server chosen.
    pub(crate) fn mirror_forgotten(&mut self) {
        self.mirror = Mirror::default();
    }

    /// The server started again, its memory lost: the changes not applied set again, the user told.
    pub(crate) fn mirror_restarted(&mut self, ctx: &mut Context) {
        let lost = !self.mirror.held.is_empty();
        self.mirror = Mirror::default();
        if lost {
            ctx.notify(
                Level::Warning,
                "The server was started again: the spawn points and patrols not applied, lost with its memory, are set again in the game",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn how_far_a_creature_wanders_is_set_apart_from_its_movement() {
        let mut base = Patrols::default();
        base.walkers.insert(
            55155,
            Walker {
                movement: 1,
                path: 0,
                wander: 50,
            },
        );
        let mut changes = Changes::default();
        changes.patrols.walkers.insert(
            55155,
            Walker {
                movement: 1,
                path: 0,
                wander: 82,
            },
        );
        let now = wanted(&changes, &base);
        let (sets, _) = sent(&Held::default(), &now, &base);
        assert_eq!(sets.len(), 2, "{sets:?}");
        assert_eq!(sets[1]["wander"], 55155);
        assert!((sets[1]["distance"].as_f64().unwrap() - 8.2).abs() < 1e-4);
        // Taken back: the database's distance set again.
        let (sets, _) = sent(&now, &Held::default(), &base);
        assert_eq!(
            sets,
            [json!({ "wander": 55155, "distance": 5.0 })],
            "its movement unchanged, not sent"
        );
    }
    use uniwow_api::scene::EntityId;

    #[test]
    fn the_server_holds_the_changes_not_applied_and_loads_again_those_taken_back() {
        let mut base = Patrols::default();
        base.paths.insert(790, vec![Point::at([0.0, 0.0, 0.0])]);
        let mut changes = Changes::default();
        changes.moved.insert(EntityId::creature(79), ([1.0, 2.0, 3.0], 0.5));
        changes.moved.insert(EntityId::game_object(5), ([1.0, 1.0, 1.0], 0.0));
        let made = EntityId::creature(900_000);
        changes.moved.insert(made, ([4.0, 5.0, 6.0], 0.0));
        changes.deleted.insert(made);
        let points = vec![Point::at([1.0, 0.0, 0.0]), Point::at([2.0, 0.0, 0.0])];
        changes.patrols.paths.insert(790, Some(points.clone()));
        changes.patrols.walkers.insert(
            80,
            Walker {
                movement: 2,
                path: 790,
                wander: 0,
            },
        );
        let now = wanted(&changes, &base);
        assert_eq!(
            now.homes.keys().collect::<Vec<_>>(),
            [&79],
            "a game object, one deleted: Apply"
        );
        assert_eq!(now.paths[&790], points);

        let (sets, reloads) = sent(&Held::default(), &now, &base);
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0]["home"], 79);
        assert_eq!(sets[1]["path"], 790);
        assert_eq!(sets[2]["walker"], 80, "the movement after the path it names");
        assert!(reloads.is_empty());

        // All undone: the database's spawn point and path loaded again, the movement set back.
        let (sets, reloads) = sent(&now, &Held::default(), &base);
        assert_eq!(reloads, [Reload::Home(79), Reload::Path(790)]);
        assert_eq!(sets, [json!({ "walker": 80, "movement": 0, "path": 0 })]);
        assert_eq!(
            sent(&now, &now, &base),
            (Vec::new(), Vec::new()),
            "nothing changed, nothing sent"
        );

        // A group made: its leader first among its members; broken back to the database's, none.
        let member = crate::patrols::Member {
            guid: 81,
            distance: 3.0,
            angle: 180.0,
        };
        changes.patrols.groups.insert(80, Some((vec![member], 0x203)));
        let grouped = wanted(&changes, &base);
        let (sets, _) = sent(&now, &grouped, &base);
        assert_eq!(
            sets,
            [json!({ "group": 80, "members": [
                { "guid": 80, "distance": 0.0, "angle": 0.0, "ai": 0x203 },
                { "guid": 81, "distance": 3.0, "angle": 180.0, "ai": 0x203 },
            ] })]
        );
        let (sets, _) = sent(&grouped, &now, &base);
        assert_eq!(
            sets,
            [json!({ "group": 80, "members": [] })],
            "no group in the database"
        );
    }
}
