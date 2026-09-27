#!/usr/bin/env python3
"""The front: several simulations, the editors, the scripts and the planner behind one port.

```
browser ── ws   localhost:8800/ws ──────────────────► front ── ws ──► child simd (lora)
browser ── http alpha.lora.sim.localhost:8800 ──────► front ───────► child (lora) ─► alpha :80
browser ── ws   alpha.lora.sim.localhost:8800/webrtc ► front ── ws ─► child ─────────► alpha
browser ── udp  localhost:8800 ─────────────────────► front ── udp ► child ── udp ──► alpha
browser ── http localhost:8800/planner/<geodata>/… ─► front ── http ► planner-web (that pack)
browser ── http localhost:8800/api/table?path=… ────► front          a loss table file (links layer)
browser ── http localhost:8800/api/coverage?… ──────► front          a node's coverage raster
browser ── POST localhost:8800/api/devices/import ──► front          a device zip, expanded
browser ── POST localhost:8800/api/geodata/import ──► front          a pack zip, expanded
driver  ── ws   localhost:8800/ws?sim=lora  {type: "plan", ...} ─────► child

page ── sim_new {geodata, nodeset, script?, time} ──► front
front ── planner-web --pack P --port <free> ───────► sidecar          a pack, first user of P
front ── losses.py --geodata --nodeset --band B ───► subprocess ── GET /link.json × pairs ─► sidecar
front ── losses_progress {sim, band, done, total} ─► every page     as the subprocess reports
front: runs.create_run → runs/<sim>/ (geodata, nodeset, script, losses/<band>.bin, builds)
front ── simd.py --run runs/<sim> --sidecar URL ───► child
front ── sim_new {ok, name, control, run, …} ─► asker

page ── script_run {name, sim} ──► front ── python -m simesh.runner <script> --sim <sim> ──► runner
runner ── ws localhost:8800/ws?sim=<sim> ─► front ─► child     the script's main(sim)
runner ── sim_pause {name} ─► front                            main ended, on a sim started for it
front: stop the child, runs.pause_run → runs/<sim>/paused/    the registry keeps it, paused
runner: report(run_dir) → runs/<sim>/report.md                when the script has a report
front ── script_output {run, line} … script_exit {run, code} ─► every page
page ── sim_resume {name} ──► front: runs.resume_run → runs/<sim>-N/, a child on it
```

One front process owns the port the container publishes. It keeps the
registry of simulations, gives each one a control port, an ether port, a
loopback /22 and a run directory, and starts one **child** per simulation:
an ordinary simd, as a subprocess with the flags a person would give it by
hand. A child keeps its own event loop and its own pace, and a crash in one
simulation ends that one; the registry keeps it, exited, with the last lines
it wrote, until it is stopped.

**Before a child starts** its run is made whole here: the geodata and the
nodeset are loaded and checked, every node's device is resolved (so a missing
package is said at once, not after a table), the loss table of every band the
nodes' carriers use is taken from the cache or computed into it by
`losses.py` as a subprocess, whose progress goes to every page, and the run
directory is laid out by `runs.create_run` (or `runs.load_snapshot`). The
child is then started on that directory and loads it itself.

**The planner sidecar.** One `planner-web` per pack in use, on
`127.0.0.1:<free>`: started when the first simulation or page socket opens
geodata on that pack, stopped when the last one lets go. It is SIMesh's own,
built from `planner/` (`simesh build planner`) to
`planner/target/release/planner-web`; not built, a pack is refused with
NO_PLANNER and synthetic ground works. The page reaches it as `/planner/<geodata>/…`, passed through with
the prefix stripped; a child is given its URL directly, for recomputing a
moved node's row.

**Script runs.** A script's `main` runs as a process of its own
(`simesh.runner`), attached to a running simulation over this same port, its
output kept (the last `SCRIPT_LINES` lines) and sent to every page as it
comes. A run that asks for a new simulation has the front start it first,
with the script as its setup, and the runner pauses it when main ends: its
stations stop with their state kept in its run, and it stays in the
registry as paused until it is resumed (in a new run directory, from that
state) or removed. A script's `report(run_dir)` then writes the run's
`report.md`, which `/api/report` serves. A run on a simulation already running leaves
it running.

The front's control websocket speaks the child's messages (simd.py lists
them) and these of its own, each answered to the asking socket as
`{type: <verb>, ok: true, …}` or `{type: <verb>, ok: false, error}`:

```
sims                                                      the registry, now
sim_new {name?, geodata, nodeset, script?, time?, stagger?, build?, pairwise?}
sim_new {name?, snapshot, time?, stagger?, build?, pairwise?}
      → {ok, name, control, ether, net, run, time, geodata, nodeset, script, snapshot}
sim_stop {name}                                           → {ok, name}
sim_pause {name}                  stopped, its state kept in its run  → {ok, name}
sim_resume {name}                 a paused one, from that state      → as sim_new
run_delete {run}                  an ended or paused run's directory, gone  → {ok, run}
select {sim}                                              which simulation the socket is on

device_list                       → {devices: [row…], arch}      builds/ refreshed, then devices.listing
device_refresh {sources?}         → {results, said}              devices.refresh
geodata_list                      → {geodata: [{name, kind, bbox, …} | {name, error}]}
geodata_open {name}               → {geodata, planner}   holds the sidecar for this socket
geodata_close                                            lets it go
geodata_new {name, pack | synthetic: {terrain, exponent, extent_m}}   → {geodata}
geodata_save {name, data} · geodata_save_as {name, data}              → {geodata}
nodeset_list {geodata?}           → {nodesets: [{name, nodes, bbox, tags, inside?} | {name, error}]}
                                    inside: whether a node stands in that geodata's extent
nodeset_open {name}               → {nodeset}
nodeset_new {name}                → {nodeset}
nodeset_save {name, data} · nodeset_save_as {name, data}              → {nodeset}
nodeset_import {name, format: sites|nodes, text | path, height_m?, device?}  → {nodeset}
script_list                       → {scripts: [{name, doc, setup, main} …]}
script_open {name}                → {script, text}
script_new {name, text?} · script_save {name, text} · script_save_as {name, text}
                                  → {script, text}
script_run {name, sim | geodata, nodeset, setup?, time?, build?}  → {run, simulation}
                                  setup: the new simulation's setup script, else this one's own
script_stop {run}                 → {}
script_log {run}                  → {run, lines}
snapshot_list                     → {snapshots: [{name, t, run, geodata, nodeset, script, …}]}
losses_compute {geodata, nodeset, bands?}  → {tables: {band: {path, cached}}}
coverage {geodata, nodes: [{name, lat, lon, height_m}]}  → {tiles: [{node, key, cached}]}
```

and these unasked:

```
front → socket        hello {front: true, port}                    on connect
front → all sockets   sims {port, sims: [...], script_runs: [...], geodata_names,
                            nodesets, scripts, snapshots}          on change and once a second
                      a sim's state: starting, running, stopping, exited, paused, or
                      ended (a run on disk that is neither; named by its run directory);
                      `report` on a sim or a script run: its run has a report.md;
                      `started`, `ended`: wall seconds (`ended` null while it runs),
                      `t`: its T now, or where it stopped, in microseconds
front → all sockets   losses_progress {sim | nodeset, band, done, total}
front → all sockets   script_output {run, line} · script_exit {run, code}
front → all sockets   devices_changed {}                           the start's refresh fetched some
front → asker         coverage_tile {geodata, node, key} · coverage_error {geodata, node, error}
anything else         → the child named by `sim`, or the selected one
child → socket        the child's own message, with `sim` added
```

`pairwise: true` in `sim_new` starts that simulation's ether on the pairwise
rule (simd's `--pairwise`); the front's own `-- --pairwise` does it for every
simulation.

`sim_load {geodata, nodeset, script?}` and `snapshot_load {name}` sent to a
running simulation pass through the front on their way: it computes the
tables and holds the sidecar first, with progress as for `sim_new`, and adds
`sidecar` before handing the message on.

`?sim=<name>` on the websocket selects that simulation from the start, and
`?quiet=1` asks every child for its quiet stream (no tx, rx, radio or levels).

Station hostnames carry the simulation: `<station>.<sim>.sim.localhost`. The
front routes on the second label and the child on the first. A bare
`<station>.sim.localhost` reaches the one simulation while exactly one runs. The
WebRTC relay is the child's own mechanism, one level up: the front terminates
the signalling, points the answer at itself, and forwards the UDP flow to the
child's relay.

Nothing here blocks: the children's, the sidecars', the scripts' and the loss
computation's output is read as it comes, files are laid out and zips
expanded on a worker thread, and every wait is an awaitable.
"""

import argparse
import asyncio
import collections
import contextlib
import ctypes
import fcntl
import ipaddress
import itertools
import json
import os
import re
import signal
import socket
import sys
import tempfile
import time

import aiohttp
from aiohttp import WSMsgType, web

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SIM_DIR)

import coverage as coverage_module  # noqa: E402 - the path is set just above
import devices as devices_module    # noqa: E402
import geodata as geodata_module    # noqa: E402
import kinds as kinds_module        # noqa: E402
import losses as losses_module      # noqa: E402
import nodeset as nodeset_module    # noqa: E402
import proxy                        # noqa: E402
import runs as runs_module          # noqa: E402
import script as script_module      # noqa: E402
import simd as simd_module          # noqa: E402
import store                        # noqa: E402
import webrtc as webrtc_module      # noqa: E402

SIGNAL_PATH = simd_module.SIGNAL_PATH
LOSSES_PY = os.path.join(SIM_DIR, "losses.py")
PLANNER_WEB = os.path.join("target", "release", "planner-web")
NO_PLANNER = ("geodata %s is a pack, and planner-web is not built: simesh build planner")

