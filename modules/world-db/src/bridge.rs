//! The client of `mod-uniwow-bridge` (step 16.2): once changes are applied, the server running asked
//! to load again what they touched, over TCP on 127.0.0.1, its protocol in the module's
//! `PROTOCOL.md`: HELLO with the token, RELOAD with the demands, DONE with what each made; and the
//! creatures dragged in the world moved live, MOVE, through a connection kept while they are.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use uniwow_api::world_db::Reload;

/// The version spoken: 2, a bridge of the version 1 spoken to in its own.
const VERSION: u16 = 2;
const HELLO: u8 = 1;
const RELOAD: u8 = 2;
const MOVE: u8 = 3;
const WELCOME: u8 = 101;
const REFUSED: u8 = 102;
const DONE: u8 = 103;

/// How long the bridge is waited for: to connect, and for the world to make the demands.
const CONNECT: Duration = Duration::from_secs(3);
const ANSWER: Duration = Duration::from_secs(40);

/// The demands of the bridge for `reloads`, in the order the server makes them: what the creatures
/// read (the quests, the addons, the paths, the groups), then the creatures and game objects spawned
/// again; the groups loaded once.
pub fn demands(reloads: &[Reload]) -> Vec<(u8, u64)> {
    let mut sorted = reloads.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut out: Vec<(u8, u64)> = Vec::new();
    for reload in sorted {
        let demand = match reload {
            Reload::Quests => (1, 0),
            Reload::ForgetQuest(id) => (7, u64::from(id)),
            Reload::Addons => (3, 0),
            Reload::Path(id) => (2, u64::from(id)),
            Reload::Formation(_) => (4, 0),
            Reload::Creature(guid) => (5, guid),
            Reload::Home(guid) => (HOME, guid),
            Reload::GameObject(guid) => (6, guid),
        };
        if !out.contains(&demand) {
            out.push(demand);
        }
    }
    out
}

fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.extend_from_slice(&(body.len() as u32 + 1).to_le_bytes());
    out.push(kind);
    out.extend_from_slice(body);
    out
}

fn string(out: &mut Vec<u8>, text: &str) {
    let bytes = &text.as_bytes()[..text.len().min(0xFFFF)];
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn read_message(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|error| format!("the bridge did not answer: {error}"))?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > 1 << 20 {
        return Err("the bridge answered something else".to_owned());
    }
    let mut bytes = vec![0u8; length];
    stream
        .read_exact(&mut bytes)
        .map_err(|error| format!("the bridge did not answer: {error}"))?;
    Ok((bytes[0], bytes[1..].to_vec()))
}

/// The body of a message, read within its bounds.
struct Body<'a>(&'a [u8]);

impl Body<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        if self.0.len() < count {
            return Err("the bridge answered something else".to_owned());
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("two bytes")))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn string(&mut self) -> Result<String, String> {
        let length = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(length)?).into_owned())
    }
}

/// The demand of a spawn point moved, of the version 2; a bridge of the version 1 asked to spawn the
/// creature again in its place.
const HOME: u8 = 8;

/// A connection to the bridge on `port` with `token`, and the version it speaks: the version 2
/// asked, the version 1 once a bridge of the version 1 refused it.
fn connect(port: u16, token: &str, timeout: Duration) -> Result<(TcpStream, u16), String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut asked = VERSION;
    loop {
        let mut stream = TcpStream::connect_timeout(&address, CONNECT)
            .map_err(|error| format!("no bridge on 127.0.0.1:{port} ({error}): is mod-uniwow-bridge installed?"))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(CONNECT)))
            .and_then(|_| stream.set_nodelay(true))
            .map_err(|error| error.to_string())?;
        let mut hello = asked.to_le_bytes().to_vec();
        string(&mut hello, token);
        stream
            .write_all(&message(HELLO, &hello))
            .map_err(|error| error.to_string())?;
        match read_message(&mut stream)? {
            (WELCOME, body) => {
                let spoken = Body(&body).u16()?;
                return Ok((stream, spoken.min(asked)));
            }
            (REFUSED, body) => {
                let reason = Body(&body).string()?;
                if asked > 1 && reason.contains("version 1") {
                    asked = 1;
                    continue;
                }
                return Err(format!("the bridge refused: {reason}"));
            }
            _ => return Err("the bridge answered something else".to_owned()),
        }
    }
}

