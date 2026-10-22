//! The creatures dragged in the world moved live through the bridge, at most ten times a second,
//! nothing written.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use uniwow_api::serde_json::{Value, json};

use crate::bridge::{Move, Moving};
use crate::lock;

/// How often the moves go to the bridge at most, and how long its connection is kept without any.
const EVERY: Duration = Duration::from_millis(100);
const IDLE: Duration = Duration::from_millis(300);

/// The moves waiting for the thread that sends them, the last of each creature; what the bridge said
/// of the last that failed.
#[derive(Default)]
pub struct Moves {
    waiting: Mutex<BTreeMap<u64, Move>>,
    wake: Condvar,
    said: Mutex<Option<String>>,
}

impl Moves {
    /// Queues `moved`, in place of one of the same creature not sent yet: a creature let go stays let
    /// go until it is held again.
    pub fn push(&self, moved: Move) {
        lock(&self.waiting).insert(moved.guid, moved);
        self.wake.notify_one();
    }

    /// What went wrong with the last moves sent, none once they went.
    pub fn said(&self) -> Option<String> {
        lock(&self.said).clone()
    }
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

/// Sends the moves queued to the bridge `server` names (its port and token), ten times a second at
/// most, until `cancelled`; its connection closed once none came for a while, for the bridge to
/// take the others.
pub fn run(moves: &Moves, server: impl Fn() -> Option<(u16, String)>, cancelled: impl Fn() -> bool) {
    let mut moving = Moving::default();
    let mut last: Option<Instant> = None;
    while !cancelled() {
        let taken = {
            let waiting = lock(&moves.waiting);
            let (mut waiting, _) = moves
                .wake
                .wait_timeout_while(waiting, Duration::from_millis(100), |waiting| waiting.is_empty())
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *waiting)
        };
        if taken.is_empty() {
            if last.is_some_and(|at| at.elapsed() >= IDLE) {
                moving.close();
                last = None;
            }
            continue;
        }
        if let Some(at) = last {
            std::thread::sleep(EVERY.saturating_sub(at.elapsed()));
        }
        last = Some(Instant::now());
        let Some((port, token)) = server() else {
            *lock(&moves.said) = Some("no server chosen".to_owned());
            continue;
        };
        let taken: Vec<Move> = taken.into_values().collect();
        let said = match moving.send(port, &token, &taken) {
            Ok(made) => made
                .into_iter()
                .find(|(done, _)| !done)
                .map(|(_, said)| format!("the server: {said}")),
            Err(reason) => Some(reason),
        };
        if let Some(said) = &said
            && lock(&moves.said).as_ref() != Some(said)
        {
            uniwow_api::log::warn!("a creature is not moved live: {said}");
        }
        *lock(&moves.said) = said;
    }
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
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[&7], at(2.0, true));
    }
}
