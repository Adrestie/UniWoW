# mod-uniwow-bridge

A module of AzerothCore that loads again, on the demand of the UniWoW editor running on the same
machine, what the editor wrote into the world database: its quests, the paths and groups of its
creatures, a creature or a game object moved, created or deleted. What the editor applies is then
seen in the game at once, without starting the server again. Its protocol is in
[PROTOCOL.md](PROTOCOL.md).

It is apart from `mod-uniwow-observer`, which reads and changes nothing: this one changes what the
server holds in memory, as the commands `.reload` and `.npc load` of AzerothCore do, and nothing in
the databases.

Licence: GPL 2.0 or later, as AzerothCore, into which it is compiled ([LICENSE](LICENSE)). The editor
speaks to it only through its protocol, over the network: they are two separate programs.

## What it does to the server

- It listens on 127.0.0.1 only, on a thread of its own, and asks for a token; an empty token refuses
  every connection.
- Its thread reads the demands and queues them; the world thread makes them at the end of its update,
  after the maps were updated, as it makes the commands of the console. No object of the game is
  touched by its network thread.
- The quests loaded again are those of `.reload all quest`, with their texts in every language. A
  client keeps the texts of the quests it was sent in its cache (`WDB`): a text changed is seen once
  its cache is emptied, a quest new at once.
- A creature or a game object spawned again is taken away where it runs and loaded from its row, as
  `.npc load` does: on a continent; in an instance, once the instance is made again. One that a pool
  or a game event spawns is read again and spawned by them.

## Installing

As the observer: the folder in `modules` of AzerothCore (a junction to this one, or a copy), CMake run
again for the module to be seen, the worldserver built again, `mod_uniwow_bridge.conf.dist` copied to
`configs/modules/mod_uniwow_bridge.conf` with its token, the worldserver started again. In the editor,
the panel *World database*: the port and the token of the bridge.