/// Sends `kind` of `id` with `count` items of `items`, and reads what each made.
fn ask(stream: &mut TcpStream, kind: u8, id: u32, count: usize, items: &[u8]) -> Result<Vec<(bool, String)>, String> {
    let mut body = id.to_le_bytes().to_vec();
    body.extend_from_slice(&(count as u16).to_le_bytes());
    body.extend_from_slice(items);
    stream
        .write_all(&message(kind, &body))
        .map_err(|error| error.to_string())?;
    let (answered, answer) = read_message(stream)?;
    if answered != DONE {
        return Err("the bridge answered something else".to_owned());
    }
    let mut answer = Body(&answer);
    let (_, count) = (answer.u32()?, answer.u16()?);
    (0..count).map(|_| Ok((answer.u8()? == 1, answer.string()?))).collect()
}

/// Asks the bridge on `port` with `token` to make `demands`: what each made, and what the server
/// said of it.
pub fn reload(port: u16, token: &str, demands: &[(u8, u64)]) -> Result<Vec<(bool, String)>, String> {
    let (mut stream, version) = connect(port, token, ANSWER)?;
    let mut items = Vec::new();
    let mut asked: Vec<(u8, u64)> = Vec::new();
    for (action, value) in demands {
        let demand = if *action == HOME && version < 2 {
            (5, *value)
        } else {
            (*action, *value)
        };
        if !asked.contains(&demand) {
            asked.push(demand);
            items.push(demand.0);
            items.extend_from_slice(&demand.1.to_le_bytes());
        }
    }
    ask(&mut stream, RELOAD, 1, asked.len(), &items)
}

/// A creature in the world moved live where it is dragged: its map and instance, its GUID at run
/// time, whether it is let go, where it is put and how it faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    pub map: u32,
    pub instance: u32,
    pub guid: u64,
    pub let_go: bool,
    pub position: [f32; 3],
    pub orientation: f32,
}

/// How long a move is waited for: the world thread makes it at its next update.
const MOVED: Duration = Duration::from_secs(5);

/// The connection the moves go through, kept while they come, made again once broken.
#[derive(Default)]
pub struct Moving {
    stream: Option<TcpStream>,
    next: u32,
}

impl Moving {
    /// Moves the creatures of `moves` through the bridge on `port` with `token`: what each made.
    pub fn send(&mut self, port: u16, token: &str, moves: &[Move]) -> Result<Vec<(bool, String)>, String> {
        let mut items = Vec::with_capacity(moves.len() * 33);
        for moved in moves.iter().take(64) {
            items.extend_from_slice(&moved.map.to_le_bytes());
            items.extend_from_slice(&moved.instance.to_le_bytes());
            items.extend_from_slice(&moved.guid.to_le_bytes());
            items.push(u8::from(moved.let_go));
            for value in moved.position.iter().chain([&moved.orientation]) {
                items.extend_from_slice(&value.to_le_bytes());
            }
        }
        let count = moves.len().min(64);
        // A connection kept may have been closed by the bridge's silence: made again once.
        for again in [false, true] {
            if self.stream.is_none() {
                let (stream, version) = connect(port, token, MOVED)?;
                if version < 2 {
                    return Err(
                        "the server's bridge is older and moves nothing live: build the worldserver again with this mod-uniwow-bridge"
                            .to_owned(),
                    );
                }
                self.stream = Some(stream);
            }
            self.next = self.next.wrapping_add(1);
            let stream = self.stream.as_mut().expect("connected");
            match ask(stream, MOVE, self.next, count, &items) {
                Ok(made) => return Ok(made),
                Err(reason) => {
                    self.stream = None;
                    if again {
                        return Err(reason);
                    }
                }
            }
        }
        unreachable!("tried twice")
    }

