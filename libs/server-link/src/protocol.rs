//! The protocol of the observer, version 1, both ways: what `server/mod-uniwow-observer/PROTOCOL.md`
//! describes, little-endian, each message its length, its kind and its body.

/// The version of the protocol spoken.
pub const VERSION: u16 = 1;
/// The longest message from the editor, and from the observer.
pub const MAX_FROM_EDITOR: usize = 1024;
pub const MAX_FROM_OBSERVER: usize = 16 << 20;
/// Capability: the observer reads.
pub const CAPABILITY_READING: u32 = 0x1;

const HELLO: u8 = 1;
const SUBSCRIBE: u8 = 2;
const UNSUBSCRIBE: u8 = 3;
const HEARTBEAT: u8 = 4;
const WELCOME: u8 = 101;
const REFUSED: u8 = 102;
const STATUS: u8 = 103;
const SNAPSHOT: u8 = 104;
const CHANGES: u8 = 105;

/// Flags of an entity.
pub const TEMPORARY: u8 = 0x01;
pub const DEAD: u8 = 0x02;
pub const WALKING: u8 = 0x04;
pub const FLYING: u8 = 0x08;
pub const MOVING: u8 = 0x10;
pub const GAME_MASTER: u8 = 0x20;

/// Flags of a spline.
pub const CATMULL_ROM: u8 = 0x01;
pub const CYCLIC: u8 = 0x02;
pub const FALLING: u8 = 0x04;

/// A circle of a map: its id, its instance (0 for a continent), its centre and its radius, in
/// the yards of the game.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    pub map: u32,
    pub instance: u32,
    pub centre: [f32; 3],
    pub radius: f32,
}

/// What the editor sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ToObserver {
    Hello { version: u16, token: String },
    Subscribe(Zone),
    Unsubscribe,
    Heartbeat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    pub version: u16,
    pub capabilities: u32,
    pub server: String,
    pub commit: String,
    pub max_radius: f32,
    pub max_entities: u32,
    /// Readings a second.
    pub rate: u16,
    /// Seconds of silence before the observer closes a connection.
    pub heartbeat: u16,
}

/// Where a subscription stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No update of its map and instance yet.
    Waiting,
    /// Read.
    Active,
    /// No map with that id and instance was updated within 2 seconds.
    NotFound,
}

/// What the observer sends.
#[derive(Clone, Debug, PartialEq)]
pub enum FromObserver {
    Welcome(Welcome),
    Refused(String),
    Status {
        state: State,
        map: u32,
        instance: u32,
        radius: f32,
    },
    Snapshot {
        map: u32,
        instance: u32,
        sequence: u64,
        entities: Vec<Entity>,
    },
    Changes {
        map: u32,
        instance: u32,
        sequence: u64,
        entities: Vec<Entity>,
        left: Vec<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Creature,
    GameObject,
    Player,
}

/// A point of a spline and the milliseconds from its start at which it is reached.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathPoint {
    pub position: [f32; 3],
    pub time: u32,
}

/// The spline an entity follows: its id, its flags, the milliseconds gone along it at the reading,
/// and the points it goes through.
#[derive(Clone, Debug, PartialEq)]
pub struct Spline {
    pub id: u32,
    pub flags: u8,
    pub elapsed: u32,
    pub points: Vec<PathPoint>,
}

/// A creature, game object or player, as the observer read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub guid: u64,
    pub kind: Kind,
    pub entry: u32,
    /// The guid of its row in `creature` or `gameobject`; 0 for none.
    pub spawn: u32,
    pub flags: u8,
    pub pool: u32,
    pub event: i16,
    pub phase: u32,
    pub display: u32,
    pub position: [f32; 3],
    pub orientation: f32,
    pub scale: f32,
    /// A game object's quaternion and state.
    pub rotation: Option<[f32; 4]>,
    pub state: Option<u8>,
    pub spline: Option<Spline>,
    pub name: String,
}

/// Little-endian bytes, a message once `frame` gives it its length and kind.
#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn f32(&mut self, value: f32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn string(&mut self, value: &str) {
        let bytes = &value.as_bytes()[..value.len().min(u16::MAX as usize)];
        self.u16(bytes.len() as u16);
        self.0.extend_from_slice(bytes);
    }

    fn frame(self, kind: u8) -> Vec<u8> {
        let mut message = Vec::with_capacity(self.0.len() + 5);
        message.extend_from_slice(&(self.0.len() as u32 + 1).to_le_bytes());
        message.push(kind);
        message.extend_from_slice(&self.0);
        message
    }
}