CONTROL_PORTS = range(9100, 9200)   # a child's page, proxy and relay (TCP and UDP)
ETHER_PORTS = range(7100, 7200)     # a child's ether
NETS = range(4, 64)                 # 127.<4k>.0.0/22; below 127.16 is left for simd by hand
NET_LOCK_DIR = os.path.join(tempfile.gettempdir(), "simesh-nets")   # one lock per /22, host-wide
READY_TIMEOUT_S = 30.0              # how long a child has to open its port
STOP_TIMEOUT_S = 15.0               # how long a child has to stop before it is killed
SIDECAR_READY_S = 60.0              # how long a planner-web has to answer /api/pack
SIDECAR_POLL_S = 0.2
REPORT_S = 1.0                      # how often every socket hears the registry
PACE_WINDOW_S = 120.0               # the wall the estimate's pace is taken over
TAIL_LINES = 40                     # a child's last lines, kept for the page
SCRIPT_LINES = 2000                 # a script run's last lines, kept for the page
ERRORS_KEPT = 5
MAX_UPLOAD = 4 << 30                # a pack zip is hundreds of megabytes
HOP_HEADERS = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
               "te", "trailers", "transfer-encoding", "upgrade", "host", "content-length"}

SIM_VERBS = ("sim_new", "sim_stop", "sim_pause", "sim_resume", "run_delete")
EDITOR_VERBS = (
    "device_list", "device_refresh",
    "geodata_list", "geodata_open", "geodata_close", "geodata_new", "geodata_save",
    "geodata_save_as",
    "nodeset_list", "nodeset_open", "nodeset_new", "nodeset_save", "nodeset_save_as",
    "nodeset_import",
    "script_list", "script_open", "script_new", "script_save", "script_save_as",
    "script_run", "script_stop", "script_log",
    "snapshot_list", "losses_compute", "coverage")
QUIET_VERBS = ("device_list", "geodata_list", "nodeset_list", "script_list", "snapshot_list",
               "geodata_open", "nodeset_open", "script_open", "geodata_close", "script_log",
               "coverage")

PR_SET_PDEATHSIG = 1


def log(msg):
    sys.stderr.write("front: %s\n" % msg)
    sys.stderr.flush()


def die_with_parent():
    """In a child, before exec: go when the front goes, however it goes.

    A front killed outright would otherwise leave its simulations and
    sidecars running with nothing in front of them and their ports taken.
    """
    with contextlib.suppress(OSError, AttributeError):
        ctypes.CDLL("libc.so.6", use_errno=True).prctl(PR_SET_PDEATHSIG, signal.SIGTERM)


def port_free(port):
    """Whether both TCP and UDP `port` can be bound on loopback just now."""
    for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
        with socket.socket(socket.AF_INET, kind) as probe:
            if kind == socket.SOCK_STREAM:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(("127.0.0.1", port))
            except OSError:
                return False
    return True


def bound_addresses():
    """Every IPv4 address a TCP or UDP socket on this host is bound to just
    now, from /proc/net: what another front's stations, or a simd started by
    hand, already stand on. Empty where /proc/net is not there to read."""
    found = set()
    for table in ("/proc/net/tcp", "/proc/net/udp"):
        try:
            with open(table, encoding="ascii") as handle:
                next(handle, None)
                for line in handle:
                    local = line.split()[1].partition(":")[0]
                    found.add(socket.inet_ntoa(int(local, 16).to_bytes(4, "little")))
        except (OSError, ValueError, IndexError):
            continue
    return found


def net_free(net, bound):
    """Whether no address of this network is among `bound`."""
    network = ipaddress.ip_network(net)
    return not any(ipaddress.ip_address(a) in network for a in bound)


def claim_net(net):
    """This front's claim on a /22, host-wide: an open descriptor holding an
    exclusive `flock` on `NET_LOCK_DIR/<net>.lock`, or None when another
    front (or another simulation of this one) holds it or the lock cannot be
    taken.

    A block is given out well before its stations bind it: the loss table
    and the child's start come between. Two fronts asking in that window
    would both find it unbound, so the claim is a lock rather than a look.
    It never waits, and it goes with the descriptor: closed when the
    simulation ends, and by the kernel when the front dies however it dies,
    so a stale file claims nothing.
    """
    try:
        os.makedirs(NET_LOCK_DIR, exist_ok=True)
        with contextlib.suppress(OSError):
            os.chmod(NET_LOCK_DIR, 0o1777)
        fd = os.open(os.path.join(NET_LOCK_DIR, net.replace("/", "_") + ".lock"),
                     os.O_RDWR | os.O_CREAT | os.O_CLOEXEC, 0o666)
    except OSError as err:
        log("network %s: no lock in %s (%s)" % (net, NET_LOCK_DIR, err))
        return None
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)
        return None
    return fd


