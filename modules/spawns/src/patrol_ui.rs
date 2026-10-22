//! The patrols in the editor (step 16.4): the points of the paths of the creatures selected given to
//! the scene, written by the gizmos and the Inspector; the card *Patrol* of the Inspector making a
//! patrol, its points listed, added, inserted, moved and deleted, a group following a leader; the
//! handles "+" of the view inserting a point; the paths drawn as the user changed them.

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
/// the last point, whether the user stopped; the paths shown, their handles "+", and the points
/// those clicked insert (the path, the place in it, where); the groups of the creatures selected, each
/// its leader and its followers, marked in the view.
#[derive(Default)]
pub struct Placing {
    pub path: Option<u32>,
    pub clicks: Vec<[f32; 3]>,
    pub last: Option<[f32; 3]>,
    pub stop: bool,
    pub shown: Vec<(u32, Vec<[f32; 3]>)>,
    pub inserts: Vec<(u32, usize, [f32; 3])>,
    pub groups: Vec<(u64, Vec<u64>, bool)>,
    /// Whether each point of the paths shown waits for Apply, as `shown` lists them.
    pub shown_pending: Vec<Vec<bool>>,
}

/// The handles "+" of the paths `shown`: the middle of each segment inserting a point there, and one
/// beyond the last adding one after it; each its path, its place in it, where in the world.
pub fn handles(shown: &[(u32, Vec<[f32; 3]>)]) -> Vec<(u32, usize, [f32; 3])> {
    let mut out = Vec::new();
    for (path, points) in shown {
        for (at, pair) in points.windows(2).enumerate() {
            let middle = std::array::from_fn(|axis| (pair[0][axis] + pair[1][axis]) / 2.0);
            out.push((*path, at + 1, middle));
        }
        if !points.is_empty() {
            let laid: Vec<Point> = points.iter().map(|at| Point::at(*at)).collect();
            out.push((*path, points.len(), patrols::beyond(&laid)));
        }
    }
    out
}

/// The colour of what the database has (green) or of what waits for Apply (red).
pub fn state_colour(pending: bool) -> egui::Color32 {
    let [r, g, b] = if pending {
        scene::state_colour::PENDING
    } else {
        scene::state_colour::DATABASE
    };
    egui::Color32::from_rgb(r, g, b)
}

/// How near the pointer must be to a handle "+", in points.
const HANDLE: f32 = 9.0;

impl PlaceTool {
    /// The handle "+" under `at` on the screen, the nearest.
    fn handle_at(&self, input: &ToolInput<'_>, at: egui::Pos2) -> Option<(u32, usize, [f32; 3])> {
        let placing = lock(&self.placing);
        handles(&placing.shown)
            .into_iter()
            .filter_map(|handle| {
                let on = input.project(handle.2.into())?;
                let away = on.distance(at);
                (away <= HANDLE).then_some((away, handle))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, handle)| handle)
    }
}

/// The tool adding points: it takes the pointer alone while points are added, a click on the
/// ground a point, Esc or a right click stops.
pub struct PlaceTool {
    pub placing: Arc<Mutex<Placing>>,
    pub scene: scene::Handle,
}

impl Tool for PlaceTool {
    fn claims(&mut self, input: &ToolInput<'_>) -> bool {
        if !input.response.hovered() {
            return false;
        }
        if lock(&self.placing).path.is_some() {
            return true;
        }
        // Over a handle "+", unless a drag began elsewhere.
        let (hover, origin) = input
            .response
            .ctx
            .input(|i| (i.pointer.hover_pos(), i.pointer.press_origin()));
        hover.is_some_and(|at| self.handle_at(input, at).is_some())
            && origin.is_none_or(|at| self.handle_at(input, at).is_some())
    }

