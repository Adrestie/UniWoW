//! The patrols in the editor (step 16.4): the points of the paths of the creatures selected given to
//! the scene, written by the gizmos and the Inspector; the panel's section *Patrol* making a patrol,
//! adding its points by clicks on the ground, making a group follow a leader; the paths drawn as the
//! user changed them.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uniwow_api::scene::{self, EntityId, Kind, PieceKey};
use uniwow_api::serde_json::{Value, json};
use uniwow_api::viewport::{Tool, ToolInput};
use uniwow_api::{Context, egui, theme};

use crate::patrols::{self, Edits, Member, Patrols, Point, Walker};
use crate::{MODULE, SpawnsModule, paths};

/// The part of the scene the points of the paths shown are given in.
const WAYPOINTS: u32 = 0;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The points being added to a path by clicks on the ground: the path, the clicks not yet taken,
/// the last point, whether the user stopped.
#[derive(Default)]
pub struct Placing {
    pub path: Option<u32>,
    pub clicks: Vec<[f32; 3]>,
    pub last: Option<[f32; 3]>,
    pub stop: bool,
}

/// The tool adding points: it takes the pointer alone while points are added, a click on the
/// ground a point, Esc or a right click stops.
pub struct PlaceTool {
    pub placing: Arc<Mutex<Placing>>,
    pub scene: scene::Handle,
}

impl Tool for PlaceTool {
    fn claims(&mut self, input: &ToolInput<'_>) -> bool {
        lock(&self.placing).path.is_some() && input.response.hovered()
    }

    fn pointer(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let mut placing = lock(&self.placing);
        if placing.path.is_none() {
            return;
        }
        let response = input.response;
        if response.ctx.input(|i| i.key_pressed(egui::Key::Escape)) || response.secondary_clicked() {
            placing.stop = true;
            return;
        }
        let Some(pointer) = response.hover_pos() else {
            return;
        };
        let ground = input
            .ray_at(pointer)
            .and_then(|ray| Some(ray.at(self.scene.ground(&ray)?).to_array()));
        let colour = egui::Color32::from_rgb(255, 80, 60);
        if let (Some(last), Some(at)) = (placing.last, ground)
            && let (Some(from), Some(to)) = (input.project(last.into()), input.project(at.into()))
        {
            painter.add(egui::Shape::dashed_line(
                &[from, to],
                egui::Stroke::new(1.5, colour),
                6.0,
                4.0,
            ));
        }
        if let Some(at) = ground.and_then(|at| input.project(at.into())) {
            painter.circle_stroke(at, 6.0, egui::Stroke::new(2.0, colour));
        }
        painter.text(
            pointer + egui::vec2(14.0, 14.0),
            egui::Align2::LEFT_TOP,
            "Click: a waypoint · Esc: done",
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
        if response.clicked_by(egui::PointerButton::Primary)
            && let Some(at) = ground
        {
            placing.clicks.push(at);
            placing.last = Some(at);
        }
    }
}

/// The paths of `read` as drawn, those the user changed as `now` has them.
pub fn drawn_paths(read: &paths::Paths, edits: &Edits, now: &Patrols) -> paths::Paths {
    if edits.is_empty() {
        return read.clone();
    }
    let mut out = read.clone();
    let paths: BTreeSet<u32> = edits.paths.keys().copied().collect();
    let touched = now
        .walkers
        .iter()
        .filter(|(guid, walker)| edits.walkers.contains_key(guid) || paths.contains(&walker.path));
    for (guid, walker) in touched {
        match now
            .paths
            .get(&walker.path)
            .filter(|points| walker.path != 0 && points.len() >= 2)
        {
            Some(points) => {
                out.insert(*guid, vec![points.iter().map(|point| point.position).collect()]);
            }
            None => {
                out.remove(guid);
            }
        }
    }
    out
}

/// The way points face as the Inspector shows it: degrees, none for none.
fn facing(point: &Point) -> Value {
    point.orientation.map_or(Value::Null, |turn| json!(turn.to_degrees()))
}

impl SpawnsModule {
    /// The patrols as the user changed them, and the paths drawn, again once the patrols read or
    /// the changes changed.
    pub(crate) fn refresh_patrols(&mut self) {
        let now = Arc::new(self.changes.patrols.laid_over(&self.patrols));
        self.drawn_paths = Arc::new(drawn_paths(&self.paths, &self.changes.patrols, &now));
        self.patrols_now = now;
        self.paths_for = None;
        self.waypoints_for = None;
    }

