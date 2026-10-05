# The protocol of mod-uniwow-observer, version 1

SPDX-License-Identifier: GPL-2.0-or-later

The observer streams, read only, the creatures, game objects and players of a zone of a map to the
editor, over TCP on 127.0.0.1. One connection holds one subscription at most.

## Framing

Every message, both ways:

| Field | Type | |
|---|---|---|
| length | u32 | the bytes that follow: the kind and the body |
| kind | u8 | below |
| body | bytes | `length - 1` bytes |

All numbers are little-endian; `f32` is an IEEE 754 single. A string is a `u16` length in bytes,
then its bytes, UTF-8. A message from the editor is 1,024 bytes at most, a message from the
observer 16 MiB at most. A message too long, of an unknown kind, out of order, or whose body is
not exactly what its kind says closes the connection, nothing more.

## From the editor

| Kind | Name | Body |
|---|---|---|
| 1 | HELLO | `u16` version (1), string token |
| 2 | SUBSCRIBE | `u32` map, `u32` instance (0 for a continent), `f32` x, y, z of the centre, `f32` radius in yards |
| 3 | UNSUBSCRIBE | none |
| 4 | HEARTBEAT | none |

HELLO comes first, once. The radius of a SUBSCRIBE is brought within the maximum the observer
gives. A SUBSCRIBE to the map and instance of the subscription already there moves its zone: what
was sent stays, and the readings that follow send what entered the zone, left it or changed. A
SUBSCRIBE to another map or instance starts again from nothing. Any message keeps the connection
alive: the observer closes one it heard nothing from for the time its WELCOME gives, so that an
editor that crashed leaves no zone kept alive; the editor sends HEARTBEAT more often than that.

A zone that follows a camera is moved once its centre is more than an eighth of its radius away
from the one sent, twice a second at most. However often it moves, the observer reads it at most
once in half of its period, the first reading of a new map aside.

## From the observer

| Kind | Name | Body |
|---|---|---|
| 101 | WELCOME | `u16` version, `u32` capabilities, string version of the server, string commit of AzerothCore, `f32` maximum radius, `u32` maximum entities, `u16` readings a second, `u16` seconds of silence before a connection is closed |
| 102 | REFUSED | string reason; the connection is closed after it |
| 103 | STATUS | `u8` state, `u32` map, `u32` instance, `f32` radius applied |
| 104 | SNAPSHOT | `u32` map, `u32` instance, `u64` sequence, `u32` count, entities |
| 105 | CHANGES | `u32` map, `u32` instance, `u64` sequence, `u32` count, entities that appeared or changed, `u32` count, `u64` GUIDs of the entities that left |

Capabilities: bit 0, reading. Nothing else in version 1.

States of STATUS:

| State | Meaning |
|---|---|
| 0 | waiting for an update of the map and instance asked for |
| 1 | active: the zone is read |
| 2 | no map with that id and instance was updated within 2 seconds: it does not exist, or the instance is gone |

After a SUBSCRIBE to a new map or instance: a STATUS 0, then a SNAPSHOT of the whole zone and a
STATUS 1 once the map was read, then a CHANGES each time a reading differs from the one before.
After a SUBSCRIBE that moves the zone, CHANGES only. Each reading of the zone increases the
sequence; a reading that changes nothing sends nothing. The observer reads the zone as many times
a second as WELCOME says. When more entities than the maximum are in the zone, the nearest to the
centre are kept.

An entity that follows a spline is sent again only when its spline changes (another id) or a field
other than its position, orientation and time along the spline changes: the editor places it from
the points of the spline, their times and the time gone at the reading, counting on from when it
received it.

## An entity

| Field | Type | |
|---|---|---|
| guid | u64 | the GUID at run time |
| kind | u8 | 1 creature, 2 game object, 3 player |
| entry | u32 | the entry of its template; 0 for a player |
| spawn | u32 | the guid of its row in `creature` or `gameobject`; 0 for none |
| flags | u8 | below |
| pool | u32 | the pool of its spawn; 0 for none |
| event | i16 | the game event of its spawn, negative when it stands while the event is off; 0 for none |
| phase | u32 | its phase mask |
| display | u32 | its display id |
| x, y, z | f32 | its position |
| orientation | f32 | in radians |
| scale | f32 | |
| rotation | 4 × f32 | game objects only: the quaternion x, y, z, w |
| state | u8 | game objects only: its state (0 active, 1 ready, 2 alternative) |
| path | u8 | points of the spline it follows sent, 32 at most; 0 when it does not move along one |
| spline | u32 | when path > 0: the id of the spline |
| spline flags | u8 | when path > 0: below |
| elapsed | u32 | when path > 0: the milliseconds gone along the spline at the reading |
| points | path × (3 × f32, u32) | when path > 0: x, y, z of each point, and the milliseconds from the start of the spline at which it is reached |
| name | string | 64 bytes at most |

The points are those the spline goes through, its first to its last: a Catmull-Rom spline has one
more point at each end, for control only, which is not sent. A spline of more than 32 points is sent
from the segment it is on, 32 points with their times.

Flags:

| Bit | Meaning |
|---|---|
| 0x01 | temporary: summoned, or spawned by a script, without a row in the database |
| 0x02 | dead, or a game object not spawned |
| 0x04 | walking |
| 0x08 | flying |
| 0x10 | moving along a spline |
| 0x20 | a game master |

Flags of a spline:

| Bit | Meaning |
|---|---|
| 0x01 | Catmull-Rom; linear otherwise |
| 0x02 | cyclic: it starts again from its first point |
| 0x04 | falling |
