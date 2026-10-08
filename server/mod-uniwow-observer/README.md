# mod-uniwow-observer

A module of AzerothCore that streams, read only, the creatures, game objects and players of a zone
of a map to the UniWoW editor running on the same machine. It changes no data of the game, spawns
nothing and offers no command; its protocol is in [PROTOCOL.md](PROTOCOL.md).

Licence: GPL 2.0 or later, as AzerothCore, into which it is compiled ([LICENSE](LICENSE)). The editor
speaks to it only through its protocol, over the network: they are two separate programs.

## What it does to the server

- It listens on 127.0.0.1 only, on a thread of its own, and asks for a token.
- It reads a zone at the end of the update of its map, on the thread updating that map. No object
  of the game is touched by its network thread.
- The zone looked at stays alive without a player: at each update of its map, the creatures and game
  objects the last reading sent, the `MaxEntities` nearest, are put back in the map's list of
  objects to update, as the sight of a player does. They move, follow their paths and respawn as they would near a player, which
  costs the server some work. `UniwowObserver.KeepAlive = 0` turns that off.
- The grids of a zone are loaded when it is subscribed to. **They stay loaded until the server
  starts again**, as those a player crossed (AzerothCore unloads grids only when a map is unloaded
  whole): flying over the whole world in the editor loads all it flew over.
- A zone is a circle of one grid at most, 533 yards. A dungeon or a battleground is seen while its
  instance exists; the observer does not keep an instance alive.
- The zone is kept no longer once the editor unsubscribes, disconnects, or sends nothing for the
  time `UniwowObserver.Heartbeat` sets.
- Any message malformed, too long or out of order closes its connection, nothing more; the
  connections, the size of the messages and what waits to be written are bounded.

## Supported AzerothCore

Built and checked against AzerothCore commit `bc9198ce7` (azerothcore-wotlk). A newer commit is a
change of this module, checked again.

## Building

The module stays in the repository of UniWoW and is seen by AzerothCore through a junction:

```
mklink /J <azerothcore>\modules\mod-uniwow-observer <uniwow>\server\mod-uniwow-observer
```

Then, in the build folder of AzerothCore, as for any module:

```
cmake .
cmake --build . --config RelWithDebInfo --target modules
cmake --build . --config RelWithDebInfo --target worldserver
```

The second build links `worldserver.exe`, which must not be running. The build copies
`conf/mod_uniwow_observer.conf.dist` beside the other settings of the modules
(`configs/modules/`); copy it as `mod_uniwow_observer.conf` and set `UniwowObserver.Token`, without
which every connection is refused. The settings are read when the worldserver starts.

## Going back

Before linking the module, save `worldserver.exe` (with its `.pdb`) and the folder `configs`. To go
back: stop the worldserver, put the saved `worldserver.exe` and `.pdb` back, remove the junction
`modules\mod-uniwow-observer` and `configs\modules\mod_uniwow_observer.conf*`, and run `cmake .` before
the next build. The module writes nothing in the databases: there is nothing to undo there.
Alternatively, `UniwowObserver.Enable = 0` keeps it built but idle.

## Checking it

`tools/probe.py` (Python 3, standard library) is a client of the protocol:

```
python tools/probe.py watch --token <token> --map 1 --x 1629 --y -4373 --radius 200 --seconds 60
python tools/probe.py move --token <token> --map 1 --x 1629 --y -4373 --radius 300 --speed 100
python tools/probe.py abuse --token <token>
```

`watch` prints what it receives each second and, at the end, which entities moved. `move` moves the
zone as a flying camera would, subscribing again as `PROTOCOL.md` says: one SNAPSHOT, then CHANGES
only. `abuse` sends what the module must survive: messages corrupted, cut short, too long or out of
order, 100 connections in a row, more connections than allowed, zones that do not exist; the
worldserver goes on. With
`UniwowObserver.StatsInterval` set, the log gives, per connection, the time the module took in the
update of its map, reading and keeping apart, on average and at most, and the bytes sent a second.
