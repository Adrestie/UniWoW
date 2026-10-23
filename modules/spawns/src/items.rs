//! The changes of the spawns listed for the user in the panel *World database*, each a line: what
//! changes of which spawn or path, before and after; taken away one by one (`spawns.discard`), with
//! those that need it, one undo entry.

use std::collections::BTreeSet;

use uniwow_api::scene::{EntityId, Kind};
use uniwow_api::world_db::Item;

use crate::data::{Changes, Link, Spawn};
use crate::patrols::{Patrols, Point, Walker};

/// What reads the names of the spawns as the database has them.
pub trait Names {
    fn spawn(&self, id: &EntityId) -> Option<Spawn>;
}

fn place(position: [f32; 3], orientation: f32) -> String {
    format!(
        "{:.1}, {:.1}, {:.1} facing {:.0}°",
        position[0],
        position[1],
        position[2],
        orientation.to_degrees()
    )
}

fn movement(walker: Walker) -> String {
    match (walker.movement, walker.path) {
        (0, _) => "stands still".to_owned(),
        (1, _) => format!("wanders within {:.1} yd", walker.wander_yards()),
        (_, 0) => "walks no path".to_owned(),
        (_, path) => format!("walks the path {path}"),
    }
}

fn points(points: Option<&Vec<Point>>) -> String {
    match points {
        None => "no path".to_owned(),
        Some(points) => format!("{} points", points.len()),
    }
}

