//! The patrols of the creatures (step 16.4): the path a creature walks (`waypoint_data`, named by its
//! `creature_addon`, its `MovementType` 2), and the groups that follow a leader
//! (`creature_formations`); read with the spawns, changed in memory with one undo entry a change,
//! kept in the project, given to the server as rows. A point of a path is an entity of its own,
//! `waypoint:<path>:<point>`, moved by the gizmos, read and written by the Inspector.

use std::collections::{BTreeMap, BTreeSet};

use uniwow_api::db::Rows;
use uniwow_api::db::write::{RowChange, row};
use uniwow_api::scene::{self, Entity, EntityId, Piece, PieceBuilder, flags};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::Reload;

/// The creatures of a map that walk a path, or whose addon names one: guid, movement, path.
pub const WALKERS: &str = "SELECT c.guid, c.MovementType, a.path_id FROM creature c \
     LEFT JOIN creature_addon a ON a.guid = c.guid \
     WHERE c.map = ? AND (c.MovementType = 2 OR (a.path_id IS NOT NULL AND a.path_id <> 0))";

/// The points of the paths the addons of the creatures of a map name, in order.
pub const POINTS: &str = "SELECT w.id, w.point, w.position_x, w.position_y, w.position_z, w.orientation, \
     w.delay, w.move_type FROM waypoint_data w WHERE w.id IN (SELECT a.path_id FROM creature_addon a \
     JOIN creature c ON c.guid = a.guid WHERE c.map = ? AND a.path_id <> 0) ORDER BY w.id, w.point";

/// The groups led by the creatures of a map: leader, member, distance, angle, behaviour.
pub const GROUPS: &str = "SELECT f.leaderGUID, f.memberGUID, f.dist, f.angle, f.groupAI \
     FROM creature_formations f JOIN creature c ON c.guid = f.leaderGUID WHERE c.map = ?";

/// The movement of a creature that walks its path (`MovementType`); one that stands, idle.
pub const WALKING: u8 = 2;
pub const IDLE: u8 = 0;

/// The behaviour of a group made by the editor: the members help the leader and he them
/// (`GROUP_AI_FLAG_MEMBER_ASSIST_LEADER | GROUP_AI_FLAG_LEADER_ASSIST_MEMBER`), and they follow him
/// (`GROUP_AI_FLAG_FOLLOW_LEADER`).
pub const GROUP_AI: u16 = 0x003 | 0x200;

/// A point of a path: where, the way it faces there if it turns, how long it waits there (ms), and
/// whether it runs to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub position: [f32; 3],
    pub orientation: Option<f32>,
    pub delay: u32,
    pub run: bool,
}

impl Point {
    pub fn at(position: [f32; 3]) -> Self {
        Self {
            position,
            orientation: None,
            delay: 0,
            run: false,
        }
    }
}

/// How a creature moves: its `MovementType` and the path its addon names, 0 for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Walker {
    pub movement: u8,
    pub path: u32,
}

/// A creature following a leader: its guid, how far and at what angle (degrees) behind him.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub guid: u64,
    pub distance: f32,
    pub angle: f32,
}

/// The patrols as the database has them for the creatures of a map, or as the user changed them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patrols {
    pub paths: BTreeMap<u32, Vec<Point>>,
    pub walkers: BTreeMap<u64, Walker>,
    /// The groups by leader: their members, the leader left out, and their behaviour.
    pub groups: BTreeMap<u64, (Vec<Member>, u16)>,
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

