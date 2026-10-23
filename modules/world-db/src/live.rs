//! What is sent to the memory of the server running alone, through its bridge, nothing written to
//! its database: the creatures dragged in the world moved live, at most ten times a second; and the
//! changes the user made and has not applied yet (the spawn points, the patrols), set in its memory
//! for them to be seen in the game at once, until *Apply to the server* writes them. The server
//! watched meanwhile: started again, it lost them, and the editor is told.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::Reload;

use crate::bridge::{GroupMember, Move, Moving, PathPoint, Set};
use crate::lock;

/// How often the moves go to the bridge at most, how long its connection is kept without any, and
/// how often the server is looked at while it holds changes not applied.
const EVERY: Duration = Duration::from_millis(100);
const IDLE: Duration = Duration::from_millis(300);
const WATCH: Duration = Duration::from_secs(10);

/// Changes to set, and what the server loads again from its database (the demands of the bridge).
type Batch = (Vec<Set>, Vec<(u8, u64)>);

/// What waits for the thread: the last move of each creature, and the changes to set, in order,
/// each with what the server loads again from its database (a change undone).
#[derive(Default)]
struct Queue {
    moves: BTreeMap<u64, Move>,
    batches: VecDeque<Batch>,
}

/// What the commands share with the thread that sends: what waits, what the bridge said of the last
/// that failed, whether the server holds changes not applied, and whether it was seen started again.
#[derive(Default)]
pub struct Moves {
    waiting: Mutex<Queue>,
    wake: Condvar,
    said: Mutex<Option<String>>,
    watch: AtomicBool,
    restarted: AtomicBool,
}

impl Moves {
    /// Queues `moved`, in place of one of the same creature not sent yet: a creature let go stays let
    /// go until it is held again.
    pub fn push(&self, moved: Move) {
        lock(&self.waiting).moves.insert(moved.guid, moved);
        self.wake.notify_one();
    }

    /// Queues `sets` and `demands` after those queued before; `watch` whether the server holds
    /// changes not applied once they are.
    pub fn push_batch(&self, sets: Vec<Set>, demands: Vec<(u8, u64)>, watch: bool) {
        self.watch.store(watch, Ordering::Relaxed);
        if sets.is_empty() && demands.is_empty() {
            return;
        }
        lock(&self.waiting).batches.push_back((sets, demands));
        self.wake.notify_one();
    }

    /// What went wrong with the last sent, none once they went.
    pub fn said(&self) -> Option<String> {
        lock(&self.said).clone()
    }

    /// Whether the server was seen started again since asked last.
    pub fn restarted(&self) -> bool {
        self.restarted.swap(false, Ordering::Relaxed)
    }

    fn tell(&self, said: Option<String>, what: &str) {
        if let Some(said) = &said
            && lock(&self.said).as_ref() != Some(said)
        {
            uniwow_api::log::warn!("{what}: {said}");
        }
        *lock(&self.said) = said;
    }
}

/// A number of `value` at `key`, within `u32`.
fn number(value: &Value, key: &str) -> Option<u32> {
    value[key].as_u64().and_then(|number| u32::try_from(number).ok())
}

fn place(value: &Value) -> Option<[f32; 3]> {
    let read: Vec<f32> = value
        .as_array()?
        .iter()
        .filter_map(|value| value.as_f64().map(|v| v as f32))
        .filter(|value| value.is_finite())
        .collect();
    <[f32; 3]>::try_from(read).ok()
}