    fn pointer(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        self.groups_ui(input, painter);
        if lock(&self.placing).path.is_none() {
            self.handles_ui(input, painter);
            return;
        }
        let mut placing = lock(&self.placing);
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

impl PlaceTool {
    /// Where the creature `guid` is: where it runs in the world, else at its spawn point.
    fn creature_at(&self, guid: u64) -> Option<[f32; 3]> {
        let id = EntityId::creature(guid);
        self.scene
            .position_now(&id)
            .or_else(|| self.scene.snapshot().editable(&id).map(|entity| entity.origin()))
    }

    /// The groups of the creatures selected marked: a crown "L" over the leader, a thin line from it
    /// to each follower, "F" over each follower.
    fn groups_ui(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let groups = lock(&self.placing).groups.clone();
        for (leader, followers, pending) in groups {
            let gold = state_colour(pending);
            let Some(head) = self
                .creature_at(leader)
                .and_then(|at| input.project([at[0], at[1], at[2] + 2.5].into()))
            else {
                continue;
            };
            for follower in followers {
                let Some(at) = self.creature_at(follower) else {
                    continue;
                };
                let Some(over) = input.project([at[0], at[1], at[2] + 2.5].into()) else {
                    continue;
                };
                painter.line_segment([head, over], egui::Stroke::new(1.0, gold.gamma_multiply(0.7)));
                painter.circle_filled(over, 7.0, egui::Color32::from_black_alpha(170));
                painter.text(
                    over,
                    egui::Align2::CENTER_CENTER,
                    "F",
                    egui::FontId::proportional(10.0),
                    gold,
                );
            }
            painter.circle_filled(head, 9.0, gold);
            painter.text(
                head,
                egui::Align2::CENTER_CENTER,
                "L",
                egui::FontId::proportional(12.0),
                egui::Color32::BLACK,
            );
        }
    }

    /// The handles "+" of the paths shown drawn, the one under the pointer lit with what it does; a
    /// click on one inserting its point.
    fn handles_ui(&mut self, input: &ToolInput<'_>, painter: &egui::Painter) {
        let shown = lock(&self.placing).shown.clone();
        if shown.is_empty() {
            return;
        }
        let response = input.response;
        let hovered = response.hover_pos().and_then(|at| self.handle_at(input, at));
        let colour = egui::Color32::from_gray(200);
        let pending = lock(&self.placing).shown_pending.clone();
        // The points themselves, numbered in the order walked, green as the database has them, red
        // waiting for Apply: clicked, one is selected and moved by its gizmo.
        for (index, (_, points)) in shown.iter().enumerate() {
            for (at, point) in points.iter().enumerate() {
                let Some(on) = input.project((*point).into()) else {
                    continue;
                };
                let waiting = pending
                    .get(index)
                    .and_then(|points| points.get(at))
                    .copied()
                    .unwrap_or(false);
                painter.circle_filled(on, 8.0, state_colour(waiting));
                painter.circle_stroke(on, 8.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
                painter.text(
                    on,
                    egui::Align2::CENTER_CENTER,
                    format!("{}", at + 1),
                    egui::FontId::proportional(10.0),
                    egui::Color32::WHITE,
                );
            }
        }
        for handle in handles(&shown) {
            let Some(at) = input.project(handle.2.into()) else {
                continue;
            };
            let lit = hovered == Some(handle);
            let radius = if lit { 7.0 } else { 5.5 };
            painter.circle_filled(at, radius, egui::Color32::from_black_alpha(if lit { 200 } else { 140 }));
            painter.circle_stroke(at, radius, egui::Stroke::new(1.5, colour));
            let arm = radius - 2.0;
            let stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
            painter.line_segment([at - egui::vec2(arm, 0.0), at + egui::vec2(arm, 0.0)], stroke);
            painter.line_segment([at - egui::vec2(0.0, arm), at + egui::vec2(0.0, arm)], stroke);
        }
        let Some(handle) = hovered else {
            return;
        };
        let last = shown
            .iter()
            .find(|(path, _)| *path == handle.0)
            .is_some_and(|(_, points)| handle.1 == points.len());
        if let Some(pointer) = response.hover_pos() {
            let text = if last {
                "Click: a point after the last (seen in the game at once, written by Apply)"
            } else {
                "Click: a point inserted here (seen in the game at once, written by Apply)"
            };
            painter.text(
                pointer + egui::vec2(14.0, 14.0),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
        }
        if response.clicked_by(egui::PointerButton::Primary) {
            lock(&self.placing).inserts.push(handle);
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
        // The groups of the creatures selected, marked in the view.
        let mut groups: Vec<(u64, Vec<u64>, bool)> = Vec::new();
        for id in scene
            .groups()
            .expanded(&selection.ids)
            .iter()
            .filter(|id| id.kind == Kind::Creature)
        {
            if let Some(leader) = self.patrols_now.leader_of(id.key)
                && !groups.iter().any(|(kept, _, _)| *kept == leader)
                && let Some((members, _)) = self.patrols_now.groups.get(&leader)
            {
                let pending = self.changes.patrols.groups.contains_key(&leader);
                groups.push((leader, members.iter().map(|member| member.guid).collect(), pending));
            }
        }
        lock(&self.place_points).groups = groups;
        lock(&self.place_points).shown_pending = shown
            .iter()
            .filter_map(|path| {
                let points = self.patrols_now.paths.get(path)?;
                Some(
                    points
                        .iter()
                        .enumerate()
                        .map(|(at, point)| patrols::pending_point(&self.patrols, *path, at, point))
                        .collect(),
                )
            })
            .collect();
        lock(&self.place_points).shown = shown
            .iter()
            .filter_map(|path| {
                let points = self.patrols_now.paths.get(path)?;
                Some((*path, points.iter().map(|point| point.position).collect()))
            })
            .collect();
        let key = PieceKey::part(MODULE, map, WAYPOINTS);
        match patrols::piece(&self.patrols_now, &self.patrols, &shown) {
            Some(piece) => scene.publish(key, Arc::new(piece)),
            None => scene.withdraw(key),
        }
        self.waypoints_shown = shown;
    }

    /// The clicks on the ground taken as points of the path being drawn, one undo entry for those
    /// of a frame; the drawing stopped once asked.
    pub(crate) fn take_clicks(&mut self, ctx: &mut Context) {
        // A handle "+" clicked: its point inserted, selected.
        let inserts = std::mem::take(&mut lock(&self.place_points).inserts);
        for (path, at, position) in inserts {
            let mut points = self.patrols_now.paths.get(&path).cloned().unwrap_or_default();
            let at = at.min(points.len());
            points.insert(at, Point::at(position));
            let mut after = self.changes.clone();
            after.patrols.paths.insert(path, Some(points));
            self.change("Insert a waypoint".to_owned(), after, ctx);
            if let Some(scene) = &self.scene {
                scene.select(scene::Select::Set(vec![EntityId::waypoint(path, at as u32 + 1)]));
            }
        }
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

    /// The creature whose patrol the Inspector shows for the selection, and the point of its path
    /// selected: a creature selected alone, or points of one path, the creature walking it.
    /// Several creatures selected: the active one, the leader of a group made of them.
    pub(crate) fn patrolled(&self, selected: &[EntityId], active: Option<EntityId>) -> Option<(u64, Option<u32>)> {
        match selected {
            [only] if only.kind == Kind::Creature => Some((only.key, None)),
            many if many.len() > 1 && many.iter().all(|id| id.kind == Kind::Creature) => {
                active.filter(|id| many.contains(id)).map(|id| (id.key, None))
            }
            points if !points.is_empty() && points.iter().all(|id| id.kind == Kind::Waypoint) => {
                let path = points[0].key as u32;
                if points.iter().any(|id| id.key as u32 != path) {
                    return None;
                }
                let walker = *self.patrols_now.walkers_of(path).first()?;
                Some((walker, (points.len() == 1).then_some(points[0].sub)))
            }
            _ => None,
        }
    }

    /// The card *Patrol* of the Inspector (its section `patrol`): how the creature selected moves,
    /// its path's points, each its place, its wait, whether it runs to it and the way it faces, the
    /// one selected in the view lit; points added, inserted, deleted, moved up and down; its group.
    /// Seen in the game at once, written by *Apply to the server* alone.
    pub(crate) fn patrol_ui(&mut self, ui: &mut egui::Ui, ctx: &mut Context) {
        let Some(scene) = self.scene.clone() else {
            return;
        };
        let selection = scene.selection();
        let selected = scene.groups().expanded(&selection.ids);
        let Some((guid, point)) = self.patrolled(&selected, selection.active()) else {
            return;
        };
        let creatures: Vec<u64> = selected
            .iter()
            .filter(|id| id.kind == Kind::Creature)
            .map(|id| id.key)
            .collect();
        let walker = self.patrols_now.walkers.get(&guid).copied().unwrap_or_default();
        let points = self.patrols_now.paths.get(&walker.path).cloned().unwrap_or_default();
        let note = (walker.path != 0).then(|| format!("path {}, {} points", walker.path, points.len()));
        theme::section(
            ui,
            egui::Id::new("spawns patrol card"),
            theme::icon::HISTORY,
            "Patrol",
            note.as_deref(),
            true,
            |ui| {
                ui.label(
                    egui::RichText::new(
                        "Seen in the game at once; written to the database only by Apply to the server; lost when the server starts again unless applied.",
                    )
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("●").color(state_colour(false)));
                    ui.label(egui::RichText::new("as the database has it").small());
                    ui.label(egui::RichText::new("●").color(state_colour(true)));
                    ui.label(egui::RichText::new("waiting for Apply").small());
                })
                .response
                .on_hover_text(scene::state_colour::LEGEND);
                self.movement_ui(ui, guid, walker, ctx);
                if walker.path == 0 || walker.movement != patrols::WALKING {
                    if ui
                        .button("Create patrol")
                        .on_hover_text(
                            "A path of its own (its guid × 10) from where it stands; its points added by the handles + of the view, Add, or Add by clicks…",
                        )
                        .clicked()
                    {
                        self.make_patrol(guid, ctx);
                    }
                } else {
                    self.points_ui(ui, walker.path, &points, point, ctx);
                    ui.label(
                        egui::RichText::new(
                            "It walks its path round and round, from its last point back to its first, as AzerothCore does.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        let drawing = lock(&self.place_points).path == Some(walker.path);
                        if ui
                            .selectable_label(drawing, "Add by clicks…")
                            .on_hover_text("Each click on the ground a point at the end of its path; Esc to stop")
                            .clicked()
                        {
                            let mut placing = lock(&self.place_points);
                            if drawing {
                                placing.path = None;
                            } else {
                                placing.path = Some(walker.path);
                                placing.last = points.last().map(|point| point.position);
                            }
                            drop(placing);
                            self.waypoints_for = None;
                        }
                        if ui
                            .button("Take the patrol away")
                            .on_hover_text("It stands still and its path is taken away")
                            .clicked()
                        {
                            self.unmake_patrol(guid, ctx);
                        }
                    });
                }
                ui.separator();
                let leader = self.patrols_now.leader_of(guid);
                let group_waiting = leader.is_some_and(|leader| self.changes.patrols.groups.contains_key(&leader))
                    || self.changes.patrols.groups.contains_key(&guid);
                ui.label(egui::RichText::new("Group").strong().color(state_colour(group_waiting)))
                    .on_hover_text(if group_waiting {
                        "The creatures following a leader (creature_formations): changed, waiting for Apply (red)"
                    } else {
                        "The creatures following a leader (creature_formations), as the database has them (green)"
                    });
                self.group_ui(ui, guid, &creatures, ctx);
            },
        );
    }

    /// How the creature `guid` moves (`MovementType`), a named list.
    fn movement_ui(&mut self, ui: &mut egui::Ui, guid: u64, walker: Walker, ctx: &mut Context) {
        const WAYS: [(u8, &str, &str); 3] = [
            (
                patrols::IDLE,
                "Stands still",
                "It stands at its spawn point (MovementType 0)",
            ),
            (
                1,
                "Wanders",
                "It wanders about its spawn point, within a few yards (MovementType 1)",
            ),
            (
                patrols::WALKING,
                "Walks its path",
                "It walks the points of its path (MovementType 2)",
            ),
        ];
        let mut chosen = None;
        let base = self.patrols.walkers.get(&guid).copied().unwrap_or_default();
        let waiting = base != walker;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Movement").color(state_colour(waiting)))
                .on_hover_text(if waiting {
                    format!(
                        "How the creature moves (creature.MovementType), waiting for Apply (red); in the database: MovementType {}, path {}",
                        base.movement, base.path
                    )
                } else {
                    "How the creature moves (creature.MovementType), as the database has it (green)".to_owned()
                });
            let shown = WAYS
                .iter()
                .find(|(way, _, _)| *way == walker.movement)
                .map_or(format!("{}", walker.movement), |(_, name, _)| (*name).to_owned());
            egui::ComboBox::from_id_salt("spawns patrol movement")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (way, name, hint) in WAYS {
                        if ui
                            .selectable_label(way == walker.movement, name)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            chosen = Some(way);
                        }
                    }
                })
                .response
                .on_hover_text("How the creature moves (creature.MovementType)");
        });
        let Some(way) = chosen.filter(|way| *way != walker.movement) else {
            return;
        };
        if way == patrols::WALKING && walker.path == 0 {
            self.make_patrol(guid, ctx);
            return;
        }
        let mut after = self.changes.clone();
        after.patrols.walkers.insert(
            guid,
            Walker {
                movement: way,
                ..walker
            },
        );
        self.change("Set how a creature moves".to_owned(), after, ctx);
    }

    /// The points of the path `path`: each a row, the one `selected` lit and chosen by a click, its
    /// fields typed; the buttons adding, inserting, deleting, moving up and down.
    fn points_ui(&mut self, ui: &mut egui::Ui, path: u32, points: &[Point], selected: Option<u32>, ctx: &mut Context) {
        let width = 58.0;
        let mut written: Option<(usize, &'static str, String)> = None;
        let mut chose = None;
        egui::ScrollArea::vertical()
            .id_salt("spawns patrol points")
            .max_height(260.0)
            .show(ui, |ui| {
                egui::Grid::new("spawns patrol points grid")
                    .num_columns(7)
                    .striped(true)
                    .spacing([6.0, 2.0])
                    .show(ui, |ui| {
                        for (label, hint) in [
                            ("#", "The number of the point; click it to select it in the view"),
                            ("X", "Where it is, west to east"),
                            ("Y", "Where it is, south to north"),
                            ("Z", "Its height"),
                            ("Wait", "How long the creature waits there, in milliseconds"),
                            ("Run", "Whether the creature runs to it, walks otherwise"),
                            ("Facing", "The way it faces there, in degrees; empty: the way it walks"),
                        ] {
                            ui.label(egui::RichText::new(label).weak()).on_hover_text(hint);
                        }
                        ui.end_row();
                        for (at, point) in points.iter().enumerate() {
                            let number = at as u32 + 1;
                            let waiting = patrols::pending_point(&self.patrols, path, at, point);
                            let database = self.patrols.paths.get(&path).and_then(|points| points.get(at)).map_or(
                                "none".to_owned(),
                                |base| {
                                    format!(
                                        "{:.2}, {:.2}, {:.2}",
                                        base.position[0], base.position[1], base.position[2]
                                    )
                                },
                            );
                            let hint = if waiting {
                                format!(
                                    "Waiting for Apply (red); in the database: {database}. Click: select it in the view"
                                )
                            } else {
                                "As the database has it (green). Click: select it in the view, its gizmo moves it"
                                    .to_owned()
                            };
                            if ui
                                .selectable_label(
                                    selected == Some(number),
                                    egui::RichText::new(format!("{number}"))
                                        .color(state_colour(waiting))
                                        .strong(),
                                )
                                .on_hover_text(hint)
                                .clicked()
                            {
                                chose = Some(number);
                            }
                            for (axis, name) in ["x", "y", "z"].into_iter().enumerate() {
                                let key = format!("{path}:{number}:{name}");
                                let value = format!("{:.2}", point.position[axis]);
                                if let Some(typed) = self.point_field(ui, key, &value, width, "Typed, then Enter") {
                                    written = Some((at, ["x", "y", "z"][axis], typed));
                                }
                            }
                            let key = format!("{path}:{number}:wait");
                            if let Some(typed) = self.point_field(
                                ui,
                                key,
                                &point.delay.to_string(),
                                width,
                                "How long it waits there (ms)",
                            ) {
                                written = Some((at, "delay", typed));
                            }
                            let mut run = point.run;
                            if ui
                                .checkbox(&mut run, "")
                                .on_hover_text("Runs to it; walks otherwise")
                                .changed()
                            {
                                written = Some((at, "run", run.to_string()));
                            }
                            let key = format!("{path}:{number}:facing");
                            let facing = point
                                .orientation
                                .map_or(String::new(), |turn| format!("{:.0}", turn.to_degrees()));
                            if let Some(typed) =
                                self.point_field(ui, key, &facing, width, "Degrees; empty: the way it walks")
                            {
                                written = Some((at, "facing", typed));
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(number) = chose
            && let Some(scene) = &self.scene
        {
            scene.select(scene::Select::Set(vec![EntityId::waypoint(path, number)]));
        }
        if let Some((at, field, typed)) = written {
            let id = EntityId::waypoint(path, at as u32 + 1);
            let (field, axis) = match field {
                "x" => ("position", Some(0)),
                "y" => ("position", Some(1)),
                "z" => ("position", Some(2)),
                other => (other, None),
            };
            let arguments = json!({ "ids": [id.to_string()], "field": field, "axis": axis, "change": typed });
            let _ = self.waypoint_command("spawns.write", &[id], &arguments, ctx);
        }
        ui.horizontal_wrapped(|ui| {
            let index = selected.map(|number| number as usize - 1);
            let mut edited: Option<(String, Vec<Point>, Option<u32>)> = None;
            if ui
                .button("Add")
                .on_hover_text("A point after the last, a few yards on")
                .clicked()
            {
                let mut after = points.to_vec();
                after.push(Point::at(patrols::beyond(points)));
                edited = Some(("Add a waypoint".to_owned(), after, Some(points.len() as u32 + 1)));
            }
            if ui
                .add_enabled(index.is_some(), egui::Button::new("Insert"))
                .on_hover_text("A point before the one selected, halfway from the one before it")
                .clicked()
                && let Some(at) = index
            {
                let mut after = points.to_vec();
                let before = at
                    .checked_sub(1)
                    .map_or(points[at].position, |back| points[back].position);
                let middle = std::array::from_fn(|axis| (before[axis] + points[at].position[axis]) / 2.0);
                after.insert(at, Point::at(middle));
                edited = Some(("Insert a waypoint".to_owned(), after, Some(at as u32 + 1)));
            }
            if ui
                .add_enabled(index.is_some(), egui::Button::new("Delete"))
                .on_hover_text("The point selected taken away (Delete in the view)")
                .clicked()
                && let Some(at) = index
            {
                let mut after = points.to_vec();
                after.remove(at);
                edited = Some(("Delete a waypoint".to_owned(), after, None));
            }
            if ui
                .add_enabled(index.is_some_and(|at| at > 0), egui::Button::new("Up"))
                .on_hover_text("The point selected walked to one earlier")
                .clicked()
                && let Some(at) = index
            {
                let mut after = points.to_vec();
                after.swap(at, at - 1);
                edited = Some(("Move a waypoint up".to_owned(), after, Some(at as u32)));
            }
            if ui
                .add_enabled(index.is_some_and(|at| at + 1 < points.len()), egui::Button::new("Down"))
                .on_hover_text("The point selected walked to one later")
                .clicked()
                && let Some(at) = index
            {
                let mut after = points.to_vec();
                after.swap(at, at + 1);
                edited = Some(("Move a waypoint down".to_owned(), after, Some(at as u32 + 2)));
            }
            if let Some((label, after_points, select)) = edited {
                let mut after = self.changes.clone();
                after.patrols.paths.insert(path, Some(after_points));
                self.change(label, after, ctx);
                if let (Some(number), Some(scene)) = (select, &self.scene) {
                    scene.select(scene::Select::Set(vec![EntityId::waypoint(path, number)]));
                }
            }
        });
    }

    /// A field of a point typed: its text kept while focused, given once Enter or once left.
    fn point_field(&mut self, ui: &mut egui::Ui, key: String, current: &str, width: f32, hint: &str) -> Option<String> {
        let mut text = self
            .point_texts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| current.to_owned());
        let response = ui
            .add_sized([width, 18.0], egui::TextEdit::singleline(&mut text))
            .on_hover_text(hint);
        if response.has_focus() || response.changed() {
            self.point_texts.insert(key.clone(), text.clone());
        }
        if response.lost_focus() {
            self.point_texts.remove(&key);
            if text.trim() != current {
                return Some(text.trim().to_owned());
            }
        }
        None
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
        // Its points added by the handles "+" or *Add by clicks…*, asked for: a mode taking every
        // click of the view must not start unasked.
        self.change(format!("Make a patrol for {}", spawn.name), after, ctx);
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
                            if ui
                                .link(name)
                                .on_hover_text("Select this follower in the view")
                                .clicked()
                                && let Some(scene) = &self.scene
                            {
                                scene.select(scene::Select::Set(vec![EntityId::creature(member.guid)]));
                            }
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
        // How a group is made: the followers selected first, the leader last.
        let leader_name = self
            .find(&EntityId::creature(guid))
            .map_or(format!("#{guid}"), |spawn| format!("{} #{guid}", spawn.name));
        let rule = if others.is_empty() {
            format!(
                "To make a group: select the followers, then the leader last (Ctrl+click); {leader_name} leads it when chosen last."
            )
        } else {
            format!(
                "Select the followers, then the leader last: {leader_name}, chosen last, leads the {} others.",
                others.len()
            )
        };
        ui.label(egui::RichText::new(rule).weak().small())
            .on_hover_text("The leader is the creature chosen last in the selection, the one the Inspector shows");
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
    fn a_path_shows_a_handle_at_the_middle_of_each_segment_and_one_beyond_its_last_point() {
        let shown = vec![(790, vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 2.0]])];
        let found = handles(&shown);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0], (790, 1, [5.0, 0.0, 0.0]), "inserted as the second point");
        assert_eq!(found[1], (790, 2, [10.0, 5.0, 1.0]));
        assert_eq!(
            found[2],
            (790, 3, [10.0, 14.0, 2.0]),
            "after the last, the way the last two go"
        );
        assert!(handles(&[(791, Vec::new())]).is_empty());
    }

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