def any_free_port():
    """A TCP port on loopback nothing holds just now, for a sidecar."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def planner_web():
    """The planner-web executable, or None when there is no planner."""
    path = os.path.join(geodata_module.planner_repo(), PLANNER_WEB)
    return path if os.path.isfile(path) and os.access(path, os.X_OK) else None


def load_geodata(name):
    """Geodata by name, a pack refused with NO_PLANNER when there is no
    planner, before its manifest is read: the missing planner is the reason,
    and a pack left unreadable with it is not. With a planner, an unreadable
    manifest is reported as such."""
    return geodata_module.load(name, refuse_packs=None if planner_web() else NO_PLANNER)


def read_geodata(path, name=None):
    """A geodata file (a run's or a snapshot's copy), as `load_geodata`."""
    return geodata_module.read(path, name, refuse_packs=None if planner_web() else NO_PLANNER)


# ---------------------------------------------------------------------------
# The planner sidecars
# ---------------------------------------------------------------------------

class Sidecar:
    """One planner-web on one pack, and who is holding it."""

    def __init__(self, pack, port):
        self.pack = pack
        self.port = port
        self.url = "http://127.0.0.1:%d" % port
        self.process = None
        self.holders = set()
        self.ready = asyncio.get_running_loop().create_future()
        self.log_path = os.path.join(store.RUNS_DIR, "planner-%s.log"
                                     % store.slug(os.path.basename(pack), "pack"))

    @property
    def up(self):
        return self.ready.done() and not self.ready.cancelled() \
            and self.ready.exception() is None


class Sidecars:
    """Every sidecar, by pack directory. A holder is a simulation or a
    control socket, and holds one pack at a time."""

    def __init__(self, session):
        self.session = session
        self.by_pack = {}

    def running(self, pack):
        car = self.by_pack.get(pack)
        return car if car is not None and car.up else None

    def url_for(self, gd):
        car = self.running(gd.pack_dir) if gd.is_pack else None
        return car.url if car else None

    async def hold(self, holder, gd):
        """The sidecar URL for this geodata, started if nobody runs one on its
        pack; None for synthetic ground. Whatever else the holder held it
        lets go."""
        if not gd.is_pack:
            self.release(holder)
            return None
        self.release(holder, keep=gd.pack_dir)
        car = self.by_pack.get(gd.pack_dir)
        if car is None:
            binary = planner_web()
            if binary is None:
                raise store.StoreError(NO_PLANNER % gd.name)
            car = Sidecar(gd.pack_dir, any_free_port())
            self.by_pack[gd.pack_dir] = car
            asyncio.ensure_future(self.start(car, binary))
        car.holders.add(holder)
        try:
            await asyncio.shield(car.ready)
        except store.StoreError:
            car.holders.discard(holder)
            raise
        return car.url

    async def start(self, car, binary):
        os.makedirs(os.path.dirname(car.log_path), exist_ok=True)
        try:
            with open(car.log_path, "ab") as out:
                car.process = await asyncio.create_subprocess_exec(
                    binary, "--pack", car.pack, "--host", "127.0.0.1", "--port", str(car.port),
                    stdin=asyncio.subprocess.DEVNULL, stdout=out, stderr=out,
                    start_new_session=True, preexec_fn=die_with_parent)
        except OSError as err:
            self.failed(car, "planner-web: %s" % err)
            return
        log("planner-web pid %d on %s for %s (log %s)"
            % (car.process.pid, car.url, car.pack, os.path.relpath(car.log_path)))
        loop = asyncio.get_running_loop()
        deadline = loop.time() + SIDECAR_READY_S
        while loop.time() < deadline:
            if car.process.returncode is not None:
                self.failed(car, "planner-web on %s exited with %s: see %s"
                            % (car.pack, car.process.returncode, car.log_path))
                return
            try:
                async with self.session.get(car.url + "/api/pack",
                                            timeout=aiohttp.ClientTimeout(total=5)) as resp:
                    if resp.status == 200:
                        if not car.ready.done():
                            car.ready.set_result(car.url)
                        return
            except (aiohttp.ClientError, asyncio.TimeoutError, OSError):
                pass
            await asyncio.sleep(SIDECAR_POLL_S)
        self.failed(car, "planner-web on %s did not answer in %.0f s: see %s"
                    % (car.pack, SIDECAR_READY_S, car.log_path))
        await self.stop(car)

    def failed(self, car, why):
        log(why)
        if self.by_pack.get(car.pack) is car:
            del self.by_pack[car.pack]
        if not car.ready.done():
            car.ready.set_exception(store.StoreError(why))
            car.ready.exception()           # retrieved: a failure nobody awaited is not news

    def release(self, holder, keep=None):
        """`holder` lets go of every pack but `keep`; a sidecar nobody holds stops."""
        for pack, car in list(self.by_pack.items()):
            if pack == keep or holder not in car.holders:
                continue
            car.holders.discard(holder)
            if not car.holders:
                del self.by_pack[pack]
                asyncio.ensure_future(self.stop(car))

    async def stop(self, car):
        if not car.ready.done():
            car.ready.set_exception(store.StoreError("the sidecar on %s was stopped" % car.pack))
            car.ready.exception()
        process = car.process
        if process is None or process.returncode is not None:
            return
        with contextlib.suppress(ProcessLookupError):
            process.terminate()
        try:
            await asyncio.wait_for(process.wait(), STOP_TIMEOUT_S)
        except asyncio.TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                process.kill()
            await process.wait()
        log("planner-web on %s stopped" % car.pack)

    async def close(self):
        cars = list(self.by_pack.values())
        self.by_pack.clear()
        await asyncio.gather(*(self.stop(car) for car in cars), return_exceptions=True)


# ---------------------------------------------------------------------------
# One simulation
# ---------------------------------------------------------------------------

class Child:
    """One simulation: its simd process, and what the front knows about it."""

    def __init__(self, front, name, port, ether_port, net, time_mode, stagger,
                 run_dir=None, sidecar=None, pairwise=False):
        self.front = front
        self.name = name
        self.port = port
        self.ether = "127.0.0.1:%d" % ether_port
        self.net = net
        self.time_mode = time_mode
        self.stagger = stagger
        self.pairwise = bool(pairwise)      # the ether's pairwise rule, for this one
        self.run_dir = run_dir or os.path.join(store.RUNS_DIR, name)
        self.sidecar = sidecar
        self.process = None
        self.state = "starting"             # starting, running, stopping, exited
        self.code = None
        self.started = time.time()
        self.ended = None                   # wall seconds, once it has exited
        self.ready = asyncio.get_running_loop().create_future()
        self.tail = collections.deque(maxlen=TAIL_LINES)
        self.errors = collections.deque(maxlen=ERRORS_KEPT)
        self.monitor = None                 # the front's own quiet socket to the child
        self.monitor_task = None
        self.reader = None
        # What the monitor has heard.
        self.loaded = {}                    # the run: geodata, nodeset, script, snapshot
        self.dirty = False
        self.clock = None
        self.nodes = {}                     # station name -> status
        self.samples = collections.deque()  # (wall, T) over the pace window

    @property
    def holder(self):
        return "sim:%s" % self.name

    # ---- the process -----------------------------------------------------

    async def start(self):
        os.makedirs(self.run_dir, exist_ok=True)
        argv = [sys.executable, "-u", os.path.join(SIM_DIR, "simd.py"),
                "--bind", "127.0.0.1:%d" % self.port,
                "--ether", self.ether,
                "--net", self.net,
                "--run", self.run_dir,
                "--time", self.time_mode]
        if self.stagger is not None:
            argv += ["--stagger", str(self.stagger)]
        if self.sidecar:
            argv += ["--sidecar", self.sidecar]
        if self.pairwise:
            argv += ["--pairwise"]
        argv += [a for a in self.front.child_args
                 if not (self.pairwise and a == "--pairwise")]
        # A process group of its own, which its stations inherit: a child that
        # dies without stopping them leaves them in it, and exited() sweeps it.
        self.process = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
            start_new_session=True, preexec_fn=die_with_parent)
        log("%s: simd pid %d on 127.0.0.1:%d, ether %s, net %s, run %s"
            % (self.name, self.process.pid, self.port, self.ether, self.net,
               os.path.relpath(self.run_dir)))
        self.reader = asyncio.ensure_future(self.read_output())

    async def read_output(self):
        """The child's log, line by line: to its file, to the tail, and the
        line that says its port is open is what makes it ready."""
        path = os.path.join(self.run_dir, "simd.log")
        with open(path, "a", buffering=1) as out:
            out.write("---- %s\n" % time.strftime("%Y-%m-%d %H:%M:%S"))
            while True:
                raw = await self.process.stdout.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").rstrip("\n")
                out.write(line + "\n")
                self.tail.append(line)
                if not self.ready.done() and "control page on" in line:
                    self.ready.set_result(True)
        code = await self.process.wait()
        self.exited(code)

    def exited(self, code):
        was = self.state
        self.state = "exited"
        self.code = code
        self.ended = time.time()
        # Stations a crashed simd left behind would hold its addresses and its
        # ether port against the next simulation given them.
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(self.process.pid, signal.SIGKILL)
        if not self.ready.done():
            self.ready.set_result(False)
        if self.monitor_task is not None:
            self.monitor_task.cancel()
        if was != "stopping":
            log("%s: simd exited with %s" % (self.name, code))
        self.front.child_gone(self)

    async def stop(self):
        """SIGTERM, which simd takes as Ctrl-C; SIGKILL if that is not enough."""
        if self.process is None or self.process.returncode is not None:
            return
        self.state = "stopping"
        self.process.send_signal(signal.SIGTERM)
        try:
            await asyncio.wait_for(asyncio.shield(self.reader), STOP_TIMEOUT_S)
        except asyncio.TimeoutError:
            log("%s: simd did not stop in %.0f s, killing it" % (self.name, STOP_TIMEOUT_S))
            with contextlib.suppress(ProcessLookupError):
                self.process.kill()
            await self.reader

    # ---- the monitor -----------------------------------------------------

    def control_url(self, quiet=False):
        return "http://127.0.0.1:%d/ws%s" % (self.port, "?quiet=1" if quiet else "")

    async def watch(self):
        """The front's own quiet socket to the child: what the registry shows."""
        try:
            async with self.front.session.ws_connect(self.control_url(quiet=True),
                                                     max_msg_size=0) as ws:
                self.monitor = ws
                async for message in ws:
                    if message.type is WSMsgType.TEXT:
                        with contextlib.suppress(ValueError):
                            self.heard(json.loads(message.data))
        except (aiohttp.ClientError, OSError) as err:
            if self.state == "running":
                log("%s: lost the control socket (%s)" % (self.name, err))
        finally:
            self.monitor = None

    def heard(self, msg):
        kind = msg.get("type")
        if kind == "snapshot":
            run = msg.get("run") or {}
            self.loaded = {key: run.get(key) for key in ("geodata", "nodeset", "script",
                                                         "snapshot")}
            if run.get("dir"):
                self.run_dir = run["dir"]
            self.dirty = bool((msg.get("nodeset") or {}).get("dirty"))
            self.nodes = {n["name"]: n["status"] for n in msg.get("nodes") or ()}
            if msg.get("clock"):
                self.clocked(msg["clock"])
        elif kind == "node":
            self.nodes[msg["name"]] = msg.get("status")
        elif kind == "node_gone":
            self.nodes.pop(msg.get("name"), None)
        elif kind == "nodeset":
            self.dirty = bool(msg.get("dirty"))
        elif kind == "clock":
            self.clocked(msg)
        elif kind == "error":
            self.errors.append([time.time(), msg.get("text")])

    def clocked(self, clock):
        """Keep T against the wall for the last two minutes: the estimate's pace."""
        self.clock = clock
        now = time.monotonic()
        if self.samples and clock["t"] < self.samples[-1][1]:
            self.samples.clear()            # a new ether: T started again
        self.samples.append((now, clock["t"]))
        while len(self.samples) > 2 and now - self.samples[1][0] >= PACE_WINDOW_S:
            self.samples.popleft()

    def pace(self):
        """Seconds of T per second of wall over the window, or None."""
        if self.clock and self.clock.get("mode") != "virtual":
            return 1.0
        if len(self.samples) < 2:
            return None
        (w0, t0), (w1, t1) = self.samples[0], self.samples[-1]
        if w1 - w0 < 5.0 or t1 <= t0:
            return None
        return (t1 - t0) / 1e6 / (w1 - w0)

    # ---- the registry's row ---------------------------------------------

    def summary(self):
        counts = collections.Counter(self.nodes.values())
        row = {"name": self.name, "state": self.state, "code": self.code,
               "port": self.port, "ether": self.ether, "net": self.net,
               "run": os.path.relpath(self.run_dir, SIM_DIR), "time": self.time_mode,
               "started": self.started, "ended": self.ended, "geodata": self.loaded.get("geodata"),
               "nodeset": self.loaded.get("nodeset"), "script": self.loaded.get("script"),
               "snapshot": self.loaded.get("snapshot"), "dirty": self.dirty,
               "stations": len(self.nodes), "counts": dict(counts),
               "errors": list(self.errors), "pace": self.pace(),
               "report": os.path.isfile(os.path.join(self.run_dir, runs_module.REPORT_FILE)),
               "tail": list(self.tail)[-12:] if self.state == "exited" else []}
        clock = self.clock or {}
        row.update({"mode": clock.get("mode"), "rate": clock.get("rate"),
                    "observed": clock.get("observed"), "t": clock.get("t")})
        row.update(self.estimate())
        return row

    def estimate(self):
        """Where the run is in its plan, and when each part should end.

        The phase is the first whose end T has not been reached; it began where
        the one before it ended, or where the plan was given. The finish is the
        T left over the pace of the last two minutes of wall, from now.
        """
        clock = self.clock or {}
        plan, t = clock.get("plan"), clock.get("t")
        if not plan or t is None or not plan.get("phases"):
            return {"plan": None, "phase": None, "eta": None}
        phases = plan["phases"]
        begin, current = plan.get("t", t), None
        for index, phase in enumerate(phases):
            if t < phase["until"]:
                current = {"index": index, "name": phase["name"],
                           "from": begin, "until": phase["until"]}
                break
            begin = phase["until"]
        pace, now = self.pace(), time.time()
        end = phases[-1]["until"]
        eta = now + (end - t) / 1e6 / pace if pace and t < end else None
        if current is not None:
            current["eta"] = (now + (current["until"] - t) / 1e6 / pace) if pace else None
        return {"plan": phases, "plan_from": plan.get("t", t), "phase": current,
                "eta": eta, "done": t >= end}


# ---------------------------------------------------------------------------
# One script run
# ---------------------------------------------------------------------------

