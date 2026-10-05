# SPDX-License-Identifier: GPL-2.0-or-later
"""A client of the protocol of mod-uniwow-observer, for checking it on a running worldserver.

    probe.py watch --token T --map 1 --x 1629 --y -4373 --radius 200 --seconds 60
        subscribes to a zone, prints what comes each second, then what moved.
    probe.py move --token T --map 1 --x 1629 --y -4373 --radius 300 --speed 100 --seconds 30
        moves the zone as a camera flying along x would, subscribing again as PROTOCOL.md says.
    probe.py abuse --token T
        sends what a careful server must survive: messages corrupted, cut short, too long, out of
        order, 100 connections in a row, more connections than allowed, zones that do not exist.

Standard library only.
"""

import argparse
import math
import socket
import struct
import sys
import time

HELLO, SUBSCRIBE, UNSUBSCRIBE, HEARTBEAT = 1, 2, 3, 4
WELCOME, REFUSED, STATUS, SNAPSHOT, CHANGES = 101, 102, 103, 104, 105
KINDS = {1: "creature", 2: "game object", 3: "player"}
STATES = {0: "waiting", 1: "active", 2: "not found"}


def message(kind, body=b""):
    return struct.pack("<IB", len(body) + 1, kind) + body


def string(value):
    data = value.encode("utf-8")
    return struct.pack("<H", len(data)) + data


def hello(token, version=1):
    return message(HELLO, struct.pack("<H", version) + string(token))


def subscribe(map_id, instance, x, y, z, radius):
    return message(SUBSCRIBE, struct.pack("<IIffff", map_id, instance, x, y, z, radius))


class Body:
    def __init__(self, data):
        self.data, self.at = data, 0

    def take(self, fmt):
        values = struct.unpack_from("<" + fmt, self.data, self.at)
        self.at += struct.calcsize("<" + fmt)
        return values if len(values) > 1 else values[0]

    def string(self):
        length = self.take("H")
        value = self.data[self.at:self.at + length].decode("utf-8", "replace")
        self.at += length
        return value


def entity(body):
    e = {}
    (e["guid"], e["kind"], e["entry"], e["spawn"], e["flags"], e["pool"], e["event"], e["phase"],
     e["display"], e["x"], e["y"], e["z"], e["orientation"], e["scale"]) = body.take("QBIIBIhIIfffff")
    if e["kind"] == 2:
        e["rotation"] = body.take("ffff")
        e["state"] = body.take("B")
    count = body.take("B")
    e["path"], e["spline"] = [], 0
    if count:
        e["spline"], e["spline_flags"], e["elapsed"] = body.take("IBI")
        e["path"] = [body.take("fffI") for _ in range(count)]
    e["name"] = body.string()
    return e


class Connection:
    def __init__(self, port, timeout=5.0):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=timeout)
        self.received = 0

    def send(self, data):
        self.socket.sendall(data)

    def read_exactly(self, count):
        data = b""
        while len(data) < count:
            chunk = self.socket.recv(count - len(data))
            if not chunk:
                raise ConnectionError("closed by the observer")
            data += chunk
        return data

    def read(self):
        length = struct.unpack("<I", self.read_exactly(4))[0]
        data = self.read_exactly(length)
        self.received += 4 + length
        return data[0], Body(data[1:])

    def closed_by_peer(self, wait=3.0):
        """Whether the observer closes the connection within `wait` seconds, reading what comes."""
        self.socket.settimeout(wait)
        try:
            while True:
                if not self.socket.recv(65536):
                    return True
        except (ConnectionError, OSError):
            return True
        except socket.timeout:
            return False

    def close(self):
        try:
            self.socket.close()
        except OSError:
            pass


def welcome(connection):
    kind, body = connection.read()
    if kind == REFUSED:
        raise SystemExit("refused: " + body.string())
    assert kind == WELCOME, kind
    version, capabilities = body.take("HI")
    server, commit = body.string(), body.string()
    radius, entities, rate, heartbeat = body.take("fIHH")
    return dict(version=version, capabilities=capabilities, server=server, commit=commit,
                radius=radius, entities=entities, rate=rate, heartbeat=heartbeat)


