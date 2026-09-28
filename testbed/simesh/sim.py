"""The hold on one running simulation that the script library runs on
(simesh.library, which a script uses; this is async, on the library's own
loop): the stations, the clock, and what to do with them.

```
library ── ws /ws?sim=<name>&quiet=1 ────────────────────► front ─► that simulation's simd
simd ── snapshot {nodes, clock, …} ─────────────────────► library   the stations, T
library ── meta {verb: announce, names: [..], stagger: 300, id: 7} ─► simd
simd ── command_result {id: 7, results: {name: reply}, t} ─► library
library ── firmware | first_boot {rules, id} ─────────────► simd
```

**Time** is the run's own: `sim.run_s` is seconds since the hold began
(or since the load it asked for), and `until`, `after` and `spread` are
seconds of it, so a real-time and a virtual-time run act at the same
instants of the run.

**A selection** is some of the stations: `sim.nodes(tag=, firmware=, kind=,
role=, names=)`, or `sim.node(name)` for one. What is done to a selection is
done to each member that is up, spread over `spread` seconds and held back
`after` seconds, and answers {name: reply}:

- `run(line, kind=)`: a line in the stations' own language, macros filled in;
  a selection of more than one kind refuses it unless `kind` narrows it;
- the intents, which each station's kind says in its own lines:
  `set_name()`, `set_role(role)`, `set_radio(**radio)`, `announce()`,
  `message(to, text)`, `path(to)`, `peer_tcp(to, port)`, `address()`; a kind
  that cannot say one answers `! …` for that station;
- `reset()`, `factory_reset()`: pressed on each.

`attach(name)` holds a running simulation; `start(geodata, nodesets, …)`
asks the front for a new one, with its time and its firmware and first-boot
rules, and holds it.
"""

import asyncio
import collections
import itertools
import json
import os
import random
import time

import aiohttp

DEFAULT_PORT = int(os.environ.get("SIMESH_PORT") or 8800)


class SimError(Exception):
    """The front or the simulation refused, or the simulation went away."""


class Station:
    """What the driver knows of one station, from simd's `node` messages."""

    def __init__(self, msg):
        self.update(msg)

    def update(self, msg):
        self.name = msg["name"]
        self.id = msg.get("id")
        self.tags = list(msg.get("tags") or ())
        self.firmware = msg.get("firmware")
        self.kind = msg.get("kind")
        # What it reports, else its role tag (nodeset.tag_role).
        self.role = msg.get("role") or next(
            (t for t in self.tags if t in ("transport", "router", "repeater")), None)
        self.lat, self.lon = msg.get("lat"), msg.get("lon")
        self.height_m = msg.get("height_m")
        self.antenna = (msg.get("antenna") or {}).get("type")
        self.max_dbm = msg.get("max_dbm")
        self.status = msg.get("status")

    def facts(self):
        """What a selection (simesh.select) can ask of it."""
        return {"name": self.name, "id": self.id, "tags": self.tags, "firmware": self.firmware,
                "kind": self.kind, "role": self.role, "antenna": self.antenna,
                "max_dbm": self.max_dbm, "lat": self.lat,
                "lon": self.lon, "height_m": self.height_m, "status": self.status}

    def __repr__(self):
        return "<station %s #%s %s %s>" % (self.name, self.id, self.kind, self.status)


class Selection:
    """Some of the stations, by name, and what can be done to all of them."""

    def __init__(self, sim, names):
        self.sim = sim
        self.names = list(names)

    def __iter__(self):
        return (self.sim.stations[n] for n in self.names if n in self.sim.stations)

    def __len__(self):
        return len(self.names)

    def __repr__(self):
        return "<selection of %d: %s>" % (len(self.names), " ".join(self.names[:8]))

    async def run(self, line, spread=0.0, after=0.0, kind=None):
        return await self.sim.ask({"type": "command", "line": line}, self.names,
                                  spread, after, kind)

    async def intent(self, verb, spread=0.0, after=0.0, **args):
        return await self.sim.ask({"type": "meta", "verb": verb, "args": args}, self.names,
                                  spread, after)

    async def set_name(self, **when):
        return await self.intent("name", **when)

    async def set_role(self, role, **when):
        return await self.intent("role", role=role, **when)

    async def set_radio(self, spread=0.0, after=0.0, **radio):
        return await self.intent("radio", spread=spread, after=after, **radio)

    async def announce(self, **when):
        return await self.intent("announce", **when)

    async def message(self, to, text, **when):
        return await self.intent("message", to=to, text=text, **when)

    async def path(self, to, **when):
        return await self.intent("path", to=to, **when)

    async def peer_tcp(self, to, port=4965, **when):
        return await self.intent("peer_tcp", to=to, port=port, **when)

    async def address(self):
        return await self.intent("address")

    async def reset(self):
        for name in self.names:
            await self.sim.send({"type": "node_reset", "name": name})

    async def factory_reset(self):
        for name in self.names:
            await self.sim.send({"type": "node_factory_reset", "name": name})