class ScriptRun:
    """A script's `main`, running as `simesh.runner` on one simulation (and
    then its `report`, when it has one, on that simulation's run)."""

    def __init__(self, front, ident, name, sim, run_dir, owns_sim=False):
        self.front = front
        self.id = ident
        self.name = name
        self.sim = sim
        self.run_dir = run_dir
        # A simulation started for this run is this run's: it is paused when
        # main ends, its state kept to be resumed. One the run attached to is
        # left running.
        self.owns_sim = owns_sim
        self.process = None
        self.state = "running"              # running, stopping, exited
        self.code = None
        self.started = time.time()
        self.lines = collections.deque(maxlen=SCRIPT_LINES)
        self.reader = None

    async def start(self, port):
        argv = [sys.executable, "-u", "-m", "simesh.runner", script_module.script_path(self.name),
                "--sim", self.sim, "--port", str(port)]
        if self.owns_sim:
            argv.append("--pause")
        env = dict(os.environ, SIMESH_PORT=str(port), SIMESH_SIM=self.sim,
                   PYTHONPATH=os.pathsep.join(p for p in (SIM_DIR, os.environ.get("PYTHONPATH"))
                                              if p))
        self.process = await asyncio.create_subprocess_exec(
            *argv, cwd=SIM_DIR, env=env, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
            start_new_session=True, preexec_fn=die_with_parent)
        log("script %s (%s) on %s: pid %d" % (self.name, self.id, self.sim, self.process.pid))
        self.reader = asyncio.ensure_future(self.read_output())

    def said(self, line):
        self.lines.append(line)
        self.front.broadcast({"type": "script_output", "run": self.id, "line": line})

    async def read_output(self):
        while True:
            raw = await self.process.stdout.readline()
            if not raw:
                break
            self.said(raw.decode("utf-8", "replace").rstrip("\n"))
        self.code = await self.process.wait()
        log("script %s (%s) exited with %s" % (self.name, self.id, self.code))
        # The runner pauses what it was told to; one that died first leaves
        # it running, and it is paused here instead.
        if self.owns_sim and self.sim in self.front.children:
            with contextlib.suppress(ValueError, store.StoreError, OSError):
                await self.front.sim_pause({"name": self.sim})
        self.state = "exited"
        self.front.broadcast({"type": "script_exit", "run": self.id, "code": self.code})
        self.front.changed = True

    async def stop(self):
        if self.process is None or self.process.returncode is not None:
            return
        self.state = "stopping"
        with contextlib.suppress(ProcessLookupError):
            self.process.send_signal(signal.SIGINT)
        try:
            await asyncio.wait_for(asyncio.shield(self.reader), STOP_TIMEOUT_S)
        except asyncio.TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.process.pid, signal.SIGKILL)
            await self.reader

    def summary(self):
        return {"run": self.id, "name": self.name, "sim": self.sim, "state": self.state,
                "code": self.code, "started": self.started,
                "run_dir": os.path.relpath(self.run_dir, SIM_DIR),
                "report": os.path.isfile(os.path.join(self.run_dir, runs_module.REPORT_FILE))}


# ---------------------------------------------------------------------------
# One socket from a page or a driver
# ---------------------------------------------------------------------------

class Conn:
    """A control websocket on the front, and its sockets to children."""

    def __init__(self, ws, selected, quiet):
        self.ws = ws
        self.selected = selected
        self.quiet = quiet
        self.upstreams = {}                 # sim name -> (child websocket, pump task)
        self.opening = {}                   # sim name -> future, while one is opened

    @property
    def holder(self):
        return "conn:%d" % id(self)

    async def send(self, message):
        text = message if isinstance(message, str) else json.dumps(message)
        with contextlib.suppress(ConnectionError, RuntimeError):
            await self.ws.send_str(text)

    def close_upstream(self, name):
        entry = self.upstreams.pop(name, None)
        if entry is not None:
            entry[1].cancel()

    def close(self):
        for name in list(self.upstreams):
            self.close_upstream(name)


# ---------------------------------------------------------------------------
# The front
# ---------------------------------------------------------------------------

