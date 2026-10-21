//! The client of `mod-uniwow-bridge` (step 16.2): once changes are applied, the server running asked
//! to load again what they touched, over TCP on 127.0.0.1, its protocol in the module's
//! `PROTOCOL.md`: HELLO with the token, RELOAD with the demands, DONE with what each made.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use uniwow_api::world_db::Reload;

const VERSION: u16 = 1;
const HELLO: u8 = 1;
const RELOAD: u8 = 2;
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

/// Asks the bridge on `port` with `token` to make `demands`: what each made, and what the server
/// said of it.
pub fn reload(port: u16, token: &str, demands: &[(u8, u64)]) -> Result<Vec<(bool, String)>, String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT)
        .map_err(|error| format!("no bridge on 127.0.0.1:{port} ({error}): is mod-uniwow-bridge installed?"))?;
    stream
        .set_read_timeout(Some(ANSWER))
        .and_then(|_| stream.set_write_timeout(Some(CONNECT)))
        .map_err(|error| error.to_string())?;
    let mut hello = VERSION.to_le_bytes().to_vec();
    string(&mut hello, token);
    stream
        .write_all(&message(HELLO, &hello))
        .map_err(|error| error.to_string())?;
    match read_message(&mut stream)? {
        (WELCOME, _) => {}
        (REFUSED, body) => return Err(format!("the bridge refused: {}", Body(&body).string()?)),
        _ => return Err("the bridge answered something else".to_owned()),
    }
    let mut body = 1u32.to_le_bytes().to_vec();
    body.extend_from_slice(&(demands.len() as u16).to_le_bytes());
    for (action, value) in demands {
        body.push(*action);
        body.extend_from_slice(&value.to_le_bytes());
    }
    stream
        .write_all(&message(RELOAD, &body))
        .map_err(|error| error.to_string())?;
    let (kind, answer) = read_message(&mut stream)?;
    if kind != DONE {
        return Err("the bridge answered something else".to_owned());
    }
    let mut answer = Body(&answer);
    let (_, count) = (answer.u32()?, answer.u16()?);
    (0..count).map(|_| Ok((answer.u8()? == 1, answer.string()?))).collect()
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
                Reload::ForgetQuest(97000),
            ]),
            [(1, 0), (7, 97000), (3, 0), (2, 790), (4, 0), (5, 79), (6, 5)]
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