class Sim:
    """One simulation, over one quiet websocket through the front."""

    def __init__(self, ws, session, name, port):
        self.ws = ws
        self.session = session
        self.name = name
        self.port = port
        self.stations = {}
        self.run = {}                   # the run as simd describes it: dir, geodata, nodeset, …
        self.t = 0
        self.t_zero = None
        self.up = {}
        self.loaded = asyncio.get_running_loop().create_future()
        self.waiting = {}
        # Every page and driver hears every answer, so an id is this driver's own.
        prefix = os.urandom(4).hex()
        self.ids = ("%s-%d" % (prefix, n) for n in itertools.count(1))
        self.rng = random.Random()
        self.result = {"clock": []}      # what a driver that keeps a record reads
        self.errors = collections.deque(maxlen=50)
        self.reader = None
        self.firmware_tasks = []         # firmware said and not yet awaited

    # ---- the stream ------------------------------------------------------

    async def read(self):
        async for msg in self.ws:
            if msg.type != aiohttp.WSMsgType.TEXT:
                continue
            m = json.loads(msg.data)
            kind = m.get("type")
            if kind == "snapshot":
                self.run = m.get("run") or {}
                self.stations = {n["name"]: Station(n) for n in m.get("nodes") or ()}
                if m.get("clock"):
                    self.t = m["clock"]["t"]
                if m.get("run") and not self.loaded.done():
                    self.t_zero = self.t
                    self.loaded.set_result(m)
            elif kind == "clock":
                self.t = m["t"]
                self.result["clock"].append([round(time.time(), 3), m["t"], m.get("barriers"),
                                             m.get("observed"), m.get("slow_idles")])
            elif kind == "node":
                if m["name"] in self.stations:
                    self.stations[m["name"]].update(m)
                else:
                    self.stations[m["name"]] = Station(m)
                if m.get("status") == "up" and self.t_zero is not None:
                    self.up.setdefault(m["name"], [time.time(), self.t])
            elif kind == "node_gone":
                self.stations.pop(m.get("name"), None)
            elif kind == "command_result":
                waiter = self.waiting.pop(m.get("id"), None)
                if waiter is not None and not waiter.done():
                    waiter.set_result(m)
            elif kind == "error":
                self.errors.append(m.get("text"))
                print("simd: %s" % m.get("text"), flush=True)
        for waiter in self.waiting.values():
            if not waiter.done():
                waiter.set_exception(SimError("simulation %s went away" % self.name))

    async def send(self, msg):
        await self.ws.send_str(json.dumps(msg))

    @property
    def run_dir(self):
        """The run's directory, where a driver's own output belongs."""
        return self.run.get("dir")

    # ---- time ------------------------------------------------------------

    @property
    def run_s(self):
        """Seconds of the run's clock since this driver attached."""
        return (self.t - (self.t_zero or 0)) / 1e6

    async def until(self, run_s):
        """Until the run's clock reaches `run_s` seconds after attaching."""
        while self.run_s < run_s:
            await asyncio.sleep(0.2)

    async def sleep(self, seconds):
        await self.until(self.run_s + seconds)

    async def all_up(self):
        """Until every station is up."""
        while not self.stations or any(s.status != "up" for s in self.stations.values()):
            await asyncio.sleep(0.2)

    async def plan(self, *phases):
        """What is left of the run: (name, until) pairs, until in seconds of
        run time after attaching. The page turns it into a phase, its
        progress, and a finish time."""
        await self.send({"type": "plan", "phases": [
            {"name": name, "until": (self.t_zero or 0) + int(until * 1e6)}
            for name, until in phases]})

    # ---- choosing --------------------------------------------------------

    def nodes(self, tag=None, firmware=None, kind=None, role=None, names=None):
        """The stations with every one of these that is given, in id order."""
        chosen = []
        for s in sorted(self.stations.values(), key=lambda s: s.id or 0):
            if tag is not None and tag not in s.tags:
                continue
            if firmware is not None and s.firmware != firmware:
                continue
            if kind is not None and s.kind != kind:
                continue
            if role is not None and s.role != role:
                continue
            if names is not None and s.name not in names:
                continue
            chosen.append(s.name)
        return Selection(self, chosen)

    def node(self, name):
        if name not in self.stations:
            raise SimError("no station called %s" % name)
        return Selection(self, [name])

    def facts(self):
        """Every station's facts, for a selection (simesh.select): {name: facts}."""
        return {name: s.facts() for name, s in self.stations.items()}

    # ---- firmware --------------------------------------------------------

    def firmware(self, rules):
        """Firmware rules said to the simulation, [{which, firmware}]: a task
        to await, which everything that needs a station up awaits first."""
        task = asyncio.ensure_future(self._firmware(rules))
        self.firmware_tasks.append(task)
        return task

    async def _firmware(self, rules):
        return await self._rules("firmware", rules)

    async def first_boot(self, rules):
        """First-boot rules said to the simulation, [{which, lines}]: kept
        with the run, for every station that boots with no state from now on."""
        return await self._rules("first_boot", rules)

    async def _rules(self, verb, rules):
        ident = next(self.ids)
        waiter = asyncio.get_running_loop().create_future()
        self.waiting[ident] = waiter
        await self.send({"type": verb, "rules": list(rules), "id": ident})
        reply = await waiter
        if reply.get("error"):
            raise SimError(reply["error"])
        return reply.get("results") or {}

    async def ready(self, selection, timeout=None):
        """The names of a selection's stations once every one is up, after
        any firmware said is in. A station that runs nothing is refused: it
        needs firmware() first."""
        if self.firmware_tasks:
            tasks, self.firmware_tasks = self.firmware_tasks, []
            await asyncio.gather(*tasks)
        names = selection.pick(self.facts())
        bare = [n for n in names if not self.stations[n].firmware]
        if bare:
            raise SimError("%s run%s no firmware: say firmware() for %s first"
                           % (", ".join(bare), "s" if len(bare) == 1 else "",
                              "it" if len(bare) == 1 else "them"))
        loop = asyncio.get_running_loop()
        deadline = None if timeout is None else loop.time() + timeout
        while any(self.stations[n].status != "up" for n in names if n in self.stations):
            if deadline is not None and loop.time() > deadline:
                raise SimError("not up after %.0f s: %s" % (timeout, ", ".join(
                    n for n in names if self.stations[n].status != "up")))
            await asyncio.sleep(0.2)
        return [n for n in names if n in self.stations]

    def pairs(self, selection=None, sample=None, seed=None):
        """Ordered pairs of distinct stations (of `selection`, else all), each
        once; `sample` of them drawn with `seed`, in draw order."""
        names = list((selection or self.nodes()).names)
        every = [(a, b) for a in names for b in names if a != b]
        if sample is None or sample >= len(every):
            return every
        return random.Random(seed).sample(every, sample)

    # ---- asking ----------------------------------------------------------

    async def ask(self, msg, names, spread=0.0, after=0.0, kind=None):
        """One command or intent on the named stations: {name: reply}."""
        ident = next(self.ids)
        waiter = asyncio.get_running_loop().create_future()
        self.waiting[ident] = waiter
        msg = dict(msg, id=ident, names=list(names))
        if spread:
            msg["stagger"] = float(spread)
        if after:
            msg["after"] = float(after)
        if kind:
            msg["kind"] = kind
        await self.send(msg)
        return (await waiter)["results"]

    async def sequence(self, steps, names, after=0.0):
        """Several commands and intents on the named stations, one after the
        other at one T inside simd: ([{name: reply}], one per step; the T in
        µs they were done at). Each step is a `command` ({type, line}) or a
        `meta` ({type, verb, args})."""
        ident = next(self.ids)
        waiter = asyncio.get_running_loop().create_future()
        self.waiting[ident] = waiter
        msg = {"type": "sequence", "steps": list(steps), "id": ident, "names": list(names)}
        if after:
            msg["after"] = float(after)
        await self.send(msg)
        reply = await waiter
        return reply["results"], reply.get("t")

    async def command(self, line, name=None, after=0.0, stagger=0.0, kind=None):
        """One line on one station, or on every station of `kind`: the whole
        `command_result`, with its `t`."""
        ident = next(self.ids)
        waiter = asyncio.get_running_loop().create_future()
        self.waiting[ident] = waiter
        msg = {"type": "command", "line": line, "id": ident}
        for key, value in (("name", name), ("kind", kind)):
            if value:
                msg[key] = value
        if after > 0:
            msg["after"] = after
        if stagger > 0:
            msg["stagger"] = stagger
        await self.send(msg)
        return await waiter

    async def snapshot(self, name):
        """This moment, flushed, as a snapshot."""
        await self.send({"type": "snapshot_save_as", "name": name})

    save_snapshot = snapshot

    async def move(self, name, lat, lon, height_m=None):
        msg = {"type": "nodeset_move", "name": name, "lat": lat, "lon": lon, "settle": True}
        if height_m is not None:
            msg["height_m"] = height_m
        await self.send(msg)

    def wait_run(self, at):
        return self.until(at)

    async def stop(self):
        """Stop the simulation itself, through the front."""
        await front_verb(self.session, self.port, {"type": "sim_stop", "name": self.name})

    async def pause(self):
        """Stop the simulation with its state kept, through the front: it
        stays listed as paused, to be resumed as it ended."""
        await front_verb(self.session, self.port, {"type": "sim_pause", "name": self.name})

    async def close(self):
        if self.reader is not None:
            self.reader.cancel()
        await self.ws.close()


