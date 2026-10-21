# The protocol of mod-uniwow-bridge, version 2

SPDX-License-Identifier: GPL-2.0-or-later

The bridge loads again, on the demand of the editor, what the editor wrote into the world database,
over TCP on 127.0.0.1. One editor at a time; a connection may ask as many times as it wants.

## Framing

As the observer's: every message is a `u32` length of what follows, a `u8` kind, then its body. All
numbers little-endian; a string a `u16` length in bytes, then its bytes, UTF-8. A message from the
editor is 4,096 bytes at most. A message too long, of an unknown kind, out of order or whose body is
not exactly what its kind says closes the connection, nothing more; so does ten seconds of silence.

## From the editor

| Kind | Name | Body |
|---|---|---|
| 1 | HELLO | `u16` version (1 or 2), string token |
| 2 | RELOAD | `u32` id, `u16` count (256 at most), demands |
| 3 | MOVE | version 2: `u32` id, `u16` count (64 at most), moves |

The bridge of version 2 welcomes an editor of the version 1 and answers its WELCOME with its own
version: an editor sends MOVE and the demand 8 only to a bridge that said 2, and asks a bridge of the
version 1 for the demand 5 in place of the 8.

A demand is a `u8` action and a `u64` value:

| Action | | Value |
|---|---|---|
| 1 | the quests, their givers and enders, their texts in every language, their points of interest; the marks over the givers sent again to the players in the world | 0 |
| 2 | the waypoints of a path (`waypoint_data`), the creatures walking it set on it again from its first point | its id |
| 3 | the addons of every creature (`creature_addon`): paths, mounts, auras | 0 |
| 4 | every group of creatures (`creature_formations`) | 0 |
| 5 | a creature read again from its row: taken away where it runs, spawned again where its row places it on a continent; taken away alone once its row is gone; read alone when a pool or a game event spawns it | its guid |
| 6 | a game object, as a creature | its guid |
| 7 | a quest deleted, taken from every character: out of the quest log of those in the world (as `.quest remove`), and out of `character_queststatus`, `_rewarded`, `_daily`, `_weekly`, `_monthly` and `_seasonal` in the database, for a quest of its id made again to be offered to them | its id |
| 8 | version 2: the spawn point of a creature read again from its row: the creatures of it running keep their place and take it as their home, going there as they come back; as the demand 5 when its row is gone or on another map | its guid |

A move puts a creature in the world where the editor drags it, live: nothing is written in the
databases.

| Field | Type | |
|---|---|---|
| map, instance | 2 × u32 | where it is, as the observer sends them |
| guid | u64 | its GUID at run time, as the observer sends it |
| let go | u8 | 0 held, 1 let go |
| x, y, z, orientation | 4 × f32 | where it is put |

A creature held stands still where it is put, its movement stopped; let go, it is put there and moves
again as its own (its path, its wandering). One the editor has not moved for five seconds is let go
where it stands, for an editor closed while it dragged one to leave none standing still.

The editor sends the demands in that order: what the creatures read, then the creatures spawned again.

## From the bridge

| Kind | Name | Body |
|---|---|---|
| 101 | WELCOME | `u16` version, string version of the server |
| 102 | REFUSED | string reason; the connection is closed after it |
| 103 | DONE | `u32` id of the RELOAD or MOVE, `u16` count, for each demand or move a `u8` (1 made, 0 not) and a string of what the server says of it |

The demands are made on the world thread, after the maps were updated, where the commands of the
console are; DONE comes once they are, or after thirty seconds with each said not made in time.