def watch(args):
    connection = Connection(args.port)
    connection.send(hello(args.token))
    info = welcome(connection)
    print(f"welcome: protocol {info['version']}, {info['server']}, commit {info['commit']}, "
          f"radius {info['radius']:.0f}, {info['entities']} entities, {info['rate']} readings a second, "
          f"heartbeat {info['heartbeat']} s")
    connection.send(subscribe(args.map, args.instance, args.x, args.y, args.z, args.radius))
    connection.socket.settimeout(1.0)
    entities, first = {}, {}
    appeared = left = changed = messages = splines = 0
    start = last_line = last_beat = time.time()
    while time.time() - start < args.seconds:
        now = time.time()
        if now - last_beat >= 2.0:
            connection.send(message(HEARTBEAT))
            last_beat = now
        try:
            kind, body = connection.read()
        except socket.timeout:
            continue
        messages += 1
        if kind == STATUS:
            state, map_id, instance, radius = body.take("BIIf")
            print(f"status: {STATES.get(state, state)}, map {map_id} instance {instance}, radius {radius:.0f}")
        elif kind == SNAPSHOT:
            map_id, instance, sequence, count = body.take("IIQI")
            for _ in range(count):
                e = entity(body)
                entities[e["guid"]] = e
                first.setdefault(e["guid"], (e["x"], e["y"], e["z"]))
            print(f"snapshot {sequence}: {count} entities")
        elif kind == CHANGES:
            map_id, instance, sequence, count = body.take("IIQI")
            for _ in range(count):
                e = entity(body)
                before = entities.get(e["guid"])
                if before is None:
                    appeared += 1
                else:
                    changed += 1
                    if e["spline"] and e["spline"] != before["spline"]:
                        splines += 1
                e["moved"] = bool(before and (before.get("moved") or e["spline"] != before["spline"]
                                              or math.dist((e["x"], e["y"]), (before["x"], before["y"])) > 1.0))
                entities[e["guid"]] = e
                first.setdefault(e["guid"], (e["x"], e["y"], e["z"]))
            for _ in range(body.take("I")):
                guid = body.take("Q")
                entities.pop(guid, None)
                left += 1
        elif kind == REFUSED:
            print("refused:", body.string())
            return
        if now - last_line >= 1.0:
            elapsed = now - start
            moving = sum(1 for e in entities.values() if e["flags"] & 0x10)
            print(f"{elapsed:5.1f} s: {len(entities)} entities, {moving} moving; {appeared} appeared, "
                  f"{changed} changes, {splines} splines started, {left} left; "
                  f"{connection.received / elapsed:,.0f} bytes a second")
            last_line = now
    elapsed = time.time() - start
    by_kind = {}
    for e in entities.values():
        by_kind[KINDS.get(e["kind"], e["kind"])] = by_kind.get(KINDS.get(e["kind"], e["kind"]), 0) + 1
    moved = [e for guid, e in entities.items()
             if e.get("moved") or math.dist((e["x"], e["y"], e["z"]), first[guid]) > 1.0]
    print(f"after {elapsed:.0f} s: {by_kind}; {len(moved)} of them moved since first seen; {splines} splines "
          f"started; {appeared} appeared, {left} left; {messages} messages, "
          f"{connection.received / elapsed:,.0f} bytes a second")
    for e in moved[:args.show]:
        print(f"  moved: {e['name']} (entry {e['entry']}, spawn {e['spawn']})")
    connection.close()


def move(args):
    """The zone moved at `speed` yards a second along x, subscribed again once its centre is an
    eighth of its radius away from the one sent, twice a second at most."""
    connection = Connection(args.port)
    connection.send(hello(args.token))
    welcome(connection)
    connection.send(subscribe(args.map, args.instance, args.x, args.y, args.z, args.radius))
    sent_x, last_sent = args.x, time.time()
    subscribes, snapshots, changes, entities = 1, 0, 0, {}
    appeared = left = 0
    start = time.time()
    connection.socket.settimeout(0.05)
    while time.time() - start < args.seconds:
        now = time.time()
        x = args.x + args.speed * (now - start)
        if abs(x - sent_x) > args.radius / 8 and now - last_sent >= 0.5:
            connection.send(subscribe(args.map, args.instance, x, args.y, args.z, args.radius))
            sent_x, last_sent, subscribes = x, now, subscribes + 1
        try:
            kind, body = connection.read()
        except socket.timeout:
            continue
        if kind == SNAPSHOT:
            snapshots += 1
            _, _, _, count = body.take("IIQI")
            for _ in range(count):
                e = entity(body)
                entities[e["guid"]] = e
        elif kind == CHANGES:
            changes += 1
            _, _, _, count = body.take("IIQI")
            for _ in range(count):
                e = entity(body)
                appeared += e["guid"] not in entities
                entities[e["guid"]] = e
            for _ in range(body.take("I")):
                left += entities.pop(body.take("Q"), None) is not None
    elapsed = time.time() - start
    print(f"moved {args.speed * elapsed:.0f} yards in {elapsed:.0f} s: {subscribes} SUBSCRIBE, {snapshots} SNAPSHOT, "
          f"{changes} CHANGES; {appeared} entities entered, {left} left, {len(entities)} in the zone at the end; "
          f"{connection.received / elapsed:,.0f} bytes a second")
    connection.close()
    return 0 if snapshots == 1 else 1