async def front_verb(session, port, msg, timeout=None):
    """One of the front's own verbs on a socket of its own: its answer."""
    async with session.ws_connect("http://127.0.0.1:%d/ws" % port, max_msg_size=0) as ws:
        await ws.send_str(json.dumps(msg))
        async for raw in ws:
            if raw.type != aiohttp.WSMsgType.TEXT:
                continue
            reply = json.loads(raw.data)
            if reply.get("type") == msg["type"] and "ok" in reply and reply.get("sim") is None:
                if not reply["ok"]:
                    raise SimError(reply.get("error") or "%s refused" % msg["type"])
                return reply
    raise SimError("the front went away")


async def attach(name=None, port=None, session=None):
    """Hold a running simulation: `name`, else `$SIMESH_SIM`."""
    name = name or os.environ.get("SIMESH_SIM")
    if not name:
        raise SimError("attach to which simulation? name one, or run under the front")
    port = port or DEFAULT_PORT
    session = session or aiohttp.ClientSession()
    ws = await session.ws_connect("http://127.0.0.1:%d/ws?quiet=1&sim=%s" % (port, name),
                                  max_msg_size=0)
    sim = Sim(ws, session, name, port)
    sim.reader = asyncio.ensure_future(sim.read())
    await asyncio.wait_for(asyncio.shield(sim.loaded), 60)
    return sim


async def start(geodata, nodesets, script=None, time="real", name=None, build=None,
                firmware_rules=None, first_boot_rules=None, port=None, session=None):
    """A new simulation from geodata and one or more nodesets (several are
    merged), its firmware and first-boot rules given from the start, started
    by the front and held. `script` is the name of the script it keeps a
    copy of."""
    port = port or DEFAULT_PORT
    session = session or aiohttp.ClientSession()
    layers = [nodesets] if isinstance(nodesets, str) else list(nodesets)
    msg = {"type": "sim_new", "geodata": geodata, "nodesets": layers, "time": time,
           "firmware_rules": list(firmware_rules or []),
           "first_boot_rules": list(first_boot_rules or [])}
    for key, value in (("script", script), ("name", name), ("build", build)):
        if value:
            msg[key] = value
    reply = await front_verb(session, port, msg)
    return await attach(reply["name"], port, session)