/// A body read within its bounds; any read past its end is an error.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(count).filter(|end| *end <= self.bytes.len());
        let end = end.ok_or_else(|| format!("a body cut short, {} bytes", self.bytes.len()))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("taken at its size"))
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.array()?))
    }
    fn vec3(&mut self) -> Result<[f32; 3], String> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn string(&mut self) -> Result<String, String> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| "a string not in UTF-8".to_owned())
    }

    fn done(&self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "{} bytes more than the message holds",
                self.bytes.len() - self.at
            ))
        }
    }
}

/// A whole message from the editor, framed.
pub fn encode_to_observer(message: &ToObserver) -> Vec<u8> {
    let mut writer = Writer::default();
    match message {
        ToObserver::Hello { version, token } => {
            writer.u16(*version);
            writer.string(token);
            writer.frame(HELLO)
        }
        ToObserver::Subscribe(zone) => {
            writer.u32(zone.map);
            writer.u32(zone.instance);
            for value in zone.centre {
                writer.f32(value);
            }
            writer.f32(zone.radius);
            writer.frame(SUBSCRIBE)
        }
        ToObserver::Unsubscribe => writer.frame(UNSUBSCRIBE),
        ToObserver::Heartbeat => writer.frame(HEARTBEAT),
    }
}

/// The message from the editor of kind `kind` and body `body`.
pub fn decode_to_observer(kind: u8, body: &[u8]) -> Result<ToObserver, String> {
    let mut reader = Reader::new(body);
    let message = match kind {
        HELLO => ToObserver::Hello {
            version: reader.u16()?,
            token: reader.string()?,
        },
        SUBSCRIBE => ToObserver::Subscribe(Zone {
            map: reader.u32()?,
            instance: reader.u32()?,
            centre: reader.vec3()?,
            radius: reader.f32()?,
        }),
        UNSUBSCRIBE => ToObserver::Unsubscribe,
        HEARTBEAT => ToObserver::Heartbeat,
        other => return Err(format!("a message of an unknown kind, {other}")),
    };
    reader.done()?;
    Ok(message)
}

/// A whole message from the observer, framed.
pub fn encode_from_observer(message: &FromObserver) -> Vec<u8> {
    let mut writer = Writer::default();
    match message {
        FromObserver::Welcome(welcome) => {
            writer.u16(welcome.version);
            writer.u32(welcome.capabilities);
            writer.string(&welcome.server);
            writer.string(&welcome.commit);
            writer.f32(welcome.max_radius);
            writer.u32(welcome.max_entities);
            writer.u16(welcome.rate);
            writer.u16(welcome.heartbeat);
            writer.frame(WELCOME)
        }
        FromObserver::Refused(reason) => {
            writer.string(reason);
            writer.frame(REFUSED)
        }
        FromObserver::Status {
            state,
            map,
            instance,
            radius,
        } => {
            writer.u8(match state {
                State::Waiting => 0,
                State::Active => 1,
                State::NotFound => 2,
            });
            writer.u32(*map);
            writer.u32(*instance);
            writer.f32(*radius);
            writer.frame(STATUS)
        }
        FromObserver::Snapshot {
            map,
            instance,
            sequence,
            entities,
        } => {
            writer.u32(*map);
            writer.u32(*instance);
            writer.u64(*sequence);
            writer.u32(entities.len() as u32);
            for entity in entities {
                encode_entity(&mut writer, entity);
            }
            writer.frame(SNAPSHOT)
        }
        FromObserver::Changes {
            map,
            instance,
            sequence,
            entities,
            left,
        } => {
            writer.u32(*map);
            writer.u32(*instance);
            writer.u64(*sequence);
            writer.u32(entities.len() as u32);
            for entity in entities {
                encode_entity(&mut writer, entity);
            }
            writer.u32(left.len() as u32);
            for guid in left {
                writer.u64(*guid);
            }
            writer.frame(CHANGES)
        }
    }
}