def abuse(args):
    """Each case: what is sent, and whether the observer behaved: closed, refused or answered."""
    results = []

    def case(name, run):
        try:
            ok, detail = run()
        except Exception as error:  # noqa: BLE001: any failure of a case is reported, not raised
            ok, detail = False, f"{type(error).__name__}: {error}"
        results.append((name, ok, detail))
        print(f"{'ok  ' if ok else 'FAIL'} {name}: {detail}")

    def welcomed():
        connection = Connection(args.port)
        connection.send(hello(args.token))
        welcome(connection)
        return connection

    def closes(data, after_hello=True):
        def run():
            connection = welcomed() if after_hello else Connection(args.port)
            connection.send(data)
            closed = connection.closed_by_peer()
            connection.close()
            return closed, "closed by the observer" if closed else "left open"
        return run

    def refused(data):
        def run():
            connection = Connection(args.port)
            connection.send(data)
            kind, body = connection.read()
            reason = body.string() if kind == REFUSED else f"kind {kind}"
            closed = connection.closed_by_peer()
            connection.close()
            return kind == REFUSED and closed, reason
        return run

    def cut(data):
        def run():
            connection = welcomed()
            connection.send(data)
            connection.close()
            return True, "the client went away in the middle of a message"
        return run

    def zone(map_id, instance, expected):
        def run():
            connection = welcomed()
            connection.send(subscribe(map_id, instance, 0.0, 0.0, 0.0, 100.0))
            connection.socket.settimeout(5.0)
            states = []
            deadline = time.time() + 4.0
            while time.time() < deadline and expected not in states:
                kind, body = connection.read()
                if kind == STATUS:
                    states.append(body.take("B"))
            connection.close()
            return expected in states, "states " + ", ".join(STATES.get(s, str(s)) for s in states)
        return run

    def many():
        for _ in range(100):
            connection = welcomed()
            connection.send(subscribe(args.map, 0, args.x, args.y, args.z, 50.0))
            connection.close()
        return True, "100 connected, welcomed, subscribed and gone"

    def too_many():
        held = [welcomed() for _ in range(args.max_connections)]
        extra = Connection(args.port)
        kind, body = extra.read()
        reason = body.string() if kind == REFUSED else f"kind {kind}"
        extra.close()
        for connection in held:
            connection.close()
        return kind == REFUSED, reason

    case("a wrong token", refused(hello("not the token")))
    case("a version not served", refused(hello(args.token, version=99)))
    case("SUBSCRIBE before HELLO", closes(subscribe(0, 0, 0, 0, 0, 10), after_hello=False))
    case("random bytes before HELLO", closes(bytes(range(256)) * 4, after_hello=False))
    case("a length of zero", closes(struct.pack("<I", 0)))
    case("a length too long", closes(struct.pack("<IB", 5000, HEARTBEAT) + b"\0" * 64))
    case("an unknown kind", closes(message(77, b"abc")))
    case("a SUBSCRIBE too short", closes(message(SUBSCRIBE, b"\1\2\3")))
    case("a SUBSCRIBE too long", closes(message(SUBSCRIBE, struct.pack("<IIffff", 0, 0, 0, 0, 0, 10) + b"x")))
    case("a HEARTBEAT with a body", closes(message(HEARTBEAT, b"x")))
    case("a second HELLO", closes(hello(args.token)))
    case("a centre outside the world", closes(subscribe(0, 0, 1e9, 0, 0, 10)))
    case("a radius not a number", closes(subscribe(0, 0, 0, 0, 0, float("nan"))))
    case("a negative radius", closes(subscribe(0, 0, 0, 0, 0, -5)))
    case("cut in the middle of a header", cut(b"\x10\x00"))
    case("cut in the middle of a body", cut(struct.pack("<IB", 25, SUBSCRIBE) + b"\0" * 6))
    case("a map that does not exist", zone(9999, 0, 2))
    case("an instance that does not exist", zone(33, 99999, 2))
    case("100 connections in a row", many)
    case(f"more than {args.max_connections} connections", too_many)
    case("a connection afterwards", lambda: (bool(welcomed()), "welcomed"))
    failed = [name for name, ok, _ in results if not ok]
    print(f"{len(results) - len(failed)} of {len(results)} cases as expected" + (f"; not: {failed}" if failed else ""))
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["watch", "move", "abuse"])
    parser.add_argument("--port", type=int, default=8087)
    parser.add_argument("--token", required=True)
    parser.add_argument("--map", type=int, default=1)
    parser.add_argument("--instance", type=int, default=0)
    parser.add_argument("--x", type=float, default=1629.0)
    parser.add_argument("--y", type=float, default=-4373.0)
    parser.add_argument("--z", type=float, default=30.0)
    parser.add_argument("--radius", type=float, default=200.0)
    parser.add_argument("--seconds", type=float, default=30.0)
    parser.add_argument("--speed", type=float, default=100.0, help="yards a second, for move")
    parser.add_argument("--show", type=int, default=5, help="entities that moved, listed at the end")
    parser.add_argument("--max-connections", type=int, default=4)
    args = parser.parse_args()
    if args.command == "watch":
        watch(args)
        return 0
    if args.command == "move":
        return move(args)
    return abuse(args)


if __name__ == "__main__":
    sys.exit(main())