impl Patrols {
    /// The patrols of the rows of `WALKERS`, `POINTS` and `GROUPS`.
    pub fn of_rows(walkers: &Rows, points: &Rows, groups: &Rows) -> Self {
        let mut out = Patrols::default();
        for row in &walkers.rows {
            let Some(guid) = number(row.first()) else {
                continue;
            };
            out.walkers.insert(
                guid as u64,
                Walker {
                    movement: number(row.get(1)).unwrap_or(0.0) as u8,
                    path: number(row.get(2)).unwrap_or(0.0) as u32,
                },
            );
        }
        let mut read: BTreeMap<u32, Vec<(u32, Point)>> = BTreeMap::new();
        for row in &points.rows {
            let (Some(id), Some(point), Some(x), Some(y), Some(z)) = (
                number(row.first()),
                number(row.get(1)),
                number(row.get(2)),
                number(row.get(3)),
                number(row.get(4)),
            ) else {
                continue;
            };
            read.entry(id as u32).or_default().push((
                point as u32,
                Point {
                    position: [x as f32, y as f32, z as f32],
                    orientation: number(row.get(5)).map(|turn| turn as f32),
                    delay: number(row.get(6)).unwrap_or(0.0) as u32,
                    run: number(row.get(7)).unwrap_or(0.0) as u32 == 1,
                },
            ));
        }
        for (id, mut points) in read {
            points.sort_by_key(|(point, _)| *point);
            out.paths
                .insert(id, points.into_iter().map(|(_, point)| point).collect());
        }
        for row in &groups.rows {
            let (Some(leader), Some(member)) = (number(row.first()), number(row.get(1))) else {
                continue;
            };
            let group = out.groups.entry(leader as u64).or_default();
            group.1 = number(row.get(4)).unwrap_or(0.0) as u16;
            if member as u64 != leader as u64 {
                group.0.push(Member {
                    guid: member as u64,
                    distance: number(row.get(2)).unwrap_or(0.0) as f32,
                    angle: number(row.get(3)).unwrap_or(0.0) as f32,
                });
            }
        }
        out
    }

    /// The leader of the group `guid` belongs to, himself when he leads one.
    pub fn leader_of(&self, guid: u64) -> Option<u64> {
        if self.groups.contains_key(&guid) {
            return Some(guid);
        }
        self.groups
            .iter()
            .find(|(_, (members, _))| members.iter().any(|member| member.guid == guid))
            .map(|(leader, _)| *leader)
    }

    /// The creatures that walk the path `path`.
    pub fn walkers_of(&self, path: u32) -> Vec<u64> {
        self.walkers
            .iter()
            .filter(|(_, walker)| walker.path == path)
            .map(|(guid, _)| *guid)
            .collect()
    }
}