    /// The point `id` as the user changed it.
    pub(crate) fn waypoint(&self, id: &EntityId) -> Option<Point> {
        (id.kind == Kind::Waypoint)
            .then(|| {
                self.patrols_now
                    .paths
                    .get(&(id.key as u32))?
                    .get(id.sub.checked_sub(1)? as usize)
                    .cloned()
            })
            .flatten()
    }

    /// The points of the paths of the creatures and points selected, and of the path being drawn,
    /// given to the scene; again once the selection or the patrols changed.
    pub(crate) fn show_waypoints(&mut self) {
        let (Some(scene), Some(map)) = (&self.scene, self.map) else {
            return;
        };
        let selection = scene.selection();
        let now = (selection.generation, self.generation);
        if self.waypoints_for == Some(now) {
            return;
        }
        self.waypoints_for = Some(now);
        let mut shown: BTreeSet<u32> = scene
            .groups()
            .expanded(&selection.ids)
            .iter()
            .filter_map(|id| match id.kind {
                Kind::Creature => self.patrols_now.walkers.get(&id.key).map(|walker| walker.path),
                Kind::Waypoint => Some(id.key as u32),
                _ => None,
            })
            .filter(|path| *path != 0)
            .collect();
        shown.extend(lock(&self.place_points).path);
        let key = PieceKey::part(MODULE, map, WAYPOINTS);
        match patrols::piece(&self.patrols_now, &shown) {
            Some(piece) => scene.publish(key, Arc::new(piece)),
            None => scene.withdraw(key),
        }
        self.waypoints_shown = shown;
    }

    /// The clicks on the ground taken as points of the path being drawn, one undo entry for those
    /// of a frame; the drawing stopped once asked.
    pub(crate) fn take_clicks(&mut self, ctx: &mut Context) {
        let (clicks, path, stop) = {
            let mut placing = lock(&self.place_points);
            let stop = std::mem::take(&mut placing.stop);
            if stop {
                placing.path = None;
            }
            (std::mem::take(&mut placing.clicks), placing.path, stop)
        };
        if stop {
            self.waypoints_for = None;
        }
        let Some(path) = path.filter(|_| !clicks.is_empty()) else {
            return;
        };
        let mut after = self.changes.clone();
        let mut points = self.patrols_now.paths.get(&path).cloned().unwrap_or_default();
        points.extend(clicks.iter().map(|at| Point::at(*at)));
        after.patrols.paths.insert(path, Some(points));
        self.change(
            if clicks.len() == 1 {
                "Add a waypoint".to_owned()
            } else {
                format!("Add {} waypoints", clicks.len())
            },
            after,
            ctx,
        );
    }