    /// The connection closed: no move for a while.
    pub fn close(&mut self) {
        self.stream = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demands_are_made_what_the_creatures_read_first_the_groups_once() {
        assert_eq!(
            demands(&[
                Reload::Creature(79),
                Reload::Formation(79),
                Reload::Path(790),
                Reload::Formation(80),
                Reload::Quests,
                Reload::Addons,
                Reload::GameObject(5),
                Reload::Creature(79),
                Reload::ForgetQuest(82100),
            ]),
            [(1, 0), (7, 82100), (3, 0), (2, 790), (4, 0), (5, 79), (6, 5)]
        );
    }

    /// A bridge of one connection, as `mod-uniwow-bridge` answers, refusing a token other than
    /// `token`.
    fn bridge(token: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (kind, body) = read_message(&mut stream).unwrap();
            assert_eq!(kind, HELLO);
            let mut hello = Body(&body);
            assert_eq!(hello.u16().unwrap(), VERSION);
            if hello.string().unwrap() != token {
                let mut reason = Vec::new();
                string(&mut reason, "wrong token");
                stream.write_all(&message(REFUSED, &reason)).unwrap();
                return;
            }
            let mut welcome = VERSION.to_le_bytes().to_vec();
            string(&mut welcome, "AzerothCore");
            stream.write_all(&message(WELCOME, &welcome)).unwrap();
            let (kind, body) = read_message(&mut stream).unwrap();
            assert_eq!(kind, RELOAD);
            let mut asked = Body(&body);
            let (id, count) = (asked.u32().unwrap(), asked.u16().unwrap());
            let mut done = id.to_le_bytes().to_vec();
            done.extend_from_slice(&count.to_le_bytes());
            for _ in 0..count {
                let (action, value) = (asked.u8().unwrap(), asked.take(8).unwrap().to_vec());
                let value = u64::from_le_bytes(value.try_into().unwrap());
                done.push(u8::from(action != 6));
                string(&mut done, &format!("{action} {value}"));
            }
            stream.write_all(&message(DONE, &done)).unwrap();
        });
        port
    }

    #[test]
    fn a_bridge_of_the_version_1_is_asked_to_spawn_a_creature_again_for_its_spawn_point() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::thread::spawn(move || {
            // The version 2 refused, as a bridge of the version 1 does.
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = read_message(&mut stream).unwrap();
            assert_eq!(Body(&body).u16().unwrap(), 2);
            let mut reason = Vec::new();
            string(&mut reason, "this bridge speaks the version 1 of its protocol");
            stream.write_all(&message(REFUSED, &reason)).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = read_message(&mut stream).unwrap();
            assert_eq!(Body(&body).u16().unwrap(), 1);
            let mut welcome = 1u16.to_le_bytes().to_vec();
            string(&mut welcome, "AzerothCore");
            stream.write_all(&message(WELCOME, &welcome)).unwrap();
            let (_, body) = read_message(&mut stream).unwrap();
            let mut body = Body(&body);
            let (id, count) = (body.u32().unwrap(), body.u16().unwrap());
            let demands: Vec<(u8, u64)> = (0..count)
                .map(|_| {
                    (
                        body.u8().unwrap(),
                        u64::from_le_bytes(body.take(8).unwrap().try_into().unwrap()),
                    )
                })
                .collect();
            let mut done = id.to_le_bytes().to_vec();
            done.extend_from_slice(&count.to_le_bytes());
            for _ in 0..count {
                done.push(1);
                string(&mut done, "made");
            }
            stream.write_all(&message(DONE, &done)).unwrap();
            demands
        });
        let made = reload(port, "secret", &demands(&[Reload::Home(79), Reload::Creature(79)])).unwrap();
        assert_eq!(made.len(), 1, "the same demand once");
        assert_eq!(asked.join().unwrap(), [(5, 79)]);
    }

    #[test]
    fn the_bridge_is_asked_with_its_token_and_tells_what_each_demand_made() {
        let port = bridge("secret");
        assert_eq!(
            reload(port, "secret", &[(1, 0), (5, 79), (6, 5)]).unwrap(),
            [
                (true, "1 0".to_owned()),
                (true, "5 79".to_owned()),
                (false, "6 5".to_owned())
            ]
        );
        let port = bridge("secret");
        assert_eq!(
            reload(port, "other", &[(1, 0)]).unwrap_err(),
            "the bridge refused: wrong token"
        );
        // No bridge: said how to have one.
        let unused = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        assert!(
            reload(port, "secret", &[(1, 0)])
                .unwrap_err()
                .contains("mod-uniwow-bridge")
        );
    }
}