/// The message from the observer of kind `kind` and body `body`.
pub fn decode_from_observer(kind: u8, body: &[u8]) -> Result<FromObserver, String> {
    let mut reader = Reader::new(body);
    let message = match kind {
        WELCOME => FromObserver::Welcome(Welcome {
            version: reader.u16()?,
            capabilities: reader.u32()?,
            server: reader.string()?,
            commit: reader.string()?,
            max_radius: reader.f32()?,
            max_entities: reader.u32()?,
            rate: reader.u16()?,
            heartbeat: reader.u16()?,
        }),
        REFUSED => FromObserver::Refused(reader.string()?),
        STATUS => FromObserver::Status {
            state: match reader.u8()? {
                0 => State::Waiting,
                1 => State::Active,
                2 => State::NotFound,
                other => return Err(format!("a state of a subscription unknown, {other}")),
            },
            map: reader.u32()?,
            instance: reader.u32()?,
            radius: reader.f32()?,
        },
        SNAPSHOT => FromObserver::Snapshot {
            map: reader.u32()?,
            instance: reader.u32()?,
            sequence: reader.u64()?,
            entities: entities(&mut reader)?,
        },
        CHANGES => FromObserver::Changes {
            map: reader.u32()?,
            instance: reader.u32()?,
            sequence: reader.u64()?,
            entities: entities(&mut reader)?,
            left: {
                let count = reader.u32()? as usize;
                // Bounded by what the body holds, whatever the count says.
                let mut left = Vec::with_capacity(count.min(body.len() / 8));
                for _ in 0..count {
                    left.push(reader.u64()?);
                }
                left
            },
        },
        other => return Err(format!("a message of an unknown kind, {other}")),
    };
    reader.done()?;
    Ok(message)
}

fn encode_entity(writer: &mut Writer, entity: &Entity) {
    writer.u64(entity.guid);
    writer.u8(match entity.kind {
        Kind::Creature => 1,
        Kind::GameObject => 2,
        Kind::Player => 3,
    });
    writer.u32(entity.entry);
    writer.u32(entity.spawn);
    writer.u8(entity.flags);
    writer.u32(entity.pool);
    writer.u16(entity.event as u16);
    writer.u32(entity.phase);
    writer.u32(entity.display);
    for value in entity.position {
        writer.f32(value);
    }
    writer.f32(entity.orientation);
    writer.f32(entity.scale);
    if entity.kind == Kind::GameObject {
        for value in entity.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]) {
            writer.f32(value);
        }
        writer.u8(entity.state.unwrap_or(0));
    }
    match &entity.spline {
        Some(spline) if !spline.points.is_empty() => {
            let points = &spline.points[..spline.points.len().min(u8::MAX as usize)];
            writer.u8(points.len() as u8);
            writer.u32(spline.id);
            writer.u8(spline.flags);
            writer.u32(spline.elapsed);
            for point in points {
                for value in point.position {
                    writer.f32(value);
                }
                writer.u32(point.time);
            }
        }
        _ => writer.u8(0),
    }
    writer.string(&entity.name);
}

fn entities(reader: &mut Reader) -> Result<Vec<Entity>, String> {
    let count = reader.u32()? as usize;
    // An entity takes 55 bytes at least: the count cannot ask for more than the body holds.
    let mut entities = Vec::with_capacity(count.min(reader.bytes.len() / 55));
    for _ in 0..count {
        entities.push(entity(reader)?);
    }
    Ok(entities)
}

fn entity(reader: &mut Reader) -> Result<Entity, String> {
    let guid = reader.u64()?;
    let kind = match reader.u8()? {
        1 => Kind::Creature,
        2 => Kind::GameObject,
        3 => Kind::Player,
        other => return Err(format!("an entity of an unknown kind, {other}")),
    };
    let entry = reader.u32()?;
    let spawn = reader.u32()?;
    let flags = reader.u8()?;
    let pool = reader.u32()?;
    let event = reader.i16()?;
    let phase = reader.u32()?;
    let display = reader.u32()?;
    let position = reader.vec3()?;
    let orientation = reader.f32()?;
    let scale = reader.f32()?;
    let (rotation, state) = if kind == Kind::GameObject {
        (
            Some([reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?]),
            Some(reader.u8()?),
        )
    } else {
        (None, None)
    };
    let count = reader.u8()? as usize;
    let spline = if count > 0 {
        let id = reader.u32()?;
        let flags = reader.u8()?;
        let elapsed = reader.u32()?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            points.push(PathPoint {
                position: reader.vec3()?,
                time: reader.u32()?,
            });
        }
        Some(Spline {
            id,
            flags,
            elapsed,
            points,
        })
    } else {
        None
    };
    Ok(Entity {
        guid,
        kind,
        entry,
        spawn,
        flags,
        pool,
        event,
        phase,
        display,
        position,
        orientation,
        scale,
        rotation,
        state,
        spline,
        name: reader.string()?,
    })
}