/// The points of `before` and `after` that differ, one a line.
fn points_detail(before: Option<&Vec<Point>>, after: Option<&Vec<Point>>) -> String {
    let (before, after) = (before.cloned().unwrap_or_default(), after.cloned().unwrap_or_default());
    let shown = |point: Option<&Point>| {
        point.map_or("—".to_owned(), |point| {
            format!(
                "{:.1}, {:.1}, {:.1}{}{}",
                point.position[0],
                point.position[1],
                point.position[2],
                if point.delay > 0 {
                    format!(", waits {} ms", point.delay)
                } else {
                    String::new()
                },
                if point.run { ", runs" } else { "" }
            )
        })
    };
    (0..before.len().max(after.len()))
        .filter(|at| before.get(*at) != after.get(*at))
        .map(|at| format!("point {}: {} → {}", at + 1, shown(before.get(at)), shown(after.get(at))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The changes of `changes` as lines, `base` the patrols as the database has them, `names` the
/// spawns as it has them.
pub fn items(changes: &Changes, base: &Patrols, names: &dyn Names) -> Vec<Item> {
    let name = |id: &EntityId| {
        changes
            .added
            .get(id)
            .cloned()
            .or_else(|| names.spawn(id))
            .map_or(format!("{id}"), |spawn| format!("{} {}", spawn.name, id.key))
    };
    let mut out = Vec::new();
    let mut moved: Vec<&EntityId> = changes.moved.keys().collect();
    moved.sort();
    for id in moved {
        if changes.added.contains_key(id) || changes.deleted.contains(id) {
            continue;
        }
        let (position, orientation) = changes.moved[id];
        let what = if id.kind == Kind::Creature {
            "spawn point moved"
        } else {
            "moved"
        };
        out.push(Item {
            key: format!("moved:{id}"),
            text: format!("{}: {what}", name(id)),
            before: names
                .spawn(id)
                .map_or("—".to_owned(), |spawn| place(spawn.position, spawn.orientation)),
            after: place(position, orientation),
            detail: String::new(),
            select: Some(id.to_string()),
            with: Vec::new(),
        });
    }
    let mut added: Vec<&EntityId> = changes.added.keys().collect();
    added.sort();
    for id in added {
        if changes.deleted.contains(id) {
            continue;
        }
        let spawn = &changes.added[id];
        let source = changes
            .sources
            .get(id)
            .map(|source| format!(", a copy of {}", name(source)));
        out.push(Item {
            key: format!("added:{id}"),
            text: format!("{}: made{}", name(id), source.unwrap_or_default()),
            before: "none".to_owned(),
            after: place(spawn.position, spawn.orientation),
            detail: String::new(),
            select: Some(id.to_string()),
            with: Vec::new(),
        });
    }
    let mut deleted: Vec<&EntityId> = changes.deleted.iter().collect();
    deleted.sort();
    for id in deleted {
        if changes.added.contains_key(id) {
            continue;
        }
        out.push(Item {
            key: format!("deleted:{id}"),
            text: format!("{}: deleted", name(id)),
            before: names
                .spawn(id)
                .map_or("—".to_owned(), |spawn| place(spawn.position, spawn.orientation)),
            after: "none".to_owned(),
            detail: String::new(),
            select: Some(id.to_string()),
            with: Vec::new(),
        });
    }
    let link_text = |link: &Link, given: bool| {
        let kind = if link.kind == Kind::GameObject {
            "game object"
        } else {
            "creature"
        };
        format!(
            "Template of {kind} {}: quest {} {} ({} it)",
            link.entry,
            link.quest,
            if given { "given" } else { "taken" },
            if link.ends { "ends" } else { "starts" }
        )
    };
    for (link, given) in changes
        .linked
        .iter()
        .map(|link| (link, true))
        .chain(changes.unlinked.iter().map(|link| (link, false)))
    {
        out.push(Item {
            key: format!(
                "link:{}:{}:{}:{}",
                if link.kind == Kind::GameObject {
                    "gameobject"
                } else {
                    "creature"
                },
                link.entry,
                link.quest,
                u8::from(link.ends)
            ),
            text: link_text(link, given),
            before: if given {
                "not given".to_owned()
            } else {
                "given".to_owned()
            },
            after: if given {
                "given".to_owned()
            } else {
                "not given".to_owned()
            },
            detail: String::new(),
            select: None,
            with: Vec::new(),
        });
    }
    for (entry, template) in &changes.templates {
        out.push(Item {
            key: format!("template:{entry}"),
            text: format!("Template of game object {entry} made: {}", template.name),
            before: "none".to_owned(),
            after: format!("display {}, size {:.2}", template.display, template.size),
            detail: String::new(),
            select: None,
            with: Vec::new(),
        });
    }
    let now = changes.patrols.laid_over(base);
    // A path made in the editor and the movements naming it: one needs the other.
    let made_paths: BTreeSet<u32> = changes
        .patrols
        .paths
        .keys()
        .copied()
        .filter(|path| !base.paths.contains_key(path))
        .collect();
    for (path, points_now) in &changes.patrols.paths {
        let before = base.paths.get(path);
        if before == points_now.as_ref() {
            continue;
        }
        let walkers = now.walkers_of(*path);
        let who = walkers
            .first()
            .map_or(String::new(), |guid| format!("{}: ", name(&EntityId::creature(*guid))));
        let with = if made_paths.contains(path) {
            changes
                .patrols
                .walkers
                .iter()
                .filter(|(_, walker)| walker.path == *path)
                .map(|(guid, _)| format!("walker:{guid}"))
                .collect()
        } else {
            Vec::new()
        };
        out.push(Item {
            key: format!("path:{path}"),
            text: format!(
                "{who}patrol path {path} {}",
                match (before, points_now) {
                    (None, _) => "made",
                    (_, None) => "taken away",
                    _ => "changed",
                }
            ),
            before: points(before),
            after: points(points_now.as_ref()),
            detail: points_detail(before, points_now.as_ref()),
            select: Some(
                walkers
                    .first()
                    .map_or(EntityId::waypoint(*path, 1), |guid| EntityId::creature(*guid))
                    .to_string(),
            ),
            with,
        });
    }
    for (guid, walker) in &changes.patrols.walkers {
        let before = base.walkers.get(guid).copied().unwrap_or_default();
        if before == *walker {
            continue;
        }
        let id = EntityId::creature(*guid);
        let with = if made_paths.contains(&walker.path) {
            vec![format!("path:{}", walker.path)]
        } else {
            Vec::new()
        };
        out.push(Item {
            key: format!("walker:{guid}"),
            text: format!("{}: how it moves", name(&id)),
            before: movement(before),
            after: movement(*walker),
            detail: String::new(),
            select: Some(id.to_string()),
            with,
        });
    }
    for (leader, group) in &changes.patrols.groups {
        let before = base.groups.get(leader);
        if before == group.as_ref() {
            continue;
        }
        let id = EntityId::creature(*leader);
        let count = |group: Option<&(Vec<crate::patrols::Member>, u16)>| {
            group.map_or("no group".to_owned(), |(members, _)| format!("leads {}", members.len()))
        };
        let detail = group
            .iter()
            .flat_map(|(members, _)| members)
            .map(|member| {
                format!(
                    "{}: {:.1} yd, {:.0}°",
                    name(&EntityId::creature(member.guid)),
                    member.distance,
                    member.angle
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Its members made to stand, to follow it: taken back with it.
        let with = group
            .iter()
            .flat_map(|(members, _)| members)
            .filter(|member| changes.patrols.walkers.contains_key(&member.guid))
            .map(|member| format!("walker:{}", member.guid))
            .collect();
        out.push(Item {
            key: format!("group:{leader}"),
            text: format!("{}: its group", name(&id)),
            before: count(before),
            after: count(group.as_ref()),
            detail,
            select: Some(id.to_string()),
            with,
        });
    }
    out
}

/// `changes` with the changes of `keys` taken away.
pub fn discarded(changes: &Changes, keys: &[String]) -> Changes {
    let mut out = changes.clone();
    for key in keys {
        let Some((what, rest)) = key.split_once(':') else {
            continue;
        };
        let id = || rest.parse::<EntityId>().ok();
        match what {
            "moved" => {
                if let Some(id) = id() {
                    out.moved.remove(&id);
                }
            }
            "added" => {
                if let Some(id) = id() {
                    out.added.remove(&id);
                    out.moved.remove(&id);
                    out.sources.remove(&id);
                }
            }
            "deleted" => {
                if let Some(id) = id() {
                    out.deleted.remove(&id);
                }
            }
            "link" => {
                let parts: Vec<&str> = rest.split(':').collect();
                if let [kind, entry, quest, ends] = parts[..]
                    && let (Ok(entry), Ok(quest)) = (entry.parse(), quest.parse())
                {
                    let link = Link {
                        kind: if kind == "gameobject" {
                            Kind::GameObject
                        } else {
                            Kind::Creature
                        },
                        entry,
                        quest,
                        ends: ends == "1",
                    };
                    out.linked.remove(&link);
                    out.unlinked.remove(&link);
                }
            }
            "template" => {
                if let Ok(entry) = rest.parse() {
                    out.templates.remove(&entry);
                }
            }
            "path" => {
                if let Ok(path) = rest.parse() {
                    out.patrols.paths.remove(&path);
                }
            }
            "walker" => {
                if let Ok(guid) = rest.parse() {
                    out.patrols.walkers.remove(&guid);
                }
            }
            "group" => {
                if let Ok(leader) = rest.parse() {
                    out.patrols.groups.remove(&leader);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_change_taken_away_leaves_the_changes_as_the_database_has_them() {
        let mut base = Patrols::default();
        base.paths
            .insert(790, vec![Point::at([0.0, 0.0, 0.0]), Point::at([10.0, 0.0, 0.0])]);
        base.walkers.insert(
            79,
            Walker {
                movement: 2,
                path: 790,
                wander: 0,
            },
        );
        let only = |change: &dyn Fn(&mut Changes)| {
            let mut changes = Changes::default();
            change(&mut changes);
            changes
        };
        let cases: Vec<(&str, Changes)> = vec![
            (
                "spawn",
                only(&|changes| {
                    changes.moved.insert(EntityId::creature(79), ([1.0, 2.0, 3.0], 0.5));
                }),
            ),
            (
                "point",
                only(&|changes| {
                    let mut points = base.paths[&790].clone();
                    points[1].position = [12.0, 3.0, 0.0];
                    changes.patrols.paths.insert(790, Some(points));
                }),
            ),
            (
                "path",
                only(&|changes| {
                    changes
                        .patrols
                        .paths
                        .insert(800, Some(vec![Point::at([5.0, 5.0, 0.0])]));
                }),
            ),
            (
                "movement",
                only(&|changes| {
                    changes.patrols.walkers.insert(79, Walker::default());
                }),
            ),
            (
                "group",
                only(&|changes| {
                    let member = crate::patrols::Member {
                        guid: 80,
                        distance: 3.0,
                        angle: 180.0,
                    };
                    changes
                        .patrols
                        .groups
                        .insert(79, Some((vec![member], crate::patrols::GROUP_AI)));
                    changes.patrols.walkers.insert(80, Walker::default());
                }),
            ),
        ];
        for (kind, changes) in cases {
            let found = items(&changes, &base, &None_);
            assert!(!found.is_empty(), "{kind}: listed");
            let keys: Vec<String> = found
                .iter()
                .flat_map(|item| std::iter::once(item.key.clone()).chain(item.with.iter().cloned()))
                .collect();
            // The first line and those that need it taken away: nothing left of that change.
            let mut first = vec![found[0].key.clone()];
            first.extend(found[0].with.iter().cloned());
            let left = discarded(&changes, &first);
            let rest: Vec<String> = keys.iter().filter(|key| !first.contains(key)).cloned().collect();
            assert_eq!(
                discarded(&left, &rest),
                Changes::default(),
                "{kind}: as the database has it"
            );
            assert!(
                items(&discarded(&changes, &keys), &base, &None_).is_empty(),
                "{kind}: no line left"
            );
        }
    }

    struct None_;

    impl Names for None_ {
        fn spawn(&self, _: &EntityId) -> Option<Spawn> {
            None
        }
    }

    #[test]
    fn each_change_is_a_line_taken_away_alone_or_with_those_that_need_it() {
        let mut base = Patrols::default();
        base.paths
            .insert(790, vec![Point::at([0.0, 0.0, 0.0]), Point::at([10.0, 0.0, 0.0])]);
        base.walkers.insert(
            79,
            Walker {
                movement: 2,
                path: 790,
                wander: 0,
            },
        );
        let mut changes = Changes::default();
        changes.moved.insert(EntityId::creature(79), ([1.0, 2.0, 3.0], 0.0));
        // A point inserted, then moved: one change of the path.
        let mut points = base.paths[&790].clone();
        points.insert(1, Point::at([6.0, 1.0, 0.0]));
        changes.patrols.paths.insert(790, Some(points));
        // A patrol made: its path and the movement naming it need each other.
        changes
            .patrols
            .paths
            .insert(800, Some(vec![Point::at([5.0, 5.0, 0.0])]));
        changes.patrols.walkers.insert(
            80,
            Walker {
                movement: 2,
                path: 800,
                wander: 0,
            },
        );
        let found = items(&changes, &base, &None_);
        let keys: Vec<&str> = found.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(keys, ["moved:creature:79", "path:790", "path:800", "walker:80"]);
        assert_eq!(
            (found[1].before.as_str(), found[1].after.as_str()),
            ("2 points", "3 points")
        );
        assert!(
            found[1].detail.contains("point 2: 10.0, 0.0, 0.0 → 6.0, 1.0, 0.0"),
            "{}",
            found[1].detail
        );
        assert_eq!(found[1].select.as_deref(), Some("creature:79"));
        assert_eq!(found[2].with, ["walker:80"]);
        assert_eq!(found[3].with, ["path:800"]);

        let left = discarded(&changes, &["path:800".to_owned(), "walker:80".to_owned()]);
        assert!(left.patrols.walkers.is_empty() && !left.patrols.paths.contains_key(&800));
        assert!(left.moved.contains_key(&EntityId::creature(79)), "the others kept");
        let left = discarded(&left, &["moved:creature:79".to_owned(), "path:790".to_owned()]);
        assert_eq!(left, Changes::default());
    }
}