/// What the user changed of the patrols: each path, creature and group as it is now, none once
/// taken away; laid over those of the database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edits {
    pub paths: BTreeMap<u32, Option<Vec<Point>>>,
    pub walkers: BTreeMap<u64, Walker>,
    pub groups: BTreeMap<u64, Option<(Vec<Member>, u16)>>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.walkers.is_empty() && self.groups.is_empty()
    }

    /// The creatures these changes name: those whose movement changed, the leaders and members of
    /// the groups changed.
    pub fn creatures(&self) -> BTreeSet<u64> {
        let mut out: BTreeSet<u64> = self.walkers.keys().copied().collect();
        for (leader, group) in &self.groups {
            out.insert(*leader);
            out.extend(
                group
                    .iter()
                    .flat_map(|(members, _)| members.iter().map(|member| member.guid)),
            );
        }
        out
    }

    /// `base` as the user changed it.
    pub fn laid_over(&self, base: &Patrols) -> Patrols {
        let mut out = base.clone();
        for (id, path) in &self.paths {
            match path {
                Some(points) => {
                    out.paths.insert(*id, points.clone());
                }
                None => {
                    out.paths.remove(id);
                }
            }
        }
        for (guid, walker) in &self.walkers {
            out.walkers.insert(*guid, *walker);
        }
        for (leader, group) in &self.groups {
            match group {
                Some(group) => {
                    out.groups.insert(*leader, group.clone());
                }
                None => {
                    out.groups.remove(leader);
                }
            }
        }
        out
    }

    /// The rows of what the user changed of `base`, and what the server loads again: a path
    /// changed its points written again; a creature its movement and its addon's path; a group its
    /// rows written again, its members taken out of any other first.
    pub fn rows(&self, base: &Patrols) -> (Vec<RowChange>, Vec<Reload>) {
        let (mut changes, mut reloads) = (Vec::new(), BTreeSet::new());
        for (id, path) in &self.paths {
            if base.paths.get(id) == path.as_ref() {
                continue;
            }
            let key = row([("id", json!(id))]);
            changes.push(RowChange::Delete {
                table: "waypoint_data".to_owned(),
                key: key.clone(),
            });
            if let Some(points) = path.as_ref().filter(|points| !points.is_empty()) {
                let rows = points
                    .iter()
                    .enumerate()
                    .map(|(at, point)| {
                        row([
                            ("id", json!(id)),
                            ("point", json!(at + 1)),
                            ("position_x", json!(point.position[0])),
                            ("position_y", json!(point.position[1])),
                            ("position_z", json!(point.position[2])),
                            ("orientation", point.orientation.map_or(Value::Null, |turn| json!(turn))),
                            ("delay", json!(point.delay)),
                            ("move_type", json!(u8::from(point.run))),
                            ("action", json!(0)),
                            ("action_chance", json!(100)),
                            ("wpguid", json!(0)),
                        ])
                    })
                    .collect();
                changes.push(RowChange::Insert {
                    table: "waypoint_data".to_owned(),
                    key,
                    rows,
                });
            }
            // The creatures walking it set on it again by the bridge, where they stand.
            reloads.insert(Reload::Path(*id));
        }
        for (guid, walker) in &self.walkers {
            let before = base.walkers.get(guid).copied().unwrap_or_default();
            if before == *walker {
                continue;
            }
            let key = row([("guid", json!(guid))]);
            if before.movement != walker.movement {
                let mut set = row([("MovementType", json!(walker.movement))]);
                if walker.movement == WALKING {
                    set.insert("wander_distance".to_owned(), json!(0));
                }
                changes.push(RowChange::Put {
                    table: "creature".to_owned(),
                    key: key.clone(),
                    set,
                    from: None,
                });
            }
            if before.path != walker.path {
                changes.push(RowChange::Put {
                    table: "creature_addon".to_owned(),
                    key,
                    set: row([("path_id", json!(walker.path))]),
                    from: None,
                });
                reloads.insert(Reload::Addons);
            }
            reloads.insert(Reload::Creature(*guid));
        }
        for (leader, group) in &self.groups {
            let before = base.groups.get(leader);
            if before == group.as_ref() {
                continue;
            }
            // Its rows and those of its members anywhere, the member being the key of the table.
            changes.push(RowChange::Delete {
                table: "creature_formations".to_owned(),
                key: row([("leaderGUID", json!(leader))]),
            });
            let mut guids: BTreeSet<u64> = before
                .iter()
                .flat_map(|(members, _)| members.iter().map(|m| m.guid))
                .collect();
            if let Some((members, ai)) = group {
                let mut rows = vec![row([
                    ("leaderGUID", json!(leader)),
                    ("memberGUID", json!(leader)),
                    ("dist", json!(0)),
                    ("angle", json!(0)),
                    ("groupAI", json!(ai)),
                    ("point_1", json!(0)),
                    ("point_2", json!(0)),
                ])];
                for member in members {
                    changes.push(RowChange::Delete {
                        table: "creature_formations".to_owned(),
                        key: row([("memberGUID", json!(member.guid))]),
                    });
                    guids.insert(member.guid);
                    rows.push(row([
                        ("leaderGUID", json!(leader)),
                        ("memberGUID", json!(member.guid)),
                        ("dist", json!(member.distance)),
                        ("angle", json!(member.angle)),
                        ("groupAI", json!(ai)),
                        ("point_1", json!(0)),
                        ("point_2", json!(0)),
                    ]));
                }
                changes.push(RowChange::Delete {
                    table: "creature_formations".to_owned(),
                    key: row([("memberGUID", json!(leader))]),
                });
                changes.push(RowChange::Insert {
                    table: "creature_formations".to_owned(),
                    key: row([("leaderGUID", json!(leader))]),
                    rows,
                });
            }
            reloads.insert(Reload::Formation(*leader));
            reloads.insert(Reload::Creature(*leader));
            reloads.extend(guids.into_iter().map(Reload::Creature));
        }
        (changes, reloads.into_iter().collect())
    }

    pub fn section(&self) -> Value {
        let point = |point: &Point| json!({ "position": point.position, "orientation": point.orientation, "delay": point.delay, "run": point.run });
        json!({
            "paths": self.paths.iter().map(|(id, path)| json!({
                "id": id,
                "points": path.as_ref().map(|points| points.iter().map(point).collect::<Vec<_>>()),
            })).collect::<Vec<_>>(),
            "walkers": self.walkers.iter().map(|(guid, walker)| json!({
                "guid": guid, "movement": walker.movement, "path": walker.path,
            })).collect::<Vec<_>>(),
            "groups": self.groups.iter().map(|(leader, group)| json!({
                "leader": leader,
                "behaviour": group.as_ref().map(|(_, ai)| ai),
                "members": group.as_ref().map(|(members, _)| members.iter().map(|member| json!({
                    "guid": member.guid, "distance": member.distance, "angle": member.angle,
                })).collect::<Vec<_>>()),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn of_section(section: &Value) -> Self {
        let list = |key: &str| section[key].as_array().cloned().unwrap_or_default();
        let point = |value: &Value| -> Option<Point> {
            let at = value["position"].as_array()?;
            Some(Point {
                position: [0, 1, 2].map(|axis| at.get(axis).and_then(Value::as_f64).unwrap_or(0.0) as f32),
                orientation: value["orientation"].as_f64().map(|turn| turn as f32),
                delay: value["delay"].as_u64().unwrap_or(0) as u32,
                run: value["run"].as_bool().unwrap_or(false),
            })
        };
        Self {
            paths: list("paths")
                .iter()
                .filter_map(|path| {
                    let points = path["points"]
                        .as_array()
                        .map(|points| points.iter().filter_map(point).collect());
                    Some((path["id"].as_u64()? as u32, points))
                })
                .collect(),
            walkers: list("walkers")
                .iter()
                .filter_map(|walker| {
                    Some((
                        walker["guid"].as_u64()?,
                        Walker {
                            movement: walker["movement"].as_u64().unwrap_or(0) as u8,
                            path: walker["path"].as_u64().unwrap_or(0) as u32,
                        },
                    ))
                })
                .collect(),
            groups: list("groups")
                .iter()
                .filter_map(|group| {
                    let members = group["members"].as_array().map(|members| {
                        members
                            .iter()
                            .filter_map(|member| {
                                Some(Member {
                                    guid: member["guid"].as_u64()?,
                                    distance: member["distance"].as_f64().unwrap_or(0.0) as f32,
                                    angle: member["angle"].as_f64().unwrap_or(0.0) as f32,
                                })
                            })
                            .collect()
                    });
                    let behaviour = group["behaviour"].as_u64().unwrap_or(u64::from(GROUP_AI)) as u16;
                    Some((group["leader"].as_u64()?, members.map(|members| (members, behaviour))))
                })
                .collect(),
        }
    }
}

/// The id of the path the editor gives the creature `guid`: its guid × 10, as AzerothCore names
/// them, or the next free of its ten when another path has it already.
pub fn path_for(guid: u64, taken: &Patrols) -> Option<u32> {
    (0..10)
        .map(|plus| guid.checked_mul(10)?.checked_add(plus))
        .filter_map(|id| id.and_then(|id| u32::try_from(id).ok()))
        .find(|id| !taken.paths.contains_key(id) && taken.walkers_of(*id).iter().all(|walker| *walker == guid))
}

/// The members of a group laid out behind the leader: two by two, a few yards apart.
pub fn laid_out(guids: &[u64]) -> Vec<Member> {
    guids
        .iter()
        .enumerate()
        .map(|(at, guid)| {
            let rank = (at / 2 + 1) as f32;
            let side = if at % 2 == 0 { 1.0 } else { -1.0 };
            Member {
                guid: *guid,
                distance: 3.0 * rank,
                angle: 180.0 - side * 25.0,
            }
        })
        .collect()
}

/// The points of the paths `shown` of `patrols` as entities, their path their extra number.
pub fn piece(patrols: &Patrols, shown: &BTreeSet<u32>) -> Option<Piece> {
    let mut builder = PieceBuilder::new(Vec::new());
    for id in shown {
        let Some(points) = patrols.paths.get(id) else {
            continue;
        };
        for (at, point) in points.iter().enumerate() {
            builder.push(Entity {
                id: EntityId::waypoint(*id, at as u32 + 1),
                flags: flags::SERVER,
                parent: None,
                label: "Waypoint",
                extra: *id,
                origin: point.position,
                bounds: scene::box_around(point.position, 0.6),
            });
        }
    }
    (!builder.is_empty()).then(|| builder.build())
}

/// The points of `path` changed as `field` and `change` say for the point `at` (from 0): its
/// position on `axis` or all, its wait, whether it runs, the way it faces.
pub fn written(point: &Point, field: &str, axis: Option<usize>, change: &str) -> Result<Point, String> {
    let mut out = point.clone();
    match field {
        "position" => {
            let axes: Vec<usize> = match axis {
                Some(axis) if axis < 3 => vec![axis],
                Some(axis) => return Err(format!("no axis {axis}")),
                None => vec![0, 1, 2],
            };
            for axis in axes {
                out.position[axis] = scene::changed_value(out.position[axis], change)?;
            }
        }
        "wait (ms)" | "delay" => {
            out.delay = scene::changed_value(point.delay as f32, change)?.max(0.0).round() as u32;
        }
        "run" => {
            out.run = match change.trim() {
                "true" | "1" => true,
                "false" | "0" => false,
                other => return Err(format!("'{other}' is no yes or no")),
            };
        }
        "facing" | "rotation" => {
            if change.trim().is_empty() || change.trim() == "none" {
                out.orientation = None;
            } else {
                let degrees = scene::changed_value(point.orientation.unwrap_or(0.0).to_degrees(), change)?;
                out.orientation = Some(degrees.to_radians().rem_euclid(std::f32::consts::TAU));
            }
        }
        other => return Err(format!("a waypoint has no field '{other}'")),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniwow_api::db::write::{Memory, apply};

    fn rows(columns: &[&str], rows: Vec<Vec<Value>>) -> Rows {
        Rows {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            rows,
        }
    }

    fn base() -> Patrols {
        Patrols::of_rows(
            &rows(
                &["guid", "MovementType", "path_id"],
                vec![vec![json!(79), json!(2), json!(790)]],
            ),
            &rows(
                &["id", "point", "x", "y", "z", "orientation", "delay", "move_type"],
                vec![
                    vec![
                        json!(790),
                        json!(2),
                        json!(2.0),
                        json!(0.0),
                        json!(0.0),
                        Value::Null,
                        json!(0),
                        json!(0),
                    ],
                    vec![
                        json!(790),
                        json!(1),
                        json!(1.0),
                        json!(0.0),
                        json!(0.0),
                        json!(1.5),
                        json!(3000),
                        json!(1),
                    ],
                ],
            ),
            &rows(
                &["leaderGUID", "memberGUID", "dist", "angle", "groupAI"],
                vec![
                    vec![json!(79), json!(79), json!(0), json!(0), json!(3)],
                    vec![json!(79), json!(80), json!(3.0), json!(180.0), json!(3)],
                ],
            ),
        )
    }

    #[test]
    fn the_patrols_are_read_from_their_rows_points_in_order_the_leader_out_of_his_members() {
        let patrols = base();
        assert_eq!(patrols.walkers[&79], Walker { movement: 2, path: 790 });
        let path = &patrols.paths[&790];
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].position, [1.0, 0.0, 0.0], "in the order of their points");
        assert_eq!(
            (path[0].delay, path[0].run, path[0].orientation),
            (3000, true, Some(1.5))
        );
        assert_eq!(path[1].orientation, None);
        assert_eq!(patrols.groups[&79].0.len(), 1);
        assert_eq!(patrols.leader_of(80), Some(79));
        assert_eq!(patrols.leader_of(79), Some(79));
        assert_eq!(patrols.leader_of(81), None);
    }

    #[test]
    fn a_patrol_made_and_a_group_changed_are_the_rows_the_server_is_given() {
        let base = base();
        let mut memory = Memory::default();
        memory.tables.insert(
            "creature".to_owned(),
            vec![
                row([
                    ("guid", json!(79)),
                    ("MovementType", json!(2)),
                    ("wander_distance", json!(0)),
                ]),
                row([
                    ("guid", json!(81)),
                    ("MovementType", json!(1)),
                    ("wander_distance", json!(5)),
                ]),
            ],
        );
        memory.tables.insert(
            "creature_formations".to_owned(),
            vec![
                row([("leaderGUID", json!(79)), ("memberGUID", json!(79))]),
                row([("leaderGUID", json!(79)), ("memberGUID", json!(80))]),
                // 82 in another group, taken out of it to follow 79.
                row([("leaderGUID", json!(90)), ("memberGUID", json!(82))]),
            ],
        );
        let mut edits = Edits::default();
        // A patrol for 81: its path, its movement, its addon.
        let path = path_for(81, &base).unwrap();
        assert_eq!(path, 810);
        edits
            .paths
            .insert(path, Some(vec![Point::at([5.0, 5.0, 0.0]), Point::at([6.0, 5.0, 0.0])]));
        edits.walkers.insert(
            81,
            Walker {
                movement: WALKING,
                path,
            },
        );
        // 79's group: 80 and 82 laid out behind him.
        edits.groups.insert(79, Some((laid_out(&[80, 82]), GROUP_AI)));
        let (changes, reloads) = edits.rows(&base);
        apply(&mut memory, &changes).unwrap();

        let points = &memory.tables["waypoint_data"];
        assert_eq!(points.len(), 2);
        assert_eq!(
            (points[1]["point"].clone(), points[1]["position_x"].clone()),
            (json!(2), json!(6.0))
        );
        assert_eq!(points[0]["orientation"], Value::Null);
        let walker = memory.tables["creature"]
            .iter()
            .find(|row| row["guid"] == json!(81))
            .unwrap();
        assert_eq!(
            (walker["MovementType"].clone(), walker["wander_distance"].clone()),
            (json!(2), json!(0))
        );
        assert_eq!(
            memory.tables["creature_addon"],
            [row([("guid", json!(81)), ("path_id", json!(810))])]
        );
        let mut group: Vec<(Value, Value)> = memory.tables["creature_formations"]
            .iter()
            .map(|row| (row["leaderGUID"].clone(), row["memberGUID"].clone()))
            .collect();
        group.sort_by_key(|pair| format!("{pair:?}"));
        assert_eq!(
            group,
            [(json!(79), json!(79)), (json!(79), json!(80)), (json!(79), json!(82))],
            "82 taken out of the group of 90"
        );
        assert!(reloads.contains(&Reload::Path(810)));
        assert!(reloads.contains(&Reload::Addons));
        assert!(reloads.contains(&Reload::Formation(79)));
        for guid in [79, 80, 81, 82] {
            assert!(reloads.contains(&Reload::Creature(guid)), "{guid}");
        }

        // Unchanged: no row.
        let mut same = Edits::default();
        same.paths.insert(790, Some(base.paths[&790].clone()));
        same.walkers.insert(79, base.walkers[&79]);
        assert!(same.rows(&base).0.is_empty());
        // Kept in the project as they were.
        assert_eq!(Edits::of_section(&edits.section()), edits);
    }

    #[test]
    fn the_points_of_the_paths_shown_are_entities_and_are_written() {
        let patrols = base();
        let piece = piece(&patrols, &BTreeSet::from([790, 1])).unwrap();
        assert_eq!(piece.len(), 2);
        assert_eq!(piece.ids[1], EntityId::waypoint(790, 2));
        assert!(super::piece(&patrols, &BTreeSet::new()).is_none());
        let point = &patrols.paths[&790][1];
        assert_eq!(
            written(point, "position", Some(2), "+3").unwrap().position,
            [2.0, 0.0, 3.0]
        );
        assert_eq!(written(point, "wait (ms)", None, "1500").unwrap().delay, 1500);
        assert!(written(point, "run", None, "true").unwrap().run);
        let facing = written(point, "facing", None, "90").unwrap().orientation.unwrap();
        assert!((facing - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert_eq!(written(point, "facing", None, "none").unwrap().orientation, None);
        assert!(written(point, "colour", None, "1").is_err());
        assert_eq!(path_for(79, &patrols), Some(791), "790 is the path of 79: its next");
    }
}