/// A change to set in the server's memory, as the command `world-db.set_live` is given it:
/// `{ "home": guid, "position", "orientation" }`, `{ "path": id, "points": [{ "position",
/// "orientation", "delay", "run" }] }`, `{ "walker": guid, "movement", "path" }`.
pub fn set_of(value: &Value) -> Result<Set, String> {
    if let Some(spawn) = value["home"].as_u64() {
        return Ok(Set::Home {
            spawn,
            position: place(&value["position"]).ok_or("a spawn point is x, y and z")?,
            orientation: value["orientation"].as_f64().unwrap_or(0.0) as f32,
        });
    }
    if let Some(path) = number(value, "path").filter(|_| value["walker"].is_null()) {
        let points = value["points"]
            .as_array()
            .ok_or("a path is its points")?
            .iter()
            .map(|point| {
                Ok(PathPoint {
                    position: place(&point["position"]).ok_or("a point is x, y and z")?,
                    orientation: point["orientation"].as_f64().map(|turn| turn as f32),
                    delay: number(point, "delay").unwrap_or(0),
                    run: point["run"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Result<_, String>>()?;
        return Ok(Set::Path { path, points });
    }
    if let Some(leader) = value["group"].as_u64() {
        let members = value["members"]
            .as_array()
            .ok_or("a group is its members")?
            .iter()
            .map(|member| {
                Ok(GroupMember {
                    guid: member["guid"].as_u64().ok_or("a member is a guid")?,
                    distance: member["distance"].as_f64().unwrap_or(0.0) as f32,
                    angle: member["angle"].as_f64().unwrap_or(0.0) as f32,
                    ai: member["ai"].as_u64().and_then(|ai| u16::try_from(ai).ok()).unwrap_or(0),
                })
            })
            .collect::<Result<_, String>>()?;
        return Ok(Set::Group { leader, members });
    }
    if let Some(spawn) = value["walker"].as_u64() {
        return Ok(Set::Walker {
            spawn,
            movement: number(value, "movement").unwrap_or(0).min(2) as u8,
            path: number(value, "path").unwrap_or(0),
        });
    }
    Err("a change is a home, a path, a walker or a group".to_owned())
}

/// What the command `world-db.set_live` queues: its changes to set, what the server loads again from
/// its database, whether the server holds changes not applied once they are.
pub fn batch_of(arguments: &Value) -> Result<(Batch, bool), String> {
    let list = |key: &str| arguments[key].as_array().cloned().unwrap_or_default();
    let sets: Vec<Set> = list("sets").iter().map(set_of).collect::<Result<_, _>>()?;
    let reloads: Vec<Reload> = list("reloads").iter().filter_map(Reload::of_json).collect();
    let watch = arguments["watch"].as_bool().unwrap_or(false);
    Ok(((sets, crate::bridge::demands(&reloads)), watch))
}

/// A GUID given as `0x…` text, as the live world gives them, or as a number.
fn guid(value: &Value) -> Option<u64> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => text.parse().ok(),
            }
        }
        other => other.as_u64(),
    }
}

/// The move the command `world-db.move_live` is given.
pub fn move_of(arguments: &Value) -> Result<Move, String> {
    let number = |key: &str| arguments[key].as_u64().and_then(|value| u32::try_from(value).ok());
    let position = arguments["position"]
        .as_array()
        .filter(|values| values.len() == 3)
        .and_then(|values| {
            let read: Vec<f32> = values
                .iter()
                .filter_map(|value| value.as_f64().map(|v| v as f32))
                .collect();
            <[f32; 3]>::try_from(read).ok()
        })
        .filter(|position| position.iter().all(|value| value.is_finite()))
        .ok_or("'position' is x, y and z in the world")?;
    Ok(Move {
        map: number("map").ok_or("'map' is the map of the creature")?,
        instance: number("instance").unwrap_or(0),
        guid: guid(&arguments["guid"]).ok_or("'guid' is the GUID of the creature at run time")?,
        let_go: arguments["let_go"].as_bool().unwrap_or(false),
        position,
        orientation: arguments["orientation"]
            .as_f64()
            .map(|turn| turn as f32)
            .filter(|turn| turn.is_finite())
            .unwrap_or(0.0),
    })
}

/// What the command answers: what went wrong with the moves sent last.
pub fn answer(moves: &Moves) -> Value {
    json!({ "said": moves.said() })
}

/// Sends what is queued to the bridge `server` names (its port and token), the changes first, the
/// moves ten times a second at most, until `cancelled`; its connection closed once nothing came for
/// a while, for the bridge to take the others. While the server holds changes not applied, it is
/// looked at every ten seconds: started again since, `restarted` is told.
pub fn run(moves: &Moves, server: impl Fn() -> Option<(u16, String)>, cancelled: impl Fn() -> bool) {
    let mut moving = Moving::default();
    let mut last: Option<Instant> = None;
    let mut looked: Option<Instant> = None;
    // The server last seen, by the port of its bridge, and when it started.
    let mut known: Option<(u16, u64)> = None;
    while !cancelled() {
        let (batches, taken) = {
            let waiting = lock(&moves.waiting);
            let (mut waiting, _) = moves
                .wake
                .wait_timeout_while(waiting, Duration::from_millis(100), |queue| {
                    queue.moves.is_empty() && queue.batches.is_empty()
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (std::mem::take(&mut waiting.batches), std::mem::take(&mut waiting.moves))
        };
        let Some((port, token)) = server() else {
            if !batches.is_empty() || !taken.is_empty() {
                *lock(&moves.said) = Some("no server chosen".to_owned());
            }
            continue;
        };
        let watching = moves.watch.load(Ordering::Relaxed);
        if batches.is_empty() && taken.is_empty() {
            if watching && looked.is_none_or(|at| at.elapsed() >= WATCH) {
                looked = Some(Instant::now());
                if let Ok(Some(started)) = moving.started(port, &token) {
                    see(moves, &mut known, port, started);
                }
                last.get_or_insert_with(Instant::now);
            }
            if last.is_some_and(|at| at.elapsed() >= IDLE) {
                moving.close();
                last = None;
            }
            continue;
        }
        for (sets, demands) in batches {
            let mut said = None;
            if !sets.is_empty() {
                said = failed(moving.set(port, &token, &sets));
            }
            if !demands.is_empty() {
                said = failed(moving.reload(port, &token, &demands)).or(said);
            }
            moves.tell(said, "a change is not seen in the game");
        }
        if let Ok(Some(started)) = moving.started(port, &token) {
            see(moves, &mut known, port, started);
        }
        if !taken.is_empty() {
            if let Some(at) = last {
                std::thread::sleep(EVERY.saturating_sub(at.elapsed()));
            }
            let taken: Vec<Move> = taken.into_values().collect();
            moves.tell(
                failed(moving.send(port, &token, &taken)),
                "a creature is not moved live",
            );
        }
        last = Some(Instant::now());
    }
}

/// What went wrong with what was sent: the bridge refused it, or the server said one was not made.
fn failed(outcome: Result<Vec<(bool, String)>, String>) -> Option<String> {
    match outcome {
        Ok(made) => made
            .into_iter()
            .find(|(done, _)| !done)
            .map(|(_, said)| format!("the server: {said}")),
        Err(reason) => Some(reason),
    }
}

/// The server of the bridge on `port` seen started at `started`: told started again when it was
/// seen started before at another time.
fn see(moves: &Moves, known: &mut Option<(u16, u64)>, port: u16, started: u64) {
    if known.is_some_and(|(seen, before)| seen == port && before != started) {
        moves.restarted.store(true, Ordering::Relaxed);
    }
    *known = Some((port, started));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_is_read_with_its_guid_as_the_live_world_gives_it() {
        let moved = move_of(&json!({
            "map": 0, "guid": "0xF130000044000ABC", "position": [1.0, 2.0, 3.0], "orientation": 1.5, "let_go": true,
        }))
        .unwrap();
        assert_eq!(moved.guid, 0xF130_0000_4400_0ABC);
        assert_eq!((moved.map, moved.instance, moved.let_go), (0, 0, true));
        assert_eq!(moved.position, [1.0, 2.0, 3.0]);
        assert!(move_of(&json!({ "map": 0, "guid": "0x1", "position": [1.0, 2.0] })).is_err());
    }

    #[test]
    fn a_server_started_again_is_told_once_another_server_is_not() {
        let (moves, mut known) = (Moves::default(), None);
        see(&moves, &mut known, 8092, 100);
        see(&moves, &mut known, 8092, 100);
        assert!(!moves.restarted());
        see(&moves, &mut known, 8093, 200);
        assert!(!moves.restarted(), "another server chosen");
        see(&moves, &mut known, 8093, 300);
        assert!(moves.restarted() && !moves.restarted(), "told once");
    }

    #[test]
    fn the_changes_to_set_are_read_as_the_command_gives_them() {
        let ((sets, demands), watch) = batch_of(&json!({
            "sets": [
                { "home": 79, "position": [1.0, 2.0, 3.0], "orientation": 0.5 },
                { "path": 790, "points": [{ "position": [1.0, 2.0, 3.0], "orientation": null, "delay": 5, "run": true }] },
                { "walker": 80, "movement": 2, "path": 790 },
            ],
            "reloads": [{ "path": 791 }],
            "watch": true,
        }))
        .unwrap();
        assert_eq!(sets.len(), 3);
        assert_eq!(
            sets[2],
            Set::Walker {
                spawn: 80,
                movement: 2,
                path: 790
            }
        );
        assert!(matches!(&sets[1], Set::Path { path: 790, points } if points[0].delay == 5 && points[0].run));
        assert_eq!((demands, watch), (vec![(2, 791)], true));
    }

    #[test]
    fn the_last_move_of_a_creature_replaces_the_one_waiting() {
        let moves = Moves::default();
        let at = |x: f32, let_go: bool| Move {
            map: 0,
            instance: 0,
            guid: 7,
            let_go,
            position: [x, 0.0, 0.0],
            orientation: 0.0,
        };
        moves.push(at(1.0, false));
        moves.push(at(2.0, true));
        let waiting = lock(&moves.waiting);
        assert_eq!(waiting.moves.len(), 1);
        assert_eq!(waiting.moves[&7], at(2.0, true));
    }
}
