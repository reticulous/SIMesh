"""What a script says to a simulation and its stations, whatever firmware they run.

    '''smoke: an LXMF identity everywhere, then an announce.'''
    from simesh import *

    time("real")
    firmware("all", "reticulous_dev_latest")
    include("scripts/startup.py")
    on_first_boot("all", '''
        lxmf create {name}
    ''')

    up("all")
    announce(nodes(tag="lora"), spread=60)
    wait(600)
    send_msg("gw02", "internet", "hello")

A script is plain Python, run from its top to its end (simesh.runner):
every call below does what it says and returns when it has, so a script is
read as it runs. Time is the run's own clock, so a real-time and a
virtual-time run act at the same instants of the run.

**Declarations**, before anything is done:

- `time(mode)`: `"real"`, `"max"` (virtual time as fast as the stations
  allow) or a number, virtual time paced at that many times the wall.
  Real by default. It is the simulation's for its whole life.
- `firmware(which, device)`: what the nodes run, a device as the Devices tab
  names it (`reticulous_dev_latest`, `reticulous_dev_20260927140352`). Each
  node runs what the last rule matching it names; a node no rule matches
  runs nothing.
- `on_first_boot(which, cmds)`: what each matching station is given the
  first time it boots with no state (every station of a new simulation, a
  node placed later, a station after a factory reset), after its name.
  `cmds` is lines in the station's own language, or intents said in each
  kind's own lines, or a list of both: `radio(freq_mhz, sf, bw_khz, cr,
  tx_dbm, sync, preamble)` sets slot 0 (`tx_dbm="max"` is each node's own
  maximum), `radio_up()` starts it, after whatever it reads when it starts,
  and `role(name)` sets its role. Rules apply in order.

Nothing about a station is said to it but what a script says: the radio,
the role and the rest come from these rules, most of them from
`scripts/startup.py`, which a script includes:

- `include(path, missing_ok=False)`: another file's code, run here as if
  written at this point; `path` is under testbed/.
- `nodesets()`: the names of the nodesets the script's world is made of,
  for including each one's own setup (`nodesets/<name>.py`).

The first thing a script does (or its end) starts the simulation with
those: on the Nodes tab's geodata and nodesets when the page ran it, or on
`--geodata`/`--nodeset` from a shell. A script run on a running simulation
(`--sim`) says its firmware and first-boot rules to that one instead, and
cannot change its time. Said later, `firmware()` and `on_first_boot()` apply
to the running simulation at once: a node whose firmware changes is
restarted on it, its state kept.

**What is done** (each waits for the nodes it names to be up first; a node
with no firmware refuses):

- `exec(which, cmds, pause=0)`: lines in each node's own language, macros
  filled in (`{name}`, `{id}`, `{addr}`, `{addr:<node>}`, `{max_dbm}`). The
  nodes go together, or one after another `pause` seconds apart. Answers
  {node: what it printed}.
- meta commands, said in each kind's own lines: `announce(which,
  spread=0)`, `max_tx_pwr(which, dbm=None)` (each node's own maximum when no
  figure is given), `send_msg(from_node, to_node, text)`.
- `reset(which)`, `factory_reset(which)`: pressed on each.
- `up(which)`: until they are all up. `wait(seconds)`, `until(seconds)`,
  `now()`: the run's clock, in seconds since the script's simulation began.
- `phase(name, until)…`: what the run is doing, for the page's estimate.
- `snapshot(name)`, `move(node, lat, lon)`, `pause()`, `stop()`.
- `stations()`, `station(name)`: each node's facts, its `max_dbm` among them.

`send_msg` and `exec` take `after=` (seconds on the run's clock before it is
done) and `wait=False`, which returns at once with a future whose
`result()` is the answer: how a driver puts many things on the clock at once.

**Which nodes** is `"all"`, a node's name, a list of names, or a selection,
`nodes(field=value, …)` (simesh.select), combined with `&`, `|`, `-`, `~`.
"""

