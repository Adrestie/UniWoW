# The protocol of mod-uniwow-bridge, version 1

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
| 1 | HELLO | `u16` version (1), string token |
| 2 | RELOAD | `u32` id, `u16` count (256 at most), demands |

A demand is a `u8` action and a `u64` value:

| Action | | Value |
|---|---|---|
| 1 | the quests, their givers and enders, their texts in every language, their points of interest; the marks over the givers sent again to the players in the world | 0 |
| 2 | the waypoints of a path (`waypoint_data`) | its id |
| 3 | the addons of every creature (`creature_addon`): paths, mounts, auras | 0 |
| 4 | every group of creatures (`creature_formations`) | 0 |
| 5 | a creature read again from its row: taken away where it runs, spawned again where its row places it on a continent; taken away alone once its row is gone; read alone when a pool or a game event spawns it | its guid |
| 6 | a game object, as a creature | its guid |

The editor sends the demands in that order: what the creatures read, then the creatures spawned again.

## From the bridge

| Kind | Name | Body |
|---|---|---|
| 101 | WELCOME | `u16` version, string version of the server |
| 102 | REFUSED | string reason; the connection is closed after it |
| 103 | DONE | `u32` id of the RELOAD, `u16` count, for each demand a `u8` (1 made, 0 not) and a string of what the server says of it |

The demands are made on the world thread, after the maps were updated, where the commands of the
console are; DONE comes once they are, or after thirty seconds with each said not made in time.