class Front:

    def __init__(self, args):
        self.args = args
        self.child_args = args.child_args
        self.children = {}                  # name -> Child
        self.paused = {}                    # name -> runs.Run waiting in its pause
        self.ended_cache = {}               # run name -> ((run.yaml mtime, has report), row)
        self.script_runs = {}               # run id -> ScriptRun
        self.run_ids = itertools.count(1)
        self.net_locks = {}                 # net -> descriptor holding its claim_net lock
        self.preparing = set()              # names whose run is being laid out
        self.conns = set()
        self.session = None
        self.proxy_session = None
        self.sidecars = None
        self.sweeps = None
        self.runner = None
        self.control_port = None
        self.listener = None
        self.relay = None
        self.reporter = None
        self.fetcher = None
        self.changed = False

    @property
    def bind_port(self):
        return int(self.args.bind.rpartition(":")[2])

    # ---- allocation ------------------------------------------------------

    def allocate(self):
        """A control port, an ether port and a /22 no live child has.

        Each is also checked free on the host, so a simd started by hand or
        another front's simulations are stepped around rather than collided
        with: two stations on one address fight over its ports, and the
        loser's web server and TCP interface never open. A network must have
        no socket bound to any of its addresses, and this front must win its
        host-wide lock (`claim_net`), which it holds until the simulation
        ends; that covers another front that has given the block out but
        whose stations have not bound it yet.
        """
        live = [c for c in self.children.values() if c.state != "exited"]
        ports = {c.port for c in live}
        ethers = {c.ether for c in live}
        nets = {c.net for c in live}
        for held in [n for n in self.net_locks if n not in nets]:
            self.release_net(held)
        port = next((p for p in CONTROL_PORTS if p not in ports and port_free(p)), None)
        ether = next((p for p in ETHER_PORTS
                      if "127.0.0.1:%d" % p not in ethers and port_free(p)), None)
        net = None
        if port is not None and ether is not None:
            bound = bound_addresses()
            for k in NETS:
                candidate = "127.%d.0.0/22" % (4 * k)
                if candidate in nets or not net_free(candidate, bound):
                    continue
                lock = claim_net(candidate)
                if lock is not None:
                    self.net_locks[candidate] = lock
                    net = candidate
                    break
        if net is None:
            raise ValueError("no free port or network left for another simulation")
        return port, ether, net

    def release_net(self, net):
        """Let go of this front's host-wide claim on a network."""
        lock = self.net_locks.pop(net, None)
        if lock is not None:
            os.close(lock)

    def free_name(self, base):
        base = re.sub(r"[^a-z0-9-]", "-", (base or "sim").lower()).strip("-")[:28] or "sim"
        taken = set(self.children) | self.preparing | set(self.paused)
        if base not in taken:
            return base
        n = 2
        while "%s-%d" % (base, n) in taken:
            n += 1
        return "%s-%d" % (base, n)

    # ---- loss tables -----------------------------------------------------

    def broadcast(self, message):
        text = json.dumps(message)
        for conn in list(self.conns):
            asyncio.ensure_future(conn.send(text))

    async def compute_losses(self, geodata_name, nodeset_name, bands, sidecar, tag):
        """Each band's table from the cache, or computed into it by
        losses.py as a subprocess: {band: (cache path, whether it was cached)}.

        Its JSON progress lines go to every page as `losses_progress` with
        `tag` ({sim} or {nodeset}) in them; its other output is kept for the
        error, should there be one.
        """
        out = {}
        for band in bands:
            argv = [sys.executable, "-u", LOSSES_PY, "--geodata", geodata_name,
                    "--nodeset", nodeset_name, "--band", band]
            if sidecar:
                argv += ["--sidecar", sidecar]
            proc = await asyncio.create_subprocess_exec(
                *argv, stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT, cwd=SIM_DIR)
            done, error, other = None, None, collections.deque(maxlen=5)
            while True:
                raw = await proc.stdout.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").strip()
                try:
                    event = json.loads(line)
                except ValueError:
                    event = None
                if not isinstance(event, dict):
                    if line:
                        other.append(line)
                    continue
                if event.get("event") == "progress":
                    self.broadcast({"type": "losses_progress", **tag, "band": band,
                                    "done": event.get("done"), "total": event.get("total")})
                elif event.get("event") == "notice":
                    log("losses %s/%s band %s: %s" % (geodata_name, nodeset_name, band,
                                                      event.get("message")))
                elif event.get("event") == "done":
                    done = event
                elif event.get("event") == "error":
                    error = event.get("message")
            await proc.wait()
            if done is None:
                raise store.StoreError("losses for %s on %s, band %s: %s" % (
                    nodeset_name, geodata_name, band,
                    error or " | ".join(other) or "losses.py exited %s" % proc.returncode))
            out[band] = (done["path"], bool(done.get("cached")))
            log("losses %s/%s band %s: %s%s" % (geodata_name, nodeset_name, band,
                                                os.path.relpath(done["path"], SIM_DIR),
                                                " (cached)" if done.get("cached") else ""))
        return out

    # ---- the simulation verbs --------------------------------------------

    async def prepare_run(self, name, msg):
        """Lay out a simulation's run directory: (run, sidecar URL or None).

        Everything that can be refused is checked before the loss tables,
        which may take minutes: the names, the script, the devices. The
        sidecar is held for the simulation from here on.
        """
        snapshot = msg.get("snapshot")
        time_mode = str(msg.get("time") or "real")
        base = os.path.join(store.RUNS_DIR, name)
        holder = "sim:%s" % name
        if snapshot or msg.get("resume"):
            if snapshot:
                run = await asyncio.to_thread(runs_module.load_snapshot, snapshot,
                                              simd_module.free_run_dir(base), time_mode)
            else:
                run = await asyncio.to_thread(runs_module.resume_run, msg["resume"],
                                              simd_module.free_run_dir(base), time_mode)
            gd = read_geodata(run.geodata_path, run.geodata_name)
            builds = kinds_module.resolve_builds(run.nodeset().nodes, msg.get("build"))
            run.set(snapshot_builds=run.meta.get("builds") or {}, builds=builds)
            sidecar = None
            if gd.is_pack:
                try:
                    sidecar = await self.sidecars.hold(holder, gd)
                except store.StoreError as err:
                    log("%s: %s; the snapshot runs, and a moved node's row waits"
                        % (name, err))
            return run, sidecar
        gd = load_geodata(msg["geodata"])
        ns = nodeset_module.load(msg["nodeset"])
        script_name = msg.get("script") or None
        if script_name:
            script_module.read(script_name)
        builds = kinds_module.resolve_builds(ns.nodes, msg.get("build"))
        sidecar = await self.sidecars.hold(holder, gd)
        tables = await self.compute_losses(gd.name, ns.name, losses_module.bands_of(ns, gd),
                                           sidecar, {"sim": name})
        run = await asyncio.to_thread(
            runs_module.create_run, simd_module.free_run_dir(base), gd, ns, script_name,
            time_mode, {band: path for band, (path, _) in tables.items()}, builds)
        return run, sidecar

    async def sim_new(self, msg):
        """Make the run, start a child on it, and say where it is."""
        snapshot = msg.get("snapshot")
        if snapshot and any(msg.get(k) for k in ("geodata", "nodeset", "script")):
            raise ValueError("a simulation is geodata, a nodeset and a script, or a "
                             "snapshot, not both")
        resume = msg.get("resume")
        if resume is not None and not isinstance(resume, runs_module.Run):
            raise ValueError("a simulation is resumed with sim_resume")
        if not snapshot and not resume and not all(msg.get(k) for k in ("geodata", "nodeset")):
            raise ValueError("a simulation needs geodata and a nodeset, or a snapshot")
        name = store.check_name(
            msg.get("name") or self.free_name(snapshot or msg.get("nodeset")), "simulation")
        old = self.children.get(name)
        if name in self.preparing or (old is not None and old.state != "exited"):
            raise ValueError("a simulation named %s is already running" % name)
        if name in self.paused:
            raise ValueError("a simulation named %s is paused: resume it, or remove it" % name)
        time_mode = str(msg.get("time") or "real")
        simd_module.ether_module.parse_time_mode(time_mode)
        stagger = msg.get("stagger")
        stagger = float(stagger) if stagger not in (None, "") else None

        self.preparing.add(name)
        try:
            run, sidecar = await self.prepare_run(name, msg)
        except BaseException:
            self.sidecars.release("sim:%s" % name)
            raise
        finally:
            self.preparing.discard(name)
        try:
            port, ether_port, net = self.allocate()
        except ValueError:
            self.sidecars.release("sim:%s" % name)
            raise
        child = Child(self, name, port, ether_port, net, time_mode, stagger, run.dir, sidecar,
                      pairwise=bool(msg.get("pairwise")))
        self.children[name] = child
        self.changed = True
        await child.start()
        try:
            ok = await asyncio.wait_for(asyncio.shield(child.ready), READY_TIMEOUT_S)
        except asyncio.TimeoutError:
            ok = False
        if not ok:
            await child.stop()
            raise ValueError("simulation %s did not start: %s" % (
                name, " | ".join(list(child.tail)[-3:]) or "no output"))
        child.state = "running"
        child.monitor_task = asyncio.ensure_future(child.watch())
        self.changed = True
        for conn in list(self.conns):
            if conn.selected == name and name not in conn.upstreams:
                asyncio.ensure_future(self.open_upstream(conn, name))
        what = snapshot or "%s / %s / %s" % (run.meta.get("geodata"), run.meta.get("nodeset"),
                                            run.meta.get("script") or "no script")
        log("%s: running %s" % (name, what))
        return {"type": "sim_new", "ok": True, "name": name,
                "control": "ws://127.0.0.1:%d/ws" % port, "ether": child.ether,
                "net": net, "run": run.dir, "time": time_mode,
                "geodata": run.meta.get("geodata"), "nodeset": run.meta.get("nodeset"),
                "script": run.meta.get("script"), "snapshot": run.meta.get("snapshot"),
                "builds": run.meta.get("builds")}

    async def sim_pause(self, msg):
        """Stop a simulation with its state kept in its run, to be resumed as
        it ended; it stays in the registry as paused."""
        name = msg.get("name")
        child = self.children.get(name)
        if child is None or child.state != "running":
            raise ValueError("no running simulation named %s" % name)
        t = (child.clock or {}).get("t") or 0
        run_dir = child.run_dir
        await self.sim_stop({"name": name})
        run = runs_module.open_run(run_dir)
        await asyncio.to_thread(runs_module.pause_run, run, name, t)
        self.paused[name] = run
        self.changed = True
        log("%s: paused at T %.0f s, in %s" % (name, t / 1e6, os.path.relpath(run_dir)))
        return {"type": "sim_pause", "ok": True, "name": name}

    async def sim_resume(self, msg):
        """A paused simulation, started again from where it ended, in a new
        run directory under its own name."""
        name = msg.get("name")
        old = self.paused.pop(name, None)
        if old is None:
            raise ValueError("no paused simulation named %s" % name)
        try:
            reply = await self.sim_new({"name": name, "resume": old,
                                        "time": old.meta.get("time") or "real"})
        except BaseException:
            if old.meta.pop("resumed", None) is not None:
                old.set()
            self.paused[name] = old
            self.changed = True
            raise
        return dict(reply, type="sim_resume")

    async def run_delete(self, msg):
        """A run that is not running deleted, directory and all: an ended one,
        or a paused one, which can then no longer be resumed."""
        name = msg.get("run")
        path = runs_module.run_path(name)
        if not os.path.isfile(os.path.join(path, runs_module.RUN_FILE)):
            raise ValueError("no run called %s" % name)
        real = os.path.realpath(path)
        if any(os.path.realpath(c.run_dir) == real and c.state != "exited"
               for c in self.children.values()):
            raise ValueError("run %s is running: stop it first" % name)
        for sim, run in list(self.paused.items()):
            if os.path.realpath(run.dir) == real:
                del self.paused[sim]
        await asyncio.to_thread(runs_module.delete_run, runs_module.open_run(path))
        self.ended_cache.pop(name, None)
        self.changed = True
        log("run %s deleted" % name)
        return {"type": "run_delete", "ok": True, "run": name}

    def ended_rows(self):
        """Every run that is neither running nor paused, newest first: what
        it ran, when, and whether it has a report. Each is read again only
        when its run.yaml or report changes."""
        busy = {os.path.realpath(c.run_dir) for c in self.children.values()}
        busy |= {os.path.realpath(r.dir) for r in self.paused.values()}
        rows = []
        for name in runs_module.runs():
            path = runs_module.run_path(name)
            if os.path.realpath(path) in busy:
                continue
            try:
                stamp = (os.stat(os.path.join(path, runs_module.RUN_FILE)).st_mtime,
                         os.path.isfile(os.path.join(path, runs_module.REPORT_FILE)))
            except OSError:
                continue
            cached = self.ended_cache.get(name)
            if cached is None or cached[0] != stamp:
                cached = (stamp, self.ended_summary(runs_module.open_run(path)))
                self.ended_cache[name] = cached
            rows.append(cached[1])
        rows.sort(key=lambda r: str(r.get("started_at") or ""), reverse=True)
        return rows

    def ended_summary(self, run):
        """An ended run's registry row."""
        try:
            stations = len(run.nodeset().nodes)
        except store.StoreError:
            stations = 0
        return {"name": run.name, "state": "ended", "code": None,
                "run": os.path.relpath(run.dir, SIM_DIR), "time": run.meta.get("time"),
                "started": run.started_at(), "ended": run.ended_at(),
                "started_at": run.meta.get("started"),
                "geodata": run.meta.get("geodata"), "nodeset": run.meta.get("nodeset"),
                "script": run.meta.get("script"), "snapshot": run.meta.get("snapshot"),
                "dirty": False, "stations": stations, "counts": {}, "errors": [], "tail": [],
                "pace": None, "mode": None, "rate": None, "observed": None,
                "t": (run.meta.get(runs_module.PAUSED) or {}).get("t") or run.last_t(),
                "plan": None, "phase": None, "eta": None, "report": run.has_report()}

    def paused_summary(self, name, run):
        """A paused simulation's registry row: what it ran, and where it ended."""
        paused = run.paused or {}
        try:
            stations = len(run.nodeset().nodes)
        except store.StoreError:
            stations = 0
        return {"name": name, "state": "paused", "code": None, "run": os.path.relpath(run.dir, SIM_DIR),
                "time": run.meta.get("time"), "started": run.started_at(),
                "ended": run.ended_at(), "paused_at": paused.get("at"),
                "geodata": run.meta.get("geodata"), "nodeset": run.meta.get("nodeset"),
                "script": run.meta.get("script"), "snapshot": run.meta.get("snapshot"),
                "dirty": False, "stations": stations, "counts": {}, "errors": [], "tail": [],
                "pace": None, "mode": None, "rate": None, "observed": None, "t": paused.get("t"),
                "plan": None, "phase": None, "eta": None, "report": run.has_report()}

    async def sim_stop(self, msg):
        name = msg.get("name")
        child = self.children.get(name)
        if child is None:
            raise ValueError("no simulation named %s" % name)
        await child.stop()
        self.children.pop(name, None)
        for conn in list(self.conns):
            conn.close_upstream(name)
        self.sidecars.release(child.holder)
        self.changed = True
        log("%s: stopped" % name)
        return {"type": "sim_stop", "ok": True, "name": name}

    def child_gone(self, child):
        for conn in list(self.conns):
            conn.close_upstream(child.name)
        if self.children.get(child.name) is child:
            self.sidecars.release(child.holder)
        if not any(c.net == child.net and c.state != "exited" for c in self.children.values()):
            self.release_net(child.net)
        self.changed = True

    async def forward_load(self, conn, name, msg):
        """A `sim_load` or `snapshot_load` on its way to a running child: the
        tables computed and the sidecar held first, then the message passed
        on with `sidecar` added."""
        child = self.children.get(name)
        try:
            if child is None or child.state != "running":
                raise ValueError("simulation %s is not running" % name)
            if msg["type"] == "sim_load" and not msg.get("run"):
                gd = load_geodata(msg["geodata"])
                ns = nodeset_module.load(msg["nodeset"])
                sidecar = await self.sidecars.hold(child.holder, gd)
                await self.compute_losses(gd.name, ns.name, losses_module.bands_of(ns, gd),
                                          sidecar, {"sim": name})
            else:
                path = os.path.join(runs_module.snapshot_path(msg["name"]),
                                    runs_module.GEODATA_FILE)
                gd = read_geodata(path)
                sidecar = None
                with contextlib.suppress(store.StoreError):
                    sidecar = await self.sidecars.hold(child.holder, gd)
            child.sidecar = sidecar
            msg = dict(msg, sidecar=sidecar)
        except (store.StoreError, ValueError, KeyError, OSError) as err:
            await conn.send({"type": "error", "sim": name, "text": str(err)})
            return
        upstream = await self.open_upstream(conn, name)
        if upstream is not None:
            with contextlib.suppress(ConnectionError, RuntimeError):
                await upstream.send_str(json.dumps(msg))

    # ---- the editors -----------------------------------------------------

    def planner_path(self, gd):
        return "/planner/%s/" % gd.name if gd.is_pack else None

    async def open_geodata(self, conn, gd):
        """Hold the geodata's sidecar for this socket: {geodata, planner}."""
        await self.sidecars.hold(conn.holder, gd)
        return {"geodata": gd.as_dict(), "planner": self.planner_path(gd)}

    @staticmethod
    def new_path(path, what, name):
        if os.path.exists(path):
            raise store.StoreError("there is already a %s called %r" % (what, name))
        return path

    @staticmethod
    def old_path(path, what, name):
        if not os.path.isfile(path):
            raise store.StoreError("no %s called %r" % (what, name))
        return path

    def write_nodeset(self, name, data, new):
        path = nodeset_module.nodeset_path(name)
        (self.new_path if new else self.old_path)(path, "nodeset", name)
        nodeset_module.write(path, nodeset_module.parse(data, "nodeset %s" % name),
                             None if new else nodeset_module.comment_of(path))
        return {"nodeset": nodeset_module.load(name).as_dict()}

    def write_geodata(self, name, data, new):
        path = geodata_module.geodata_path(name)
        (self.new_path if new else self.old_path)(path, "geodata", name)
        if geodata_module.PACK in geodata_module.parse(data, path) and planner_web() is None:
            raise store.StoreError(NO_PLANNER % name)
        geodata_module.write(path, data)
        return {"geodata": load_geodata(name).as_dict()}

    def script_reply(self, name):
        path = self.old_path(script_module.script_path(name), "script", name)
        return {"script": script_module.describe(path, name), "text": script_module.read(name)}

    async def editor(self, conn, verb, msg):
        """One editor verb, answered to the asking socket."""
        name = msg.get("name")
        if verb == "device_list":
            # The workspace's own catalogues change as it builds; the web's
            # are refreshed at start and on the tab's Refresh.
            local = devices_module.builds_sources()
            if local:
                await devices_module.refresh(local, say=lambda line: None)
            rows = await asyncio.to_thread(devices_module.listing)
            return {"devices": rows, "arch": devices_module.machine_arch()}
        if verb == "device_refresh":
            said = []
            results = await devices_module.refresh(msg.get("sources") or None, say=said.append)
            return {"results": results, "said": said}
        if verb == "geodata_list":
            rows = []
            for each in geodata_module.names():
                try:
                    rows.append(load_geodata(each).as_dict())
                except store.StoreError as err:
                    rows.append({"name": each, "error": str(err)})
            return {"geodata": rows}
        if verb == "geodata_open":
            return await self.open_geodata(conn, load_geodata(name))
        if verb == "geodata_close":
            self.sidecars.release(conn.holder)
            return {}
        if verb == "geodata_new":
            data = {k: msg[k] for k in (geodata_module.PACK, geodata_module.SYNTHETIC)
                    if msg.get(k)}
            return self.write_geodata(store.check_name(name, "geodata"), data, new=True)
        if verb in ("geodata_save", "geodata_save_as"):
            return self.write_geodata(store.check_name(name, "geodata"), msg["data"],
                                      new=verb == "geodata_save_as")
        if verb == "nodeset_list":
            # The extent is the manifest's: listing needs no planner.
            bbox = geodata_module.load(msg["geodata"]).bbox if msg.get("geodata") else None
            rows = []
            for each in nodeset_module.names():
                try:
                    row = nodeset_module.summary(each)
                    if bbox is not None:
                        row["inside"] = nodeset_module.load(each).inside(bbox)
                    rows.append(row)
                except store.StoreError as err:
                    rows.append({"name": each, "error": str(err)})
            return {"nodesets": rows}
        if verb == "nodeset_open":
            return {"nodeset": nodeset_module.load(name).as_dict()}
        if verb == "nodeset_new":
            return {"nodeset": nodeset_module.create(store.check_name(name, "nodeset")).as_dict()}
        if verb in ("nodeset_save", "nodeset_save_as"):
            return self.write_nodeset(store.check_name(name, "nodeset"), msg["data"],
                                      new=verb == "nodeset_save_as")
        if verb == "nodeset_import":
            return self.import_nodeset(msg)
        if verb == "script_list":
            return {"scripts": [script_module.describe(script_module.script_path(each), each)
                                for each in script_module.names()]}
        if verb == "script_open":
            return self.script_reply(name)
        if verb == "script_new":
            script_module.write(store.check_name(name, "script"),
                                msg.get("text") or script_module.DEFAULT_TEXT, new=True)
            return self.script_reply(name)
        if verb in ("script_save", "script_save_as"):
            script_module.write(store.check_name(name, "script"), msg["text"],
                                new=verb == "script_save_as")
            return self.script_reply(name)
        if verb == "script_run":
            return await self.script_run(msg)
        if verb == "script_stop":
            run = self.script_runs.get(msg.get("run"))
            if run is None:
                raise ValueError("no script run %s" % msg.get("run"))
            await run.stop()
            return {"run": run.id}
        if verb == "script_log":
            run = self.script_runs.get(msg.get("run"))
            if run is None:
                raise ValueError("no script run %s" % msg.get("run"))
            return {"run": run.id, "lines": list(run.lines)}
        if verb == "snapshot_list":
            rows = []
            for each in runs_module.snapshots():
                with contextlib.suppress(store.StoreError):
                    info = runs_module.snapshot_info(each)
                    rows.append({"name": each, **{k: v for k, v in info.items()
                                                  if k != "builds"}})
            return {"snapshots": rows}
        if verb == "losses_compute":
            gd = load_geodata(msg["geodata"])
            ns = nodeset_module.load(msg["nodeset"])
            bands = [str(b) for b in msg["bands"]] if msg.get("bands") \
                else losses_module.bands_of(ns, gd)
            sidecar = await self.sidecars.hold(conn.holder, gd)
            tables = await self.compute_losses(gd.name, ns.name, bands, sidecar,
                                               {"nodeset": ns.name})
            return {"geodata": gd.name, "nodeset": ns.name,
                    "tables": {band: {"path": path, "cached": hit}
                               for band, (path, hit) in tables.items()}}
        if verb == "coverage":
            return await self.coverage(conn, msg)
        raise ValueError("unknown verb %s" % verb)

    def import_nodeset(self, msg):
        """A planner CSV as a new nodeset, its transmit powers in the nodes' radios."""
        name = store.check_name(msg["name"], "nodeset")
        path = nodeset_module.nodeset_path(name)
        self.new_path(path, "nodeset", name)
        fmt = msg.get("format") or "sites"
        if fmt not in ("sites", "nodes"):
            raise ValueError("an import is format sites (planner optimize) or nodes "
                             "(planner nodes import)")
        source, tmp = msg.get("path"), None
        if not source:
            fd, tmp = tempfile.mkstemp(suffix=".csv")
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(msg["text"])
            source = tmp
        extra = {}
        if msg.get("height_m"):
            extra["height_m"] = float(msg["height_m"])
        if msg.get("device"):
            extra["device"] = str(msg["device"])
        try:
            if fmt == "sites":
                data = nodeset_module.import_sites_csv(source, **extra)
            else:
                data = nodeset_module.import_nodes_csv(source, **extra)
        finally:
            if tmp:
                os.unlink(tmp)
        nodeset_module.write(path, data, "imported from %s (%s)" % (
            os.path.basename(msg.get("path") or "an upload"), fmt))
        log("imported %d nodes into nodeset %s" % (len(data["nodes"]), name))
        return {"nodeset": nodeset_module.load(name).as_dict()}

    async def script_run(self, msg):
        """A script's `main` on a simulation: the one named, or a new one
        started first from geodata and a nodeset, with the script as setup,
        which stops again when main ends (its run directory stays)."""
        name = msg.get("name")
        info = script_module.describe(
            self.old_path(script_module.script_path(name), "script", name), name)
        if info.get("error"):
            raise store.StoreError(info["error"])
        if not info[script_module.MAIN]:
            raise store.StoreError("script %s has no main(sim) to run" % name)
        sim = msg.get("sim")
        if not sim:
            spec = {"geodata": msg.get("geodata"), "nodeset": msg.get("nodeset"),
                    "time": msg.get("time") or "real", "build": msg.get("build"),
                    "name": msg.get("sim_name")}
            # The new simulation's setup: the one asked for, else the script's own.
            if msg.get("setup"):
                spec["script"] = msg["setup"]
            elif info[script_module.SETUP]:
                spec["script"] = name
            sim = (await self.sim_new({k: v for k, v in spec.items() if v}))["name"]
        child = self.children.get(sim)
        if child is None or child.state != "running":
            raise ValueError("simulation %s is not running" % sim)
        run = ScriptRun(self, "r%d" % next(self.run_ids), name, sim, child.run_dir,
                        owns_sim=not msg.get("sim"))
        self.script_runs[run.id] = run
        await run.start(self.bind_port)
        self.changed = True
        # Not `sim`: a message carrying `sim` is that simulation's own to a page.
        return {"run": run.id, "simulation": sim}

    async def coverage(self, conn, msg):
        """Each node's coverage raster: those in the cache said at once, the
        rest computed one at a time through the sidecar and sent to this
        socket as `coverage_tile` as each lands."""
        gd = load_geodata(msg["geodata"])
        if not gd.is_pack:
            raise ValueError("geodata %s is synthetic: its coverage is worked out on the page"
                             % gd.name)
        tiles, todo = [], []
        for node in msg.get("nodes") or ():
            raster_key = coverage_module.key(gd, node)
            hit = coverage_module.cached(gd.name, raster_key) is not None
            tiles.append({"node": node["name"], "key": raster_key, "cached": hit})
            if not hit and (gd.name, raster_key) not in self.sweeps.busy:
                todo.append((node, raster_key))
        if todo:
            sidecar = await self.sidecars.hold(conn.holder, gd)

            async def work():
                for node, raster_key in todo:
                    self.sweeps.busy.add((gd.name, raster_key))
                    try:
                        await self.sweeps.raster(sidecar, gd, node)
                        await conn.send({"type": "coverage_tile", "geodata": gd.name,
                                         "node": node["name"], "key": raster_key})
                    except store.StoreError as err:
                        await conn.send({"type": "coverage_error", "geodata": gd.name,
                                         "node": node["name"], "error": str(err)})
                    finally:
                        self.sweeps.busy.discard((gd.name, raster_key))
            asyncio.ensure_future(work())
        return {"geodata": gd.name, "tiles": tiles}

    # ---- the registry, to everyone ---------------------------------------

    def sims_message(self):
        return {"type": "sims", "port": self.args.public_port,
                "sims": [c.summary() for c in self.children.values()]
                        + [self.paused_summary(n, r) for n, r in self.paused.items()]
                        + self.ended_rows(),
                "script_runs": [r.summary() for r in self.script_runs.values()],
                **simd_module.store_lists()}

    async def refresh_devices(self):
        """Every catalogue's newest devices, once at start, in the background;
        a page on the Devices tab lists again when it is told."""
        try:
            results = await devices_module.refresh(say=lambda line: log("devices: " + line))
        except Exception as err:            # noqa: BLE001 - a start that goes on without them
            log("devices: refresh failed: %s" % err)
            return
        if any(r["state"] == "fetched" for r in results):
            self.broadcast({"type": "devices_changed"})

    async def report(self):
        """The registry to every socket once a wall second, and on a change
        as soon as the next tick. Pace and estimate move every second anyway."""
        while True:
            await asyncio.sleep(REPORT_S if not self.changed else 0.1)
            self.changed = False
            if self.conns:
                text = json.dumps(self.sims_message())
                await asyncio.gather(*(c.send(text) for c in list(self.conns)))

    # ---- the control websocket -------------------------------------------

    async def ws_control(self, request):
        ws = web.WebSocketResponse(heartbeat=30, max_msg_size=0)
        await ws.prepare(request)
        conn = Conn(ws, request.query.get("sim") or None,
                    request.query.get("quiet", "") not in ("", "0"))
        self.conns.add(conn)
        await conn.send({"type": "hello", "front": True, "port": self.args.public_port})
        await conn.send(self.sims_message())
        if conn.selected and await self.open_upstream(conn, conn.selected) is None:
            await conn.send({"type": "error", "text": "simulation %s is not running"
                             % conn.selected})
        try:
            async for message in ws:
                if message.type is not WSMsgType.TEXT:
                    continue
                try:
                    msg = json.loads(message.data)
                except ValueError:
                    continue
                if isinstance(msg, dict):
                    await self.from_conn(conn, msg)
        finally:
            self.conns.discard(conn)
            self.sidecars.release(conn.holder)
            conn.close()
        return ws

    async def from_conn(self, conn, msg):
        kind = msg.get("type")
        if kind == "sims":
            await conn.send(self.sims_message())
        elif kind in SIM_VERBS or kind in EDITOR_VERBS:
            # A task, so a child that takes seconds to come up, or a table
            # that takes minutes, does not hold this socket's next message.
            asyncio.ensure_future(self.verb(conn, kind, msg))
        elif kind == "select":
            name = msg.get("sim") or None
            for other in list(conn.upstreams):
                if other != name:
                    conn.close_upstream(other)
            conn.selected = name
            if name is not None:
                await self.open_upstream(conn, name)
        else:
            name = msg.pop("sim", None) or conn.selected
            if name is None:
                await conn.send({"type": "error", "text": "no simulation selected"})
                return
            if kind in ("sim_load", "snapshot_load") and not msg.get("run"):
                asyncio.ensure_future(self.forward_load(conn, name, msg))
                return
            upstream = await self.open_upstream(conn, name)
            if upstream is None:
                await conn.send({"type": "error", "sim": name,
                                 "text": "simulation %s is not running" % name})
                return
            with contextlib.suppress(ConnectionError, RuntimeError):
                await upstream.send_str(json.dumps(msg))

    async def verb(self, conn, kind, msg):
        try:
            if kind in SIM_VERBS:
                reply = await getattr(self, kind)(msg)
            else:
                reply = {"type": kind, "ok": True, **await self.editor(conn, kind, msg)}
            if kind not in QUIET_VERBS:
                self.changed = True         # the registry's lists may have moved
        except (ValueError, OSError, KeyError, TypeError, store.StoreError,
                kinds_module.CommandError, devices_module.DeviceError) as err:
            text = str(err) if not isinstance(err, KeyError) else "missing %s" % err
            reply = {"type": kind, "ok": False, "error": text}
            log("%s: %s" % (kind, text))
        await conn.send(reply)

    async def open_upstream(self, conn, name):
        """This socket's own websocket to one child, opened once and kept.

        Everything the child says comes back with `sim` spliced in, as text:
        a busy map is thousands of messages a second, and parsing each one to
        add a field would be the front's whole cost.
        """
        entry = conn.upstreams.get(name)
        if entry is not None:
            return entry[0]
        if name in conn.opening:
            return await conn.opening[name]
        child = self.children.get(name)
        if child is None or child.state != "running":
            return None
        waiter = asyncio.get_running_loop().create_future()
        conn.opening[name] = waiter
        try:
            upstream = await self.session.ws_connect(child.control_url(conn.quiet),
                                                     max_msg_size=0)
        except (aiohttp.ClientError, OSError):
            upstream = None
        del conn.opening[name]
        waiter.set_result(upstream)
        if upstream is None:
            return None
        prefix = '{"sim": %s, ' % json.dumps(name)

        async def pump():
            try:
                async for message in upstream:
                    if message.type is WSMsgType.TEXT and message.data.startswith("{"):
                        await conn.send(prefix + message.data[1:])
            finally:
                await upstream.close()
                if conn.upstreams.get(name, (None,))[0] is upstream:
                    del conn.upstreams[name]

        conn.upstreams[name] = (upstream, asyncio.ensure_future(pump()))
        return upstream

    # ---- consoles --------------------------------------------------------

    async def ws_console(self, request):
        """A station's console, pumped to the child's own console socket."""
        child = self.children.get(request.match_info["sim"])
        if child is None or child.state != "running":
            return web.Response(status=404, text="no such simulation\n")
        browser = web.WebSocketResponse(heartbeat=30)
        await browser.prepare(request)
        url = "http://127.0.0.1:%d/ws/console/%s" % (child.port, request.match_info["name"])
        try:
            async with self.session.ws_connect(url) as upstream:

                async def up():
                    async for message in browser:
                        if message.type is WSMsgType.BINARY:
                            await upstream.send_bytes(message.data)
                        elif message.type is WSMsgType.TEXT:
                            await upstream.send_str(message.data)

                async def down():
                    async for message in upstream:
                        if message.type is WSMsgType.BINARY:
                            await browser.send_bytes(message.data)
                        elif message.type is WSMsgType.TEXT:
                            await browser.send_str(message.data)

                halves = [asyncio.ensure_future(up()), asyncio.ensure_future(down())]
                _, pending = await asyncio.wait(halves, return_when=asyncio.FIRST_COMPLETED)
                for half in pending:
                    half.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await half
        except (aiohttp.ClientError, OSError) as err:
            log("console %s/%s: %s" % (child.name, request.match_info["name"], err))
        return browser

    # ---- the planner, passed through -------------------------------------

    async def planner_proxy(self, request):
        """`/planner/<geodata>/<rest>` to that pack's sidecar as `/<rest>`.

        Only geodata somebody holds has a sidecar: the page opens it
        (`geodata_open`) before it asks for tiles. The body is streamed both
        ways and left encoded as the sidecar sent it.
        """
        name = request.match_info["geodata"]
        try:
            gd = load_geodata(name)
        except store.StoreError as err:
            return web.Response(status=404, text="%s\n" % err)
        if not gd.is_pack:
            return web.Response(status=404, text="geodata %s is synthetic: it has no planner\n"
                                % name)
        url = self.sidecars.url_for(gd)
        if url is None:
            return web.Response(status=404, text="no planner runs for geodata %s: open it "
                                "first\n" % name)
        target = "%s/%s" % (url, request.match_info["tail"])
        if request.query_string:
            target += "?" + request.query_string
        headers = {k: v for k, v in request.headers.items() if k.lower() not in HOP_HEADERS}
        body = await request.read() if request.can_read_body else None
        try:
            async with self.proxy_session.request(request.method, target, headers=headers,
                                                  data=body, allow_redirects=False) as up:
                resp = web.StreamResponse(status=up.status, reason=up.reason)
                for key, value in up.headers.items():
                    if key.lower() not in HOP_HEADERS:
                        resp.headers.add(key, value)
                if up.content_length is not None:
                    resp.content_length = up.content_length
                await resp.prepare(request)
                async for chunk in up.content.iter_chunked(1 << 16):
                    await resp.write(chunk)
                await resp.write_eof()
                return resp
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            return web.Response(status=502, text="the planner for %s: %s\n" % (name, err))

    # ---- stations --------------------------------------------------------

    def route(self, label):
        """The child and the station name behind a `<label>.sim.localhost`.

        `alpha.lora` is station alpha of simulation lora; a bare `alpha` is
        allowed while one simulation runs, so an address from before there
        were several still reaches the same station.
        """
        station, _, sim = label.partition(".")
        running = [c for c in self.children.values() if c.state == "running"]
        if sim:
            child = self.children.get(sim)
            if child is None or child.state != "running":
                return None, "No simulation named %s is running.\n" % sim
            return child, None
        if len(running) == 1:
            return running[0], None
        if not running:
            return None, "No simulation is running.\n"
        return None, ("%d simulations are running; name one: "
                      "http://%s.<simulation>.sim.localhost:%s/ (%s)\n"
                      % (len(running), station, self.args.public_port,
                         ", ".join(sorted(c.name for c in running))))

    def resolve_host(self, label, path=""):
        """Where a request goes: the front's own app, or a child's port.

        A station request goes to its child's port as it stands, `Host` and
        all, and the child's own proxy picks the station off the first label.
        `/webrtc` stays here, because the answer has to be pointed at the
        front's relay before it reaches a browser outside the container.
        """
        if label is None:
            return ("127.0.0.1", self.control_port)
        child, why = self.route(label)
        if child is None:
            return proxy.Refusal("404 Not Found", why)
        if path.split("?", 1)[0] == SIGNAL_PATH:
            return ("127.0.0.1", self.control_port)
        return ("127.0.0.1", child.port)

    async def ws_signalling(self, request):
        """The child's signalling, with the answer pointed at the front's relay,
        which forwards the flow to the child's relay (webrtc.bridge)."""
        label = proxy.label_of(request.headers.get("Host", "").encode("latin-1"))
        child, why = self.route(label) if label else (None, "no station\n")
        if child is None:
            return web.Response(status=404, text=why)
        return await webrtc_module.bridge(
            request, "http://127.0.0.1:%d%s" % (child.port, SIGNAL_PATH),
            self.relay, "127.0.0.1", child.port, label, pass_host=True)

    # ---- the HTTP side ---------------------------------------------------

    async def api_sims(self, request):
        return web.json_response(self.sims_message())

    async def api_store(self, request):
        return web.json_response(simd_module.store_lists())

    async def api_table(self, request):
        """A loss table file, for the page's links layer: `?path=` as
        `losses_compute` or a run's `losses/<band>.bin` names it, absolute or
        relative to testbed/. Only a .bin under the cache or the runs is served."""
        path = os.path.normpath(os.path.join(SIM_DIR, request.query.get("path", "")))
        roots = [os.path.join(os.path.realpath(d), "") for d in (store.LOSSES_DIR, store.RUNS_DIR)]
        real = os.path.realpath(path)
        if not (real.endswith(".bin") and any(real.startswith(r) for r in roots)
                and os.path.isfile(real)):
            raise web.HTTPNotFound(text="no such loss table")
        return web.FileResponse(real, headers={"Cache-Control": "no-store",
                                               "Content-Type": "application/octet-stream"})

    async def api_report(self, request):
        """A run's report.md, as its script's report wrote it: `?run=` as the
        registry names the run, relative to testbed/."""
        run_dir = os.path.realpath(os.path.join(SIM_DIR, request.query.get("run", "")))
        runs_root = os.path.join(os.path.realpath(store.RUNS_DIR), "")
        path = os.path.join(run_dir, runs_module.REPORT_FILE)
        if not (run_dir + os.sep).startswith(runs_root) or not os.path.isfile(path):
            raise web.HTTPNotFound(text="no report for that run")
        return web.FileResponse(path, headers={"Cache-Control": "no-store",
                                               "Content-Type": "text/markdown; charset=utf-8"})

    async def api_coverage(self, request):
        """One node's coverage raster, by geodata and key, from the cache."""
        try:
            path = coverage_module.cached(request.query.get("geodata", ""),
                                          request.query.get("key", ""))
        except store.StoreError as err:
            raise web.HTTPNotFound(text=str(err)) from err
        if path is None:
            raise web.HTTPNotFound(text="no such coverage raster")
        return web.FileResponse(path, headers={"Content-Type": "application/octet-stream"})

    async def upload(self, request, suffix=".zip"):
        """A request's body into a temporary file beside the store, streamed:
        its path, for the caller to remove."""
        fd, tmp = tempfile.mkstemp(prefix=".upload-", suffix=suffix, dir=SIM_DIR)
        size = 0
        try:
            with os.fdopen(fd, "wb") as handle:
                async for chunk in request.content.iter_chunked(1 << 16):
                    size += len(chunk)
                    if size > MAX_UPLOAD:
                        raise web.HTTPRequestEntityTooLarge(max_size=MAX_UPLOAD,
                                                            actual_size=size)
                    handle.write(chunk)
        except BaseException:
            os.unlink(tmp)
            raise
        return tmp

    async def api_device_import(self, request):
        """POST a device zip, `?name=` the name it was given: into `imported`."""
        tmp = await self.upload(request)
        try:
            got = await asyncio.to_thread(devices_module.import_zip, tmp,
                                          request.query.get("name", ""))
        except devices_module.DeviceError as err:
            return web.json_response({"ok": False, "error": str(err)})
        finally:
            os.unlink(tmp)
        log("imported device %s (%s)" % (got["ref"], got["name"]))
        return web.json_response({"ok": True, "ref": got["ref"], "name": got["name"]})

    async def api_geodata_import(self, request):
        """POST a pack zip, `?name=` the geodata it becomes: into the planner's
        packs, and a geodata file naming it."""
        tmp = await self.upload(request)
        try:
            gd = await asyncio.to_thread(geodata_module.import_pack, tmp,
                                         request.query.get("name", ""))
        except store.StoreError as err:
            return web.json_response({"ok": False, "error": str(err)})
        finally:
            os.unlink(tmp)
        self.changed = True
        log("imported pack %s as geodata %s" % (gd.pack_dir, gd.name))
        return web.json_response({"ok": True, "geodata": gd.as_dict()})

    def app(self):
        app = web.Application(client_max_size=64 << 20)
        app.router.add_get(SIGNAL_PATH, self.ws_signalling)
        app.router.add_get("/ws", self.ws_control)
        app.router.add_get("/ws/console/{sim}/{name}", self.ws_console)
        app.router.add_get("/api/sims", self.api_sims)
        app.router.add_get("/api/store", self.api_store)
        app.router.add_get("/api/table", self.api_table)
        app.router.add_get("/api/coverage", self.api_coverage)
        app.router.add_get("/api/report", self.api_report)
        app.router.add_post("/api/devices/import", self.api_device_import)
        app.router.add_post("/api/geodata/import", self.api_geodata_import)
        app.router.add_route("*", "/planner/{geodata}/{tail:.*}", self.planner_proxy)
        app.router.add_get("/{tail:.*}", simd_module.serve_page)
        return app

    async def start_http(self):
        """The app on a loopback port, the listener and the relay on the bind,
        as simd does: one port, and `Host` decides."""
        self.runner = web.AppRunner(self.app(), access_log=None)
        await self.runner.setup()
        site = web.TCPSite(self.runner, "127.0.0.1", 0)
        await site.start()
        self.control_port = self.runner.addresses[0][1]
        host, _, port = self.args.bind.rpartition(":")
        self.listener = await proxy.serve(
            host or "0.0.0.0", int(port), self.resolve_host,
            routes="<station>.<simulation>.sim.localhost to that simulation's simd")
        webrtc_module.log = log
        self.relay = await webrtc_module.serve(host or "0.0.0.0", int(port),
                                               self.args.relay_host, self.args.relay_port)
        log("page on http://localhost:%s/, stations at "
            "http://<station>.<simulation>.sim.localhost:%s/"
            % (self.args.public_port, self.args.public_port))
        if planner_web() is None:
            log("planner-web is not built in %s (simesh build planner): packs are refused, "
                "synthetic ground works" % geodata_module.planner_repo())

    # ---- the run ---------------------------------------------------------

    async def open_sessions(self):
        self.session = aiohttp.ClientSession()
        self.proxy_session = aiohttp.ClientSession(
            auto_decompress=False, timeout=aiohttp.ClientTimeout(total=None, sock_connect=10))
        self.sidecars = Sidecars(self.session)
        self.sweeps = coverage_module.Sweeps(self.session)

    async def run(self):
        loop = asyncio.get_running_loop()
        done = loop.create_future()
        for sig in (signal.SIGINT, signal.SIGTERM):
            with contextlib.suppress(NotImplementedError):
                loop.add_signal_handler(sig, lambda: done.done() or done.set_result(None))
        os.makedirs(store.RUNS_DIR, exist_ok=True)
        for run in runs_module.paused_runs():
            self.paused[run.paused.get("simulation") or run.name] = run
        await self.open_sessions()
        await self.start_http()
        self.reporter = asyncio.ensure_future(self.report())
        self.fetcher = asyncio.ensure_future(self.refresh_devices())
        try:
            await done
        finally:
            await self.shutdown()

    async def shutdown(self):
        log("stopping")
        if self.reporter is not None:
            self.reporter.cancel()
        if self.fetcher is not None:
            self.fetcher.cancel()
        await asyncio.gather(*(r.stop() for r in self.script_runs.values()),
                             return_exceptions=True)
        await asyncio.gather(*(c.stop() for c in self.children.values()),
                             return_exceptions=True)
        for conn in list(self.conns):
            conn.close()
        if self.sidecars is not None:
            await self.sidecars.close()
        if self.runner is not None:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.runner.cleanup(), simd_module.SHUTDOWN_TIMEOUT_S)
        if self.listener is not None:
            self.listener.close()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.listener.wait_closed(),
                                       simd_module.SHUTDOWN_TIMEOUT_S)
        if self.relay is not None:
            self.relay.close()
        for session in (self.session, self.proxy_session):
            if session is not None:
                await session.close()


def parse_args(argv):
    ap = argparse.ArgumentParser(
        description="several simulations behind one port: the registry, the "
                    "children, the editors, the scripts, the planner sidecars, the page",
        epilog="Anything after `--` is given to every child simd as it stands: "
               "--noise-figure and --pairwise, say.")
    ap.add_argument("--bind", default="0.0.0.0:8800",
                    help="host:port for the page, the stations and the WebRTC "
                         "relay (default 0.0.0.0:8800)")
    ap.add_argument("--relay-host", default="127.0.0.1",
                    help="the address a browser sends the DataChannel to "
                         "(default 127.0.0.1)")
    ap.add_argument("--relay-port", type=int, default=0,
                    help="the UDP port a browser sends the DataChannel to, as "
                         "the browser sees it (default: the bind port)")
    ap.add_argument("child_args", nargs=argparse.REMAINDER,
                    help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    if args.child_args[:1] == ["--"]:
        args.child_args = args.child_args[1:]
    args.public_port = args.bind.rpartition(":")[2]
    if not args.relay_port:
        args.relay_port = int(args.public_port)
    return args


def main(argv=None):
    args = parse_args(argv)
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(Front(args).run())
    return 0


if __name__ == "__main__":
    sys.exit(main())