    /// The commands on points of paths: read, write, duplicate (a copy after each), delete.
    pub(crate) fn waypoint_command(
        &mut self,
        name: &str,
        ids: &[EntityId],
        arguments: &Value,
        ctx: &mut Context,
    ) -> Result<Value, String> {
        match name {
            "spawns.read" => {
                let entities: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| {
                        let point = self.waypoint(id)?;
                        Some(json!({
                            "id": id.to_string(),
                            "components": { "transform": { "position": point.position } },
                        }))
                    })
                    .collect();
                Ok(json!({ "entities": entities }))
            }
            "spawns.waypoint_read" => {
                let entities: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| {
                        let point = self.waypoint(id)?;
                        Some(json!({ "id": id.to_string(), "fields": {
                            "path": id.key, "point": id.sub, "wait": point.delay, "run": point.run,
                            "facing": facing(&point),
                        } }))
                    })
                    .collect();
                Ok(json!({ "entities": entities }))
            }
            "spawns.write" | "spawns.waypoint_write" => {
                let field = arguments["field"].as_str().ok_or("'field' names a field")?;
                let axis = arguments["axis"].as_u64().map(|axis| axis as usize);
                let change = arguments["change"].as_str().ok_or("'change' is a value")?;
                let mut after = self.changes.clone();
                let (mut changed, mut refused) = (0, Vec::new());
                for id in ids {
                    let Some(point) = self.waypoint(id) else {
                        refused.push(format!("{id}: not held"));
                        continue;
                    };
                    // A gizmo's turn: the way it faces, about the vertical alone.
                    let written = match scene::turn_of(change) {
                        Some(([_, _, z], degrees)) if field == "rotation" && z != 0.0 => {
                            let turned = point.orientation.unwrap_or(0.0).to_degrees() + degrees * z.signum();
                            patrols::written(&point, "facing", None, &turned.to_string())
                        }
                        Some(_) => Err("a waypoint turns about the vertical alone".to_owned()),
                        None => patrols::written(&point, if field == "wait" { "delay" } else { field }, axis, change),
                    };
                    match written {
                        Ok(written) if written != point => {
                            let path = id.key as u32;
                            let mut points = self.patrols_now.paths.get(&path).cloned().unwrap_or_default();
                            if let Some(held) = after.patrols.paths.get(&path).cloned().flatten() {
                                points = held;
                            }
                            if let Some(at) = points.get_mut(id.sub as usize - 1) {
                                *at = written;
                                changed += 1;
                            }
                            after.patrols.paths.insert(path, Some(points));
                        }
                        Ok(_) => {}
                        Err(reason) => refused.push(format!("{id}: {reason}")),
                    }
                }
                self.change(format!("Set the {field} of {changed} waypoints"), after, ctx);
                Ok(json!({ "changed": changed, "refused": refused }))
            }
            "spawns.duplicate" | "spawns.delete" => {
                let mut after = self.changes.clone();
                let mut by_path: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
                for id in ids.iter().filter(|id| self.waypoint(id).is_some()) {
                    by_path.entry(id.key as u32).or_default().push(id.sub);
                }
                let mut made = Vec::new();
                for (path, mut at) in by_path {
                    let mut points = self.patrols_now.paths.get(&path).cloned().unwrap_or_default();
                    // The last first, for the places of the others to hold.
                    at.sort_unstable_by(|a, b| b.cmp(a));
                    for point in at {
                        let index = point as usize - 1;
                        if name == "spawns.delete" {
                            points.remove(index);
                        } else {
                            let mut copy = points[index].clone();
                            let next = points.get(index + 1).map_or(copy.position, |next| next.position);
                            copy.position = std::array::from_fn(|axis| (copy.position[axis] + next[axis]) / 2.0);
                            if copy.position == points[index].position {
                                copy.position[0] += 2.0;
                            }
                            points.insert(index + 1, copy);
                            made.push(EntityId::waypoint(path, point + 1).to_string());
                        }
                    }
                    after.patrols.paths.insert(path, Some(points));
                }
                let verb = if name == "spawns.delete" { "Delete" } else { "Duplicate" };
                self.change(format!("{verb} {} waypoints", ids.len()), after, ctx);
                Ok(if name == "spawns.delete" {
                    json!({ "deleted": ids.len() })
                } else {
                    json!({ "ids": made })
                })
            }
            other => Err(format!("'{other}' takes no waypoint")),
        }
    }

    /// The section *Patrol* of the panel: the patrol of the creature selected, made, drawn by clicks
    /// and taken away; the group it leads or follows; a group made of the creatures selected.
    pub(crate) fn patrol_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            return;
        };
        let selection = scene.selection();
        let creatures: Vec<u64> = scene
            .groups()
            .expanded(&selection.ids)
            .iter()
            .filter(|id| id.kind == Kind::Creature)
            .map(|id| id.key)
            .collect();
        let active = selection
            .active()
            .filter(|id| id.kind == Kind::Creature)
            .map(|id| id.key)
            .or(creatures.first().copied());
        let drawing = lock(&self.place_points).path;
        theme::section(
            ui,
            egui::Id::new("spawns patrol"),
            theme::icon::HISTORY,
            "Patrol",
            None,
            true,
            |ui| {
                let Some(guid) = active else {
                    ui.weak("Select a creature: its path, its group.");
                    return;
                };
                ui.label(
                    egui::RichText::new(
                        "Seen in the game at once; written to the database only by Apply to the server; lost when the server starts again unless applied.",
                    )
                    .weak()
                    .small(),
                );
                let name = self
                    .find(&EntityId::creature(guid))
                    .map_or(format!("#{guid}"), |spawn| format!("{} #{guid}", spawn.name));
                let walker = self.patrols_now.walkers.get(&guid).copied().unwrap_or_default();
                let points = self.patrols_now.paths.get(&walker.path).map_or(0, Vec::len);
                ui.label(match (walker.movement, walker.path) {
                    (patrols::WALKING, path) if path != 0 => format!("{name} walks the path {path}, {points} points"),
                    (1, _) => format!("{name} wanders about its place"),
                    (_, path) if path != 0 => format!("{name} stands; its addon names the path {path}"),
                    _ => format!("{name} stands still"),
                });
                ui.horizontal_wrapped(|ui| {
                    if walker.path == 0 || walker.movement != patrols::WALKING {
                        if ui
                            .button("Make a patrol")
                            .on_hover_text("A path of its own (its guid × 10), drawn by clicks on the ground")
                            .clicked()
                        {
                            self.make_patrol(guid, ctx);
                        }
                    } else {
                        let adding = drawing == Some(walker.path);
                        if ui
                            .selectable_label(adding, "Add points…")
                            .on_hover_text("Each click on the ground a point at the end of its path; Esc to stop")
                            .clicked()
                        {
                            let mut placing = lock(&self.place_points);
                            if adding {
                                placing.path = None;
                            } else {
                                placing.path = Some(walker.path);
                                placing.last = self
                                    .patrols_now
                                    .paths
                                    .get(&walker.path)
                                    .and_then(|points| points.last())
                                    .map(|point| point.position);
                            }
                            drop(placing);
                            self.waypoints_for = None;
                        }
                        if ui
                            .button("Take the patrol away")
                            .on_hover_text("It stands still and its path is taken away; in memory, written by Apply")
                            .clicked()
                        {
                            self.unmake_patrol(guid, ctx);
                        }
                    }
                });
                ui.separator();
                self.group_ui(ui, guid, &creatures, ctx);
            },
        );
    }

    fn make_patrol(&mut self, guid: u64, ctx: &mut Context) {
        let Some(spawn) = self.find(&EntityId::creature(guid)) else {
            return;
        };
        let walker = self.patrols_now.walkers.get(&guid).copied().unwrap_or_default();
        let path = match walker.path {
            0 => patrols::path_for(guid, &self.patrols_now),
            held => Some(held),
        };
        let Some(path) = path else {
            return;
        };
        let mut after = self.changes.clone();
        after.patrols.walkers.insert(
            guid,
            Walker {
                movement: patrols::WALKING,
                path,
            },
        );
        if !self.patrols_now.paths.contains_key(&path) {
            after.patrols.paths.insert(path, Some(vec![Point::at(spawn.position)]));
        }
        self.change(format!("Make a patrol for {}", spawn.name), after, ctx);
        let mut placing = lock(&self.place_points);
        placing.path = Some(path);
        placing.last = Some(
            self.patrols_now
                .paths
                .get(&path)
                .and_then(|points| points.last())
                .map_or(spawn.position, |point| point.position),
        );
    }

    fn unmake_patrol(&mut self, guid: u64, ctx: &mut Context) {
        let walker = self.patrols_now.walkers.get(&guid).copied().unwrap_or_default();
        let mut after = self.changes.clone();
        after.patrols.walkers.insert(guid, Walker::default());
        if self
            .patrols_now
            .walkers_of(walker.path)
            .iter()
            .all(|walker| *walker == guid)
        {
            after.patrols.paths.insert(walker.path, None);
        }
        let mut placing = lock(&self.place_points);
        if placing.path == Some(walker.path) {
            placing.path = None;
        }
        drop(placing);
        self.change("Take the patrol away".to_owned(), after, ctx);
    }

    /// The group of `guid`: those it leads, their places behind it; the one it follows; a group made
    /// of the creatures selected, led by it.
    fn group_ui(&mut self, ui: &mut egui::Ui, guid: u64, creatures: &[u64], ctx: &mut Context) {
        let now = self.patrols_now.clone();
        let others: Vec<u64> = creatures.iter().copied().filter(|other| *other != guid).collect();
        match now.leader_of(guid) {
            Some(leader) if leader == guid => {
                let (members, ai) = now.groups[&leader].clone();
                ui.label(format!("Leads a group of {}", members.len()));
                let shown = match &self.group_edit {
                    Some((edited, held, _)) if *edited == leader => held.clone(),
                    _ => members.clone(),
                };
                let mut edited = shown.clone();
                egui::Grid::new("spawns group")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.weak("Member");
                        ui.weak("Distance");
                        ui.weak("Angle");
                        ui.end_row();
                        for member in &mut edited {
                            let name = self
                                .find(&EntityId::creature(member.guid))
                                .map_or(format!("#{}", member.guid), |spawn| {
                                    format!("{} #{}", spawn.name, member.guid)
                                });
                            ui.label(name);
                            ui.add(
                                egui::DragValue::new(&mut member.distance)
                                    .speed(0.1)
                                    .range(0.0..=50.0)
                                    .suffix(" yd"),
                            )
                            .on_hover_text("How far behind the leader it walks, in yards; written by Apply");
                            ui.add(
                                egui::DragValue::new(&mut member.angle)
                                    .speed(1.0)
                                    .range(0.0..=360.0)
                                    .suffix("°"),
                            )
                            .on_hover_text(
                                "The angle from the leader it walks at, in degrees, 180 right behind; written by Apply",
                            );
                            ui.end_row();
                        }
                    });
                // Held while dragged, written once let go: one undo entry.
                if edited != shown {
                    self.group_edit = Some((leader, edited, ai));
                }
                if ui.input(|i| !i.pointer.any_down())
                    && let Some((_, held, ai)) = self.group_edit.take()
                    && held != members
                {
                    let mut after = self.changes.clone();
                    after.patrols.groups.insert(leader, Some((held, ai)));
                    self.change("Place the members of a group".to_owned(), after, ctx);
                }
                ui.horizontal(|ui| {
                    if ui
                        .button("Lay them out behind")
                        .on_hover_text(
                            "Its members two by two behind it, a few yards apart; in memory, written by Apply",
                        )
                        .clicked()
                    {
                        let guids: Vec<u64> = members.iter().map(|member| member.guid).collect();
                        let mut after = self.changes.clone();
                        after
                            .patrols
                            .groups
                            .insert(leader, Some((patrols::laid_out(&guids), ai)));
                        self.change("Lay a group out".to_owned(), after, ctx);
                    }
                    if ui
                        .button("Break the group")
                        .on_hover_text("Its members follow it no longer; in memory, written by Apply")
                        .clicked()
                    {
                        let mut after = self.changes.clone();
                        after.patrols.groups.insert(leader, None);
                        self.change("Break a group".to_owned(), after, ctx);
                    }
                });
            }
            Some(leader) => {
                let name = self
                    .find(&EntityId::creature(leader))
                    .map_or(format!("#{leader}"), |spawn| format!("{} #{leader}", spawn.name));
                ui.label(format!("Follows {name}"));
                if ui
                    .button("Leave the group")
                    .on_hover_text("It follows its leader no longer; in memory, written by Apply")
                    .clicked()
                {
                    let (members, ai) = now.groups[&leader].clone();
                    let left: Vec<Member> = members.into_iter().filter(|member| member.guid != guid).collect();
                    let mut after = self.changes.clone();
                    after
                        .patrols
                        .groups
                        .insert(leader, (!left.is_empty()).then_some((left, ai)));
                    self.change("Leave a group".to_owned(), after, ctx);
                }
            }
            None => {
                ui.weak("In no group.");
            }
        }
        if !others.is_empty()
            && ui
                .button(format!("Make a group of the {} selected, led by it", others.len() + 1))
                .on_hover_text("They follow it, two by two behind it; each taken out of any other group")
                .clicked()
        {
            self.make_group(guid, &others, ctx);
        }
    }

    fn make_group(&mut self, leader: u64, others: &[u64], ctx: &mut Context) {
        let now = self.patrols_now.clone();
        let mut after = self.changes.clone();
        for member in others {
            // Taken out of the group it was in; one it led broken.
            if let Some(before) = now.leader_of(*member).filter(|before| *before != leader) {
                if before == *member {
                    after.patrols.groups.insert(before, None);
                } else if let Some((members, ai)) = after.patrols.laid_over(&self.patrols).groups.get(&before).cloned()
                {
                    let left: Vec<Member> = members.into_iter().filter(|kept| kept.guid != *member).collect();
                    after
                        .patrols
                        .groups
                        .insert(before, (!left.is_empty()).then_some((left, ai)));
                }
            }
            // A member follows: it walks no path of its own.
            if now
                .walkers
                .get(member)
                .is_some_and(|walker| walker.movement != patrols::IDLE)
            {
                after.patrols.walkers.insert(*member, Walker::default());
            }
        }
        if now.leader_of(leader).is_some_and(|before| before != leader) {
            let before = now.leader_of(leader).unwrap_or(leader);
            if let Some((members, ai)) = now.groups.get(&before).cloned() {
                let left: Vec<Member> = members.into_iter().filter(|kept| kept.guid != leader).collect();
                after
                    .patrols
                    .groups
                    .insert(before, (!left.is_empty()).then_some((left, ai)));
            }
        }
        after
            .patrols
            .groups
            .insert(leader, Some((patrols::laid_out(others), patrols::GROUP_AI)));
        self.change(format!("Make a group of {}", others.len() + 1), after, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paths_drawn_are_those_of_the_database_but_where_the_user_changed_them() {
        let read: paths::Paths = [
            (79, vec![vec![[0.0; 3], [1.0, 0.0, 0.0]]]),
            (80, vec![vec![[5.0; 3], [6.0; 3]]]),
        ]
        .into_iter()
        .collect();
        let base = Patrols::default();
        let mut edits = Edits::default();
        assert_eq!(drawn_paths(&read, &edits, &base), read);
        edits.walkers.insert(81, Walker { movement: 2, path: 810 });
        edits
            .paths
            .insert(810, Some(vec![Point::at([1.0; 3]), Point::at([2.0; 3])]));
        edits.walkers.insert(80, Walker::default());
        let now = edits.laid_over(&base);
        let drawn = drawn_paths(&read, &edits, &now);
        assert_eq!(drawn[&81], vec![vec![[1.0; 3], [2.0; 3]]]);
        assert!(!drawn.contains_key(&80), "its patrol taken away");
        assert_eq!(drawn[&79], read[&79]);
    }
}