import asyncio
import builtins
import os
import textwrap
import threading

from simesh import sim as sim_module
from simesh.select import Nodes, nodes, which  # noqa: F401 - the library's face

TIME_MODES = ("real", "max")
TESTBED_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class ScriptError(Exception):
    """A script asked for something that cannot be done."""


class Intent:
    """Something a station is told in its own kind's lines, whatever its
    firmware: what `radio()` and `role()` give `on_first_boot()`. As data
    it is {"intent": verb, "args": {…}}, which simd says through the kind."""

    def __init__(self, verb, **args):
        self.verb = verb
        self.args = {k: v for k, v in args.items() if v is not None}

    def to_json(self):
        return {"intent": self.verb, "args": dict(self.args)}

    def __repr__(self):
        return "%s(%s)" % (self.verb, ", ".join("%s=%r" % kv for kv in self.args.items()))


def lines_of(cmds):
    """What `cmds` is, in order: a string of one or more lines, an Intent,
    or a list of either. A line is kept as its text, an intent as its data."""
    blocks = [cmds] if isinstance(cmds, (str, Intent)) else list(cmds)
    out = []
    for block in blocks:
        if isinstance(block, Intent):
            out.append(block.to_json())
            continue
        for line in textwrap.dedent(str(block)).splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                out.append(line)
    return out


def time_mode(mode):
    """A time() argument as simd spells it: real, max or `<k>x`."""
    if mode in TIME_MODES:
        return mode
    try:
        rate = float(str(mode).rstrip("x"))
    except ValueError:
        raise ScriptError("time() is \"real\", \"max\" or a pace such as 10, not %r"
                          % (mode,)) from None
    if rate <= 0:
        raise ScriptError("a pace is more than 0")
    return "%gx" % rate


class Runtime:
    """One script's hold on its simulation: the declarations until it is
    started, then the simulation, its websocket on an event loop of its own
    in a thread beside the script's."""

    def __init__(self):
        self.world = {}             # geodata, nodesets, name, build, port, script, sim
        self.time = None
        self.firmware_rules = []
        self.first_boot_rules = []
        self.sim = None
        self.loop = None
        self.thread = None
        self.lock = threading.Lock()

    def configure(self, **world):
        self.world = {k: v for k, v in world.items() if v is not None}

    # ---- the loop ---------------------------------------------------------

    def call(self, coro, wait=True):
        """Run `coro` on the loop: its result, or with `wait` false a future."""
        if self.loop is None:
            self.loop = asyncio.new_event_loop()
            self.thread = threading.Thread(target=self.loop.run_forever, name="simesh-loop",
                                           daemon=True)
            self.thread.start()
        future = asyncio.run_coroutine_threadsafe(coro, self.loop)
        return future.result() if wait else future

    def held(self):
        """The simulation, started (or attached to) on first need."""
        with self.lock:
            if self.sim is None:
                self.sim = self.call(self._begin())
            return self.sim

    async def _begin(self):
        world = self.world
        port = world.get("port")
        attach_to = world.get("sim") or (os.environ.get("SIMESH_SIM") if not world else None)
        if attach_to:
            if self.time is not None:
                raise ScriptError("time() is a new simulation's: %s runs as it was started"
                                  % attach_to)
            sim = await sim_module.attach(attach_to, port)
            if self.firmware_rules:
                await sim.firmware(self.firmware_rules)
            if self.first_boot_rules:
                await sim.first_boot(self.first_boot_rules)
            return sim
        if not world.get("geodata") or not world.get("nodesets"):
            raise ScriptError("this script has no simulation: run it from the Scripts tab, or "
                              "with simesh run SCRIPT --geodata G --nodeset N (or --sim S)")
        if not self.firmware_rules:
            raise ScriptError("this script says no firmware(), so no node would run anything: "
                              "a script that is included by others (startup.py) is run "
                              "through one of them")
        return await sim_module.start(
            world["geodata"], world["nodesets"], world.get("script"), self.time or "real",
            world.get("name"), world.get("build"), self.firmware_rules, self.first_boot_rules,
            port)

    def close(self):
        if self.loop is None:
            return
        if self.sim is not None:
            self.call(self._close())
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(5)
        self.loop = self.sim = None

    async def _close(self):
        await self.sim.close()
        await self.sim.session.close()


runtime = Runtime()


def _sim():
    return runtime.held()


def _names(which_nodes):
    return which(which_nodes).pick(_sim().facts())


# ---- declarations ------------------------------------------------------------

def time(mode):  # noqa: A001 - the library's own word
    """How the new simulation keeps time: "real", "max", or a pace."""
    if runtime.sim is not None:
        raise ScriptError("time() comes before anything is done: the simulation keeps the "
                          "time it was started with")
    runtime.time = time_mode(mode)


def firmware(which_nodes, device):
    """What the nodes run; see the module's docstring."""
    rule = {"which": which(which_nodes).to_json(), "firmware": str(device)}
    if runtime.sim is None:
        runtime.firmware_rules.append(rule)
        return {}
    return runtime.call(runtime.sim.firmware([rule]))


def on_first_boot(which_nodes, cmds):
    """Lines and intents a station is given the first time it boots with no state."""
    rule = {"which": which(which_nodes).to_json(), "lines": lines_of(cmds)}
    if runtime.sim is None:
        runtime.first_boot_rules.append(rule)
        return None
    return runtime.call(runtime.sim.first_boot([rule]))


def radio(freq_mhz=None, sf=None, bw_khz=None, cr=None, tx_dbm=None, sync=None, preamble=None):
    """The radio intent, for `on_first_boot()`: slot 0's settings, in each
    station's own lines. `tx_dbm="max"` is each node's own maximum power."""
    return Intent("radio", freq_mhz=freq_mhz, sf=sf, bw_khz=bw_khz, cr=cr, tx_dbm=tx_dbm,
                  sync=sync, preamble=preamble)


def radio_up():
    """The radio started, for `on_first_boot()`: said after everything the
    radio reads when it starts (its settings, SUPE, the community radius)."""
    return Intent("radio_up")


def role(name):
    """The role intent, for `on_first_boot()`: transport, router, repeater or client."""
    return Intent("role", role=str(name))


def include(path, missing_ok=False):
    """Run another file's code here, as if it were written at this point:
    `path` is under testbed/ (`scripts/startup.py`, `nodesets/mitte7.py`).
    With `missing_ok`, a file that is not there is nothing."""
    full = os.path.join(TESTBED_DIR, path)
    if not os.path.isfile(full):
        if missing_ok:
            return
        raise ScriptError("include: no file %s" % path)
    with open(full, encoding="utf-8") as handle:
        code = compile(handle.read(), full, "exec")
    builtins.exec(code, {"__name__": "include_%s" % os.path.splitext(os.path.basename(path))[0],
                         "__file__": full})


def nodesets():
    """The names of the nodesets this script's world is made of, in order."""
    if runtime.world.get("nodesets"):
        return list(runtime.world["nodesets"])
    merged = _sim().run.get("nodeset") or ""
    return [name for name in merged.split("+") if name]


# ---- what is done -------------------------------------------------------------

def up(which_nodes="all"):
    """Until every one of these is up: their names."""
    sim = _sim()
    return runtime.call(sim.ready(which(which_nodes)))


def exec(which_nodes, cmds, pause=0.0, after=0.0, wait=True):   # noqa: A001
    """Lines in each node's own language; see the module's docstring."""
    lines = lines_of(cmds)
    sim = _sim()

    async def go():
        names = await sim.ready(which(which_nodes))

        async def one(name):
            replies = []
            for line in lines:
                reply = await sim.ask({"type": "command", "line": line}, [name], after=after)
                replies.append(str(reply.get(name, "")).rstrip("\n"))
            return "\n".join(replies)

        if not pause:
            return dict(zip(names, await asyncio.gather(*(one(n) for n in names))))
        out = {}
        for index, name in enumerate(names):
            if index:
                await sim.sleep(float(pause))
            out[name] = await one(name)
        return out
    return runtime.call(go(), wait)


def _meta(which_nodes, verb, args=None, spread=0.0, after=0.0, wait=True):
    sim = _sim()

    async def go():
        names = await sim.ready(which(which_nodes))
        return await sim.ask({"type": "meta", "verb": verb, "args": dict(args or {})}, names,
                             spread, after)
    return runtime.call(go(), wait)


def announce(which_nodes, spread=0.0, after=0.0):
    """Each one announces, spread over `spread` seconds."""
    return _meta(which_nodes, "announce", spread=spread, after=after)


def max_tx_pwr(which_nodes, dbm=None):
    """Their transmit power in dBm, each node's own maximum when none is given."""
    sim = _sim()
    if dbm is not None:
        return _meta(which_nodes, "tx_power", {"dbm": float(dbm)})
    out = {}
    for name in runtime.call(sim.ready(which(which_nodes))):
        best = sim.stations[name].max_dbm
        out.update(_meta([name], "tx_power", {"dbm": float(best)}))
    return out


def send_msg(from_node, to_node, text, after=0.0, wait=True):
    """An LXMF message from one node to another, in the sender's own lines."""
    sim = _sim()
    frm = which(from_node).pick(sim.facts())
    to = which(to_node).pick(sim.facts())
    if len(frm) != 1 or len(to) != 1:
        raise ScriptError("send_msg is from one node to one node, not %d to %d"
                          % (len(frm), len(to)))
    return _meta(frm, "message", {"to": to[0], "text": str(text)}, after=after, wait=wait)


def reset(which_nodes):
    """Reset pressed on each: the process restarts, its state kept."""
    sim = _sim()
    runtime.call(sim.nodes(names=_names(which_nodes)).reset())


def factory_reset(which_nodes):
    """Each one's state wiped, and set up again as on its first boot."""
    sim = _sim()
    runtime.call(sim.nodes(names=_names(which_nodes)).factory_reset())


def now():
    """Seconds of the run's clock since this script's simulation began."""
    return _sim().run_s


def wait(seconds):
    """`seconds` of the run's clock."""
    sim = _sim()
    runtime.call(sim.sleep(float(seconds)))


def until(seconds):
    """Until the run's clock reads `seconds`."""
    sim = _sim()
    runtime.call(sim.until(float(seconds)))


def phase(*phases):
    """What the run is doing: (name, until) pairs, until in seconds of `now()`."""
    sim = _sim()
    runtime.call(sim.plan(*phases))


def snapshot(name):
    """This moment, flushed, as a snapshot."""
    sim = _sim()
    runtime.call(sim.snapshot(name))


def move(node, lat, lon, height_m=None):
    """A node to another place; its links are recomputed."""
    sim = _sim()
    runtime.call(sim.move(node, lat, lon, height_m))


def pause():
    """Stop the simulation with its state kept, to be resumed as it ended."""
    sim = _sim()
    runtime.call(sim.pause())


def stop():
    """Stop the simulation."""
    sim = _sim()
    runtime.call(sim.stop())


def stations():
    """Every station's facts: {name: {id, tags, firmware, kind, role, status,
    max_dbm, …}}."""
    return _sim().facts()


def station(name):
    """One station's facts."""
    got = _sim().facts().get(name)
    if got is None:
        raise ScriptError("no station called %s" % name)
    return got


def result_of(futures):
    """Every future's result, in order: what `wait=False` calls come to."""
    return [f.result() for f in futures]


__all__ = ["time", "firmware", "on_first_boot", "radio", "radio_up", "role", "include",
           "nodesets",
           "up", "exec", "announce", "max_tx_pwr",
           "send_msg", "reset", "factory_reset", "now", "wait", "until", "phase", "snapshot",
           "move", "pause", "stop", "stations", "station", "nodes", "result_of", "ScriptError"]
