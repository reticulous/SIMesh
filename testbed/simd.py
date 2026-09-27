#!/usr/bin/env python3
"""The simulated testbed as one process: ether, stations, proxy and control.

```
page ── ws /ws ─────────────────────────────────────► simd   control, one JSON object a frame
simd ── snapshot {run, geodata, nodeset, script, nodes, …} ─► page   on connect and on every load
page ── nodeset_move {name, lat, lon, settle: true} ──► simd
simd: the run's nodeset.yaml written, the move logged with T (nodeset-edits.jsonl)
simd ── node {name, …, stale: true}, nodeset {…} ────► page
simd ── GET <sidecar>/link.json × (n-1) ─────────────► planner-web   (synthetic: computed here)
simd: the node's row and column into runs/<run>/losses/<band>.bin, then into the ether
simd ── node {name, …, stale: false} ───────────────► page
script ── meta {verb: announce, tag: lora, stagger: 300, id} ► simd
simd: each station's kind turns the intent into its own lines, spread over 300 s of T
simd ── command_result {id, verb, results: {name: reply}, t} ► every page
page ── ws /ws/console/<name> ◄────────────────────► the station's pty
browser ── http <name>.sim.localhost:<port> ────────► the station's own web UI
```

One command starts this, and this is the whole testbed. It holds

- the `Ether` and its UDP endpoint, in this process's own event loop, so the
  loss tables reach the medium by direct call and never over a wire;
- the stations, one firmware process on a pty each, with a supervisor apiece;
- one listener on the bind port, which routes by `Host`: a
  `<name|id>.sim.localhost` request is proxied to that station's own web UI,
  and everything else is the control page and its websockets;
- the loaded **run** (runs.py): the geodata, the run's own copy of the
  nodeset, the script's copy when it has one, the loss tables the ether
  reads, and every station's directory. A run is made from geodata, a
  nodeset and optionally a script (`sim_load`) or from a snapshot
  (`snapshot_load`), or is handed over made already (`--run`, which is how
  the front starts a simulation).

Several of these run at once as the children of front.py, each on its own
ports, network and run directory, with the front in front of all of them.

**Setup.** A station that boots with no state is set up: its node's
declared settings (name, role, the radio's figures) in its kind's own lines,
then the run's script's `setup(node)` when it has one, which runs here, in
this loop, and must only await (script.py), then whatever starts its radio.
A station with state is left alone.

Loss tables. The run holds one table per band its nodes' carriers use
(`losses.bands_of`), and the ether rules on every frame from them, with the
nodeset's offsets added (`losses.with_offsets`). A nodeset edit that changes
the geometry (a move, a height, a node added) recomputes the touched nodes'
rows and columns into the run's copy, as a task: through the planner sidecar
named by `--sidecar` or by the load that brought the geodata in, or on
synthetic ground here. Until a row lands the ether keeps the old one, and the
node's `node` message says `stale: true`. The cache in `losses/` is never
written by an edit.

Messages, page → simd (anything else is ignored, as on the wire):

```
sim_load {geodata, nodeset, script?, build?, sidecar?}   a new run from these, factory fresh
sim_load {run, sidecar?}                            adopt a run directory made already
snapshot_load {name, build?, sidecar?}              a new run from a snapshot, state and all
snapshot_save_as {name}                             this moment, flushed, as a snapshot
nodeset_add {name, lat, lon, id?, height_m?, height_from?, gain_dbi?, device?, role?, radio?, tags?}
nodeset_move {name, lat, lon, height_m?, settle?}   settle false: a drag still moving
nodeset_remove {name}
nodeset_set {name, id?, lat?, lon?, height_m?, height_from?, gain_dbi?, device?, role?, radio?, tags?}
nodeset_offset {between: [a, b], db, note?}         db 0 removes it
levels {name, freq?}                                what the others would hear from it
command {line, name? | names? | tag?, kind?, stagger?, after?, id?}
                                                    one line on the stations chosen, all of one kind
meta {verb, args?, name? | names? | tag?, kind?, stagger?, after?, id?}
                                                    an intent, in each station's own lines
node_reset {name} · node_factory_reset {name} · reset_all · factory_reset_all
start_all · stop_all
plan {phases: [{name, until}]}                      a driver's phases, T in µs
```

Messages, simd → page:

```
snapshot {run, geodata, nodeset, script, bands, nodes: [node…], port, clock,
          geodata_names, nodesets, scripts, snapshots, medium}
node {name, id, kind, device, device_name, web, lat, lon, height_m, height_from,
      gain_dbi, tags, declared_role, declared_radio, role, status, stale,
      mode?, freq?, sf?, bw?}
node_gone {name}
nodeset {name, dirty, geometry_hash, nodes, offsets}
store {geodata_names, nodesets, scripts, snapshots}   after a snapshot is saved
losses_progress {band, done, total}                 while a sim_load computes
levels {name, freq, heard: {name: dBm}}
command_result {id?, line | verb, name, results: {name: text}, t}
clock {mode, rate, t, observed, barriers, slow_idles, plan}
tx {name, eid, freq, t_start, t_end} · rx {name, from, eid, verdict, level}
radio {name, mode, freq, sf, bw}
notice {text}                                       something done that a person should know
error {text}
```

**Choosing stations.** `command` and `meta` go to the stations that are up
(or in setup) among the ones named: `name`, a list of `names`, every one
carrying `tag`, or all of them. A line is in one kind's language, so the
chosen stations must all be of one kind, or `kind` must narrow them to one;
an intent goes to every kind, each in its own lines. `meta`'s verbs are the
intents (kinds.INTENTS) and `address`, each station's LXMF delivery address;
`message` and `path` take `to`, a node's name, whose address is asked of it
first, and `peer_tcp` takes `to` and `port`.

`role` is what the station's kind reads from it (kinds.ROLES), polled; None
when the kind cannot say. `declared_role` and `declared_radio` are the
nodeset's. `?quiet=1` on the websocket leaves out tx, rx, radio and levels,
which is most of the traffic on a busy network and nothing a driver or the
front's registry reads. See INTERNALS.md for why it is shaped this way.

Nothing here blocks. Every wait is an awaitable, the stations' ptys are read
on a thread of their own (stations.Ptys) that hands this loop what it acts
on, and a framed-RPC query is a future that a reply handed over completes.
"""

import argparse
import asyncio
import contextlib
import json
import os
import signal
import sys

from aiohttp import WSMsgType, web

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SIM_DIR)
sys.path.insert(0, os.path.join(SIM_DIR, "..", "ether"))

import ether as ether_module       # noqa: E402 - the paths are set just above
import slt                         # noqa: E402
import geodata as geodata_module   # noqa: E402
import kinds as kinds_module       # noqa: E402
import losses as losses_module     # noqa: E402
import nodeset as nodeset_module   # noqa: E402
import proxy                       # noqa: E402
import runs as runs_module         # noqa: E402
import script as script_module     # noqa: E402
import stations as stations_module  # noqa: E402
import store                       # noqa: E402
import webrtc as webrtc_module     # noqa: E402
from simesh import setup as setup_module  # noqa: E402

UI_DIST = os.path.join(SIM_DIR, "ui", "dist", "spa")
DEFAULT_RUN = os.path.join(store.RUNS_DIR, "simd")

ROLE_POLL_S = 6.0           # how often a station is asked what it does for the mesh
SETUP_TIMEOUT_S = 90.0      # how long a fresh station has to say it is up
SHUTDOWN_TIMEOUT_S = 5.0    # how long a connection may hold up a stop
SETTLE_S = 15.0             # how long a first boot gets to finish landing what setup asked for
CLOCK_REPORT_S = 1.0        # how often the page hears how fast T is going
WEBRTC_PORT = 4433          # the station's own DataChannel port (s.net.webrtc_port)
SIGNAL_PATH = "/webrtc"     # the one station route simd keeps for itself
LOUD = ("tx", "rx", "radio", "levels")  # what a `quiet` control socket is not sent
NODE_FIELDS = ("id", "lat", "lon", "height_m", "height_from", "gain_dbi", "device", "role",
               "radio", "tags")
ADDRESS = "address"         # meta's one verb that is not an intent: each station's address

log = stations_module.log


# ---------------------------------------------------------------------------
# Where a run goes, and what a page can pick from
# ---------------------------------------------------------------------------

def free_run_dir(path):
    """`path`, or `path-2`, `path-3`…: the first that holds no run yet. A run
    is an output and is never written over."""
    path = os.path.abspath(path)
    candidate, n = path, 2
    while os.path.exists(os.path.join(candidate, runs_module.RUN_FILE)):
        candidate = "%s-%d" % (path, n)
        n += 1
    return candidate


def store_lists():
    """Everything a page can pick from, by name."""
    return {"geodata_names": geodata_module.names(), "nodesets": nodeset_module.names(),
            "scripts": script_module.names(), "snapshots": runs_module.snapshots()}


async def serve_page(request):
    """The built page, with every unknown path falling back to it.

    One page and no router beyond it, so a deep link is the same document;
    a request for a file that is really there gets that file.
    """
    path = request.match_info.get("tail", "")
    if path and not path.startswith("."):
        candidate = os.path.normpath(os.path.join(UI_DIST, path))
        if candidate.startswith(UI_DIST) and os.path.isfile(candidate):
            return web.FileResponse(candidate)
    page = os.path.join(UI_DIST, "index.html")
    if os.path.isfile(page):
        return web.FileResponse(page)
    return web.Response(
        text="The control page has not been built.\n\n"
             "In SIMesh/testbed/ui run `npm install && npx quasar build`.\n",
        content_type="text/plain")


# ---------------------------------------------------------------------------
# The process
# ---------------------------------------------------------------------------

class Simd:
    """The testbed: the medium, the stations, the run and the page."""

    def __init__(self, args):
        self.args = args
        self.run_base = args.run            # where new runs go: this, or this-2, …
        self.ether_addr = args.ether
        self.ether = None
        self.ether_transport = None
        self.run = None                     # a runs.Run, or None until one is loaded
        self.geodata = None
        self.nodeset = None                 # the run's own copy, as edited
        self.setup_fn = None                # the run's script's `setup`, or None
        self.kinds = {}                     # device ref -> Kind
        self.builds = {}                    # device ref -> what it resolved to
        self.tables = {}                    # band -> slt.Table, the model's own
        self.sidecar = args.sidecar         # planner-web base URL, for a pack
        self.stale = set()                  # nodes whose rows are not in the tables yet
        self.pending_rows = set()           # of those, the ones still to compute
        self.rows_task = None
        self.stations = {}                  # node name -> Station
        self.pages = {}                     # the control websockets -> quiet
        self.consoles = {}                  # node name -> set of websockets
        self.poller = None
        self.stopping = False
        self.control_port = None            # where the aiohttp app really listens
        self.front = None
        self.runner = None
        self.relay = None                   # the WebRTC UDP relay
        self.starter = None                 # the staggered start, while it runs
        self.observed_rate = None           # seconds of T per second of wall, lately
        self.clock_watch = None
        self.plan = None                    # a driver's phases, {t, phases: [{name, until}]}

    # ---- lookups ---------------------------------------------------------

    def name_of(self, sid):
        """The node name behind a station id, or None if the nodeset has none."""
        return self.ether.names.get(sid) if self.ether is not None else None

    def node_for_label(self, label):
        """The node behind a `<label>.sim.localhost`, by name or by id.

        Behind the front the label is `<station>.<simulation>`; the front has
        already chosen this simulation by the second part, so only the first
        is this process's business.
        """
        if self.nodeset is None or label is None:
            return None
        label = label.split(".", 1)[0]
        if label in self.nodeset.nodes:
            return label
        if label.isdigit():
            return self.nodeset.by_id(int(label))
        return None

    def kind_of(self, name):
        node = self.nodeset.nodes.get(name) if self.nodeset else None
        return self.kinds.get(kinds_module.device_of(node)) if node else None

    def ids(self):
        return {name: node["id"] for name, node in self.nodeset.nodes.items()}

    def resolve_host(self, label, path=""):
        """Where a request with this `Host` label goes: a station, or simd.

        A label is a name or an id out of `<label>.sim.localhost`; anything
        else — `localhost`, an IP, no Host at all — is the control page, which
        is why one port serves both.

        One station route is simd's own: `/webrtc`, the DataChannel's
        signalling. simd stands in the middle of it so it can point the SDP
        answer at the relay, because the address the station would offer is
        inside the container and the browser is outside it.
        """
        if label is None:
            return ("127.0.0.1", self.control_port)
        name = self.node_for_label(label)
        if name is None:
            return None
        kind = self.kind_of(name)
        port = kind.web_port() if kind else None
        if port is None:
            return proxy.Refusal(
                "404 Not Found", "%s is a %s station, which has no web UI.\n"
                % (name, kind.name if kind else "kindless"))
        if path.split("?", 1)[0] == SIGNAL_PATH:
            return ("127.0.0.1", self.control_port)
        return (stations_module.bind_addr(self.nodeset.nodes[name]["id"]), port)

    # ---- the medium ------------------------------------------------------

    async def start_ether(self, record_dir=None):
        """A fresh ether on the bind, recording into `record_dir`.

        One per run: T starts at zero with the run, and the record is the
        run's. Every station has been stopped before it is replaced.
        """
        if self.ether is not None:
            self.ether.close()
            # A transport lets go of its socket on the loop's next turn.
            for _ in range(3):
                await asyncio.sleep(0)
        loop = asyncio.get_running_loop()
        record = None
        if record_dir is not None:
            os.makedirs(record_dir, exist_ok=True)
            record = os.path.join(record_dir, "record.tsv")
        bind = ether_module.parse_bind(self.ether_addr)
        physics = ether_module.Physics(self.args.noise_figure)
        self.ether_transport, self.ether = await loop.create_datagram_endpoint(
            lambda: ether_module.Ether(record, physics=physics, time_mode=self.args.time,
                                       pairwise=self.args.pairwise, seed=self.args.seed,
                                       epoch=self.args.epoch),
            local_addr=bind)
        self.ether.on_tx = self.ether_tx
        self.ether.on_rx = self.ether_rx
        self.ether.on_station = self.ether_station
        self.ether.on_drain = self.drain_consoles
        if record is not None:
            log("ether on %s, recording to %s" % (self.ether_addr, record))
            log("time: %s; %s; %s rule" % (
                ether_module.describe_time(self.ether.mode, self.ether.rate),
                physics.describe(), "pairwise" if self.args.pairwise else "receiver-centred"))
        if self.ether.clock.virtual and not os.path.exists(kinds_module.SHIM):
            log("error: no time shim at %s: a virtual-time run needs it "
                "(see SIMesh/README.md)" % kinds_module.SHIM)

    @property
    def virtual(self):
        return self.ether is not None and self.ether.clock.virtual

    async def sleep(self, seconds):
        """Wait on the run's clock: conductor time in a virtual run."""
        await self.ether.sleep(seconds)

    def clock_message(self):
        """What the page is told about time: the mode, T, and how fast it goes."""
        if self.ether is None:
            return None
        return {"type": "clock", "mode": self.ether.mode, "rate": self.ether.rate,
                "t": self.ether.now(), "observed": self.observed_rate,
                "barriers": self.ether.barriers,
                "slow_idles": sum(st.slow_idles for st in self.ether.stations.values()),
                "plan": self.plan}

    async def watch_clock(self):
        """Tell the page how fast T is going, once a second of wall."""
        loop = asyncio.get_running_loop()
        last_wall, last_t = loop.time(), self.ether.now()
        slow_seen = {}
        while not self.stopping:
            await asyncio.sleep(CLOCK_REPORT_S)
            wall, t = loop.time(), self.ether.now()
            if t < last_t:                  # a new run, a new ether
                last_t, slow_seen = t, {}
            if wall > last_wall:
                self.observed_rate = (t - last_t) / 1_000_000.0 / (wall - last_wall)
            if self.virtual and t == last_t and self.ether.busy():
                log("T stands at %.3f s, waiting on station(s) %s" % (
                    t / 1e6, ", ".join(str(s) for s in self.ether.waiting_on())))
            if self.virtual:
                slow = {sid: st.slow_idles - slow_seen.get(sid, 0)
                        for sid, st in self.ether.stations.items()}
                slow = {sid: n for sid, n in slow.items() if n > 0}
                if slow and self.observed_rate is not None and self.observed_rate < 2:
                    log("T at %.3f s held by busy station(s): %s" % (t / 1e6, ", ".join(
                        "%s %d" % (self.name_of(sid) or sid, n)
                        for sid, n in sorted(slow.items(), key=lambda x: -x[1])[:5])))
                slow_seen = {sid: st.slow_idles for sid, st in self.ether.stations.items()}
            last_wall, last_t = wall, t
            self.broadcast(self.clock_message())

    def apply_to_ether(self):
        """Give the medium the tables with the offsets on them, and which
        station id is which node with what antenna gain, wholesale.

        Cheap enough to redo outright on every change of ids, gains, offsets
        or the node set; a node the tables have no row for yet is in the
        medium but hears nothing and is heard by nothing until its row lands.
        """
        if self.nodeset is None:
            self.ether.clear()
            return
        nodes = self.nodeset.nodes
        self.ether.set_losses(losses_module.with_offsets(self.tables, self.nodeset),
                              {name: node["id"] for name, node in nodes.items()},
                              {node["id"]: node["antenna"]["gain_dbi"] for node in nodes.values()})

    def drain_consoles(self, sids, done):
        """The ether is about to move T past stations `sids`, which have run
        since it last did: read everything they printed first (their replies,
        the marker), so what it makes the testbed do happens at this T."""
        wanted = set(sids)
        drains = [s.drain for s in self.stations.values()
                  if s.node_id in wanted and s.drain is not None]
        if not drains:
            done()
            return
        stations_module.ptys().call(stations_module.catch_up, drains,
                                    asyncio.get_running_loop(), done)

    def ether_tx(self, sid, eid, freq, t_start, t_end):
        name = self.name_of(sid)
        if name is not None:
            self.broadcast({"type": "tx", "name": name, "eid": eid, "freq": freq,
                            "t_start": t_start, "t_end": t_end})

    def ether_rx(self, sid, from_sid, eid, verdict, level):
        name, sender = self.name_of(sid), self.name_of(from_sid)
        if name is not None and sender is not None:
            self.broadcast({"type": "rx", "name": name, "from": sender,
                            "eid": eid, "verdict": verdict, "level": round(level, 1)})

    def ether_station(self, sid, state):
        """A station said what its radio is doing; the map draws only the carrier."""
        name = self.name_of(sid)
        if name is None:
            return
        self.broadcast({"type": "radio", "name": name, "mode": state.get("mode"),
                        "freq": state.get("freq"), "sf": state.get("sf"),
                        "bw": state.get("bw")})

    # ---- the loss rows ---------------------------------------------------

    def need_rows(self, names):
        """These nodes' rows and columns are out of date: mark them stale
        and have them recomputed, in the background, into the run's tables."""
        names = set(names) & set(self.nodeset.nodes)
        if not names:
            return
        self.stale |= names
        self.pending_rows |= names
        for name in names:
            self.broadcast(self.node_message(name))
        if self.rows_task is None or self.rows_task.done():
            self.rows_task = asyncio.ensure_future(self.recompute_rows())

    async def recompute_rows(self):
        """Recompute the pending rows until none are left.

        Each pass works on the nodeset as it is when the pass begins; a node
        edited again meanwhile is pending again and gets another pass. The
        new tables are written to the run's `losses/` and handed to the ether
        whole, so no frame is ruled on half a row.
        """
        run = self.run
        while self.pending_rows and run is self.run:
            names, self.pending_rows = set(self.pending_rows), set()
            ns = self.nodeset.copy()
            try:
                fresh = {}
                for band, table in list(self.tables.items()):
                    fresh[band] = await losses_module.update_nodes(
                        table, self.geodata, ns, names, self.sidecar, notice=log)
            except (store.StoreError, OSError) as err:
                self.error("recomputing losses for %s: %s" % (", ".join(sorted(names)), err))
                continue
            except Exception as err:        # noqa: BLE001 - aiohttp's, among others
                self.error("recomputing losses for %s: %r" % (", ".join(sorted(names)), err))
                continue
            if run is not self.run:
                return
            for band, table in fresh.items():
                losses_module.write_table(table, run.table_path(band))
            self.tables = fresh
            self.apply_to_ether()
            done = names - self.pending_rows
            self.stale -= done
            for name in sorted(done):
                if name in self.nodeset.nodes:
                    self.broadcast(self.node_message(name))
            log("losses recomputed for %s" % ", ".join(sorted(names)))

    # ---- stations --------------------------------------------------------

    def make_station(self, name):
        node = self.nodeset.node(name)
        station = stations_module.Station(
            name, node["id"], self.run.node_dir(name), self.kind_of(name), self.ether_addr,
            on_status=self.station_status, on_output=self.station_output,
            clock=self.ether if self.virtual else None)
        station.watchers = len(self.consoles.get(name, ()))
        return station

    def watched(self, name):
        """The console windows open on a station, as the pty thread sees it:
        it hands console bytes over only while there is one."""
        station = self.stations.get(name)
        if station is not None:
            station.watchers = len(self.consoles.get(name, ()))

    def station_status(self, station, status):
        if self.stations.get(station.name) is station:
            self.broadcast(self.node_message(station.name))

    def station_output(self, station, data):
        """Console bytes go to whoever has that station's terminal open."""
        text = data.decode("utf-8", "replace") if isinstance(data, bytes) else data
        for socket in list(self.consoles.get(station.name, ())):
            asyncio.ensure_future(self.send_console(socket, text))

    async def send_console(self, socket, text):
        try:
            await socket.send_str(text)
        except (ConnectionError, RuntimeError):
            pass

    async def after_start(self, station):
        """Wait for a station to be up, and set it up if it has never been.

        A station with state is left alone: its state is the run's, and
        setting it up again would be re-answering questions it has already
        answered. A station without is a node somebody has just put on the
        map, and it should be a working station rather than a dot waiting for
        someone to type.
        """
        if not await station.kind.wait_up(station, SETUP_TIMEOUT_S):
            log("station %s never came up (%s)" % (station.name, station.kind.name))
            return
        if not station.was_configured:
            station.set_status(stations_module.SETUP)
            try:
                await self.send_setup(station)
            except kinds_module.CommandError as err:
                self.error("setting up %s: %s" % (station.name, err))
        else:
            await self.restate_role(station)
        station.set_status(stations_module.UP)
        log("station %s is up at t %.3f s" % (station.name, self.ether.now() / 1e6))
        await self.read_role(station)
        if not station.was_configured:
            # Not everything setup asks for lands at once: an LXMF identity
            # reaches the store some seconds after the command that created it
            # has returned. One more flush once that has settled, so a station
            # reset moments after its first boot keeps what it was given
            # rather than coming back half-configured.
            await self.sleep(SETTLE_S)
            await self.flush_station(station)

    def expanded(self, name, lines):
        return kinds_module.expand_all(lines, name, self.nodeset.nodes[name]["id"], self.ids())

    async def restate_role(self, station):
        """A station of a kind that forgets its role when it restarts comes
        back up with its state but not its role: the declared one is said
        again, so a reset does not turn a transport into a client."""
        role = (self.nodeset.nodes.get(station.name) or {}).get("role")
        if not role or not station.kind.role_volatile:
            return
        try:
            await station.kind.setup(station, self.expanded(
                station.name, station.kind.lines("role", role=role)))
        except kinds_module.CommandError as err:
            self.error("telling %s its role again: %s" % (station.name, err))

    def setup_node(self, station):
        """The `simesh.setup.Node` a script's `setup` is handed for a station."""
        name, kind = station.name, station.kind

        async def run(line):
            line = kinds_module.expand(str(line), name, self.nodeset.nodes[name]["id"], self.ids())
            try:
                return await kind.setup_line(station, line)
            except kinds_module.CommandError as err:
                raise setup_module.Refused("%s: %s" % (line, err)) from err

        def lines(verb, **args):
            try:
                return kind.lines(verb, **args)
            except kinds_module.CommandError as err:
                raise setup_module.Refused(str(err)) from err

        def addr(other):
            return stations_module.bind_addr(self.nodeset.node(other)["id"])

        return setup_module.Node(name, self.nodeset.node(name), kind.name, run, lines, addr)

    async def send_setup(self, station):
        """The node's declared settings in its kind's lines, then the script's
        `setup(node)`, then what starts the radio, then a flush.

        The radio is started last so that whatever a radio reads when it
        starts, the declared figures and anything the script sets, is in
        place by then. The declared settings and the script are the whole of
        what a station is told: simd adds no settings of its own. Its kind
        does flush once they are in. A store that coalesces writes would
        otherwise come back from a Reset pressed the moment a node came up
        with none of it, and a testbed that lost its own setup that way would
        be lying about what it had configured.
        """
        node = self.nodeset.node(station.name)
        kind = station.kind
        lines = self.expanded(station.name, kind.declared(node))
        if lines:
            await kind.setup(station, lines)
        failure = None
        if self.setup_fn is not None:
            try:
                await self.setup_fn(self.setup_node(station))
            except setup_module.Refused as err:
                failure = "script setup: %s" % err
            except Exception as err:            # noqa: BLE001 - a script's own code
                failure = "script setup: %r" % err
        start = kind.radio_start() if node.get("radio") else []
        await kind.setup(station, self.expanded(station.name, start))
        if failure:
            raise kinds_module.CommandError(failure)

    async def start_station(self, name):
        if self.run is None or name not in self.nodeset.nodes or name in self.stations:
            return
        kind = self.kind_of(name)
        if kind is None:
            self.error("%s: its device %s was not resolved"
                       % (name, kinds_module.device_of(self.nodeset.nodes[name])))
            return
        if not kind.elf or not os.path.exists(kind.elf):
            self.error("%s: device %s has no binary at %s" % (name, kind.label, kind.elf))
            return
        station = self.make_station(name)
        self.stations[name] = station
        station.run(self.watch_after_start)
        self.broadcast(self.node_message(name))

    async def watch_after_start(self, station):
        """after_start, with anything it raises said rather than lost.

        It runs as a task nobody awaits, so an exception in it would vanish
        and leave the station showing whatever status it had reached.
        """
        try:
            await self.after_start(station)
        except asyncio.CancelledError:
            raise
        except Exception as err:                # noqa: BLE001 - see docstring
            self.error("%s: %r" % (station.name, err))

    async def flush_station(self, station):
        """Ask a station to commit its store before we take it away from it.

        A store may coalesce writes (reticulous holds them for a minute by
        default), and some of what a station records — an LXMF identity among
        them — lands there a little after the command that asked for it. So
        anything that stops or resets a station flushes it first, and so does
        taking a snapshot: a snapshot copied out of a store with a minute of
        writes still in RAM would be a picture of a moment that never quite
        existed. What a flush is, and whether there is one, is the kind's.

        Best effort. A station that will not answer is one whose store we
        cannot flush, and refusing to stop it over that would be worse.
        """
        if station.status not in (stations_module.UP, stations_module.SETUP):
            return
        with contextlib.suppress(kinds_module.CommandError):
            await station.kind.flush(station)

    async def flush_all(self):
        await asyncio.gather(*(self.flush_station(s) for s in self.stations.values()),
                             return_exceptions=True)

    async def stop_station(self, name, flush=True):
        station = self.stations.pop(name, None)
        if station is None:
            return
        if flush:
            await self.flush_station(station)
        await station.stop()

    async def start_all(self):
        """Start every station that is not running, spread over a minute.

        Not all at once. Two dozen firmware processes forking in the same
        instant is a thundering herd against one host, and the network it
        produces is worse than the load: stations that boot together finish
        booting together, so their first announces land on top of each other
        and the opening minute is a collision storm no real network powered up
        by hand would ever have. Spreading the starts costs nothing and makes
        the first minute of the air look like a first minute.
        """
        if self.nodeset is None:
            return
        pending = [name for name, _ in sorted(self.nodeset.nodes.items(),
                                              key=lambda item: item[1]["id"])
                   if name not in self.stations]
        if not pending:
            return
        gap = self.args.stagger / len(pending)
        for index, name in enumerate(pending):
            if index:
                await self.sleep(gap)
            # A minute is long enough for the nodeset to have moved on.
            if self.stopping or self.nodeset is None:
                return
            await self.start_station(name)

    def begin_start_all(self):
        """Run the staggered start in the background.

        The caller is handling a message from the page, and a start that took
        a minute to return would hold every other message behind it for that
        minute. Held as a task so the next load or stop can cancel it rather
        than race it.
        """
        self.cancel_start()
        self.starter = asyncio.ensure_future(self.start_all())

    def cancel_start(self):
        if self.starter is not None:
            self.starter.cancel()
            self.starter = None

    async def stop_all(self, flush=True):
        self.cancel_start()
        await asyncio.gather(*(self.stop_station(name, flush)
                               for name in list(self.stations)))

    # ---- the role poll ---------------------------------------------------

    async def read_role(self, station):
        """Ask a station what it does for the mesh, and say so.

        Read live rather than taken from the nodeset, because the setting is
        live: a person can flip it on the station itself, and the map should
        show it without the nodeset knowing. A kind that cannot be asked
        answers None, and the map shows the declared role.
        """
        try:
            role = await station.kind.role(station)
        except kinds_module.CommandError:
            return
        if role != station.role:
            station.role = role
            if self.stations.get(station.name) is station:
                self.broadcast(self.node_message(station.name))

    async def poll_roles(self):
        """Every ROLE_POLL_S of the run's clock: of T in a virtual run, so the
        questions reach the stations at the same instants in every run."""
        while not self.stopping:
            await self.sleep(ROLE_POLL_S)
            running = [s for s in self.stations.values() if s.status == stations_module.UP]
            if running:
                await asyncio.gather(*(self.read_role(s) for s in running),
                                     return_exceptions=True)

    # ---- talking to the page ---------------------------------------------

    def radio_of(self, sid):
        """The station's last stated radio, so a page that connects late has it.

        A station states its radio when it changes and not otherwise, so a page
        opened on a quiet network would wait for the next change to learn what
        anything is tuned to. The ether already holds the last one.
        """
        station = self.ether.stations.get(sid) if self.ether else None
        state = station.state(0) if station else None
        if not state:
            return {}
        return {"mode": state.get("mode"), "freq": state.get("freq"),
                "sf": state.get("sf"), "bw": state.get("bw")}

    def node_message(self, name):
        node = self.nodeset.nodes.get(name) if self.nodeset else None
        if node is None:
            return {"type": "node_gone", "name": name}
        station = self.stations.get(name)
        kind = self.kind_of(name)
        return {"type": "node", "name": name, "id": node["id"],
                "kind": kind.name if kind else None,
                "device": kinds_module.device_of(node),
                "device_name": kind.label if kind else None,
                "web": kind is not None and kind.web_port() is not None,
                "lat": node["lat"], "lon": node["lon"], "height_m": node["height_m"],
                "height_from": node["height_from"], "gain_dbi": node["antenna"]["gain_dbi"],
                "tags": list(node["tags"]), "declared_role": node.get("role"),
                "declared_radio": node.get("radio"),
                "status": station.status if station else stations_module.STOPPED,
                "role": station.role if station else None,
                "stale": name in self.stale,
                **self.radio_of(node["id"])}

    def nodeset_message(self):
        return {"type": "nodeset", **self.nodeset.as_dict()} if self.nodeset else \
            {"type": "nodeset", "name": None}

    def script_info(self):
        if self.run is None or self.run.script_path is None:
            return None
        return script_module.describe(self.run.script_path, self.run.meta.get("script"))

    def snapshot(self):
        loaded = self.run is not None
        return {"type": "snapshot",
                "run": self.run.as_dict() if loaded else None,
                "geodata": self.geodata.as_dict() if loaded else None,
                "nodeset": self.nodeset.as_dict() if loaded else None,
                "script": self.script_info(),
                "bands": sorted(self.tables),
                "nodes": [self.node_message(name)
                          for name in (self.nodeset.nodes if loaded else ())],
                "port": self.args.public_port,
                "clock": self.clock_message(),
                "medium": {"noise_figure_db": self.args.noise_figure,
                           "pairwise": self.args.pairwise},
                **store_lists()}

    def broadcast(self, message):
        text = json.dumps(message)
        loud = message.get("type") in LOUD
        for socket, quiet in list(self.pages.items()):
            if not (quiet and loud):
                asyncio.ensure_future(self.send_page(socket, text))

    async def send_page(self, socket, text):
        try:
            await socket.send_str(text)
        except (ConnectionError, RuntimeError):
            self.pages.pop(socket, None)

    def error(self, text):
        log("error: %s" % text)
        self.broadcast({"type": "error", "text": text})

    def notice(self, text):
        log(text)
        self.broadcast({"type": "notice", "text": text})

    def progress(self, band):
        """A losses progress callback that tells the page about a hundred times."""
        def show(done, total):
            step = max(1, total // 100)
            if done == total or done % step == 0:
                self.broadcast({"type": "losses_progress", "band": band,
                                "done": done, "total": total})
        return show

    # ---- what the page asks for ------------------------------------------

    async def handle(self, msg):
        """One message from the page. Anything unknown is ignored, as on the wire."""
        kind = msg.get("type")
        handler = getattr(self, "do_" + kind, None) if isinstance(kind, str) else None
        if handler is None:
            return
        try:
            await handler(msg)
        except (store.StoreError, kinds_module.CommandError) as err:
            self.error(str(err))
        except OSError as err:
            self.error("%s: %s" % (kind, err))
        except (KeyError, TypeError, ValueError) as err:
            self.error("%s: malformed message (%s)" % (kind, err))

    def need_run(self):
        if self.run is None:
            raise store.StoreError("nothing is loaded: load geodata and a nodeset, "
                                   "or a snapshot")
        return self.run

    def edited(self, what, **fields):
        """After a nodeset edit: the run's nodeset.yaml written, the edit
        logged with T, and the page told."""
        run = self.need_run()
        nodeset_module.write(run.nodeset_path, self.nodeset.data)
        run.log_edit(self.ether.now(), what, **fields)
        self.broadcast(self.nodeset_message())

    @staticmethod
    def node_fields(msg, without=()):
        return {key: msg[key] for key in NODE_FIELDS
                if key in msg and msg[key] is not None and key not in without}

    async def resolve_device(self, ref):
        """A device a node newly names, resolved and made a kind, in place."""
        if ref in self.kinds:
            return
        builds = kinds_module.resolve_builds({"new": {"device": ref}}, self.args.build)
        self.builds.update(builds)
        self.kinds.update(kinds_module.make_kinds(builds))
        self.run.set(builds=self.builds)

    async def do_nodeset_add(self, msg):
        self.need_run()
        name = msg["name"]
        fields = self.node_fields(msg, without=("lat", "lon"))
        if fields.get("device"):
            await self.resolve_device(fields["device"])
        node = self.nodeset.add_node(name, float(msg["lat"]), float(msg["lon"]), **fields)
        if kinds_module.device_of(node) not in self.kinds:
            await self.resolve_device(kinds_module.device_of(node))
        self.edited("add", node=name, **{k: node[k] for k in ("id", "lat", "lon", "height_m")})
        self.apply_to_ether()
        self.need_rows([name])
        await self.start_station(name)
        log("node %s added as station %d" % (name, node["id"]))

    async def do_nodeset_move(self, msg):
        """A node moved. A drag sends these at a few Hz with `settle`
        false; the file, the log and the recompute wait for the one that
        settles, and the map shows the node stale meanwhile."""
        self.need_run()
        name = msg["name"]
        self.nodeset.move_node(name, float(msg["lat"]), float(msg["lon"]))
        if msg.get("height_m") is not None:
            self.nodeset.set_node(name, height_m=float(msg["height_m"]))
        if not msg.get("settle", True):
            self.stale.add(name)
            self.broadcast(self.node_message(name))
            return
        node = self.nodeset.node(name)
        nodeset_module.write(self.run.nodeset_path, self.nodeset.data)
        self.run.log_move(self.ether.now(), name, node["lat"], node["lon"], node["height_m"])
        self.broadcast(self.nodeset_message())
        self.need_rows([name])

    async def do_nodeset_remove(self, msg):
        self.need_run()
        name = msg["name"]
        self.nodeset.node(name)
        await self.stop_station(name)
        self.nodeset.remove_node(name)
        self.stale.discard(name)
        self.pending_rows.discard(name)
        self.edited("remove", node=name)
        self.apply_to_ether()
        self.broadcast({"type": "node_gone", "name": name})

    async def do_nodeset_set(self, msg):
        """Change a node's facts. A new id is a new address, and a new device
        may be a new kind: either restarts the station. One with state keeps
        it, and whatever it set up with its old id or address is then stale,
        which the page is told. A new role or radio on a running station is
        said to it at once, in its kind's lines."""
        self.need_run()
        name = msg["name"]
        before = dict(self.nodeset.node(name))
        fields = self.node_fields(msg)
        if fields.get("device"):
            await self.resolve_device(fields["device"])
        old_kind = self.kind_of(name)
        changed_id = self.nodeset.set_node(name, **fields)
        node = self.nodeset.node(name)
        self.edited("set", node=name, **fields)
        if any(node[k] != before[k] for k in ("lat", "lon", "height_m")):
            self.need_rows([name])
        new_kind = self.kind_of(name)
        station = self.stations.get(name)
        if (changed_id or new_kind is not old_kind) and station is not None:
            had_state = station.configured
            await self.stop_station(name)
            self.apply_to_ether()
            await self.start_station(name)
            why = ("is now id %d at %s" % (node["id"], stations_module.bind_addr(node["id"]))
                   if changed_id else "now runs %s" % (new_kind.label if new_kind else "nothing"))
            self.notice("%s %s; its station was restarted%s" % (
                name, why, ", keeping its state: anything it set up with its old id or "
                "address is stale until it is factory reset" if had_state else ""))
            return
        self.apply_to_ether()
        self.broadcast(self.node_message(name))
        said = []
        if node.get("radio") != before.get("radio") and node.get("radio"):
            said += station.kind.lines("radio", **node["radio"]) if station else []
        if node.get("role") != before.get("role") and node.get("role"):
            said += station.kind.lines("role", role=node["role"]) if station else []
        if said and station is not None and station.status == stations_module.UP:
            try:
                await station.kind.setup(station, self.expanded(name, said))
            except kinds_module.CommandError as err:
                self.error("telling %s its new settings: %s" % (name, err))
            await self.read_role(station)

    async def do_nodeset_offset(self, msg):
        self.need_run()
        a, b = msg["between"][:2]
        db = float(msg.get("db") or 0)
        self.nodeset.set_offset(a, b, db, msg.get("note"))
        self.edited("offset", between=[a, b], db=db)
        self.apply_to_ether()

    async def do_node_reset(self, msg):
        """Press reset: the process goes, the supervisor brings it back.

        Its state store is untouched, so it comes back as the station it was —
        which is what a reset button does and the whole of why this is not
        called anything else.
        """
        station = self.stations.get(msg["name"])
        if station is None:
            await self.start_station(msg["name"])
        else:
            await self.flush_station(station)
            await station.restart()

    async def do_node_factory_reset(self, msg):
        """Throw away a station's state and start it again, set up afresh.

        Stopped rather than reset, because the state has to go while nothing is
        holding it; on the way back up the directory is empty, which is exactly
        the station a node placed on the map for the first time is, so setup
        runs again by the ordinary path and not by a special case.
        """
        run = self.need_run()
        name = msg["name"]
        self.nodeset.node(name)
        await self.stop_station(name, flush=False)
        runs_module.wipe_state(run.dir, name)
        await self.start_station(name)
        log("factory reset %s" % name)

    async def do_factory_reset_all(self, msg):
        run = self.need_run()
        await self.stop_all(flush=False)
        runs_module.wipe_state(run.dir)
        self.begin_start_all()
        log("factory reset the whole testbed")

    async def do_reset_all(self, msg):
        """Press reset on every station, spread over the start window.

        Pressing them all at once is the same collision storm `start_all`
        spreads out, and for the same reason: every station comes up and
        announces itself into the same air, so the mesh spends its first
        minutes talking over itself and half the network learns nothing. The
        stagger is what a network of real boards has for free.
        """
        await self.flush_all()
        self.cancel_start()
        self.starter = asyncio.ensure_future(self.restart_all())

    async def restart_all(self):
        stations = list(self.stations.values())
        if not stations:
            return
        gap = self.args.stagger / len(stations)
        for index, station in enumerate(stations):
            if index:
                await self.sleep(gap)
            if self.stopping:
                return
            await station.restart()

    def chosen(self, msg):
        """The stations a `command` or `meta` goes to: up or in setup, among
        `name`, `names`, those carrying `tag`, or all; of `kind` when given.
        A list of (name, station) in id order."""
        wanted = None
        if msg.get("name"):
            wanted = {msg["name"]}
        elif msg.get("names") is not None:
            wanted = set(msg["names"])
        tag = msg.get("tag")
        out = []
        for name, node in sorted(self.nodeset.nodes.items(), key=lambda kv: kv[1]["id"]):
            station = self.stations.get(name)
            if station is None or station.status not in (stations_module.UP,
                                                         stations_module.SETUP):
                continue
            if wanted is not None and name not in wanted:
                continue
            if tag and tag not in node["tags"]:
                continue
            if msg.get("kind") and station.kind.name != msg["kind"]:
                continue
            out.append((name, station))
        return out

    async def spread(self, msg, targets, one):
        """Run `one(name, station)` on every target, spread over `stagger`
        seconds of the run's clock; {name: reply}, a failure as `! why`.

        A command that puts something on the air — an announce above all —
        fired at two dozen stations in the same instant is a collision storm
        rather than a measurement, and the answers are about the storm. Zero
        keeps them simultaneous, which is what a question nobody transmits to
        answer wants.
        """
        spread = max(0.0, float(msg.get("stagger") or 0))
        gap = spread / len(targets) if spread and targets else 0.0

        async def run(index, name, station):
            if gap:
                await self.sleep(index * gap)
            try:
                return name, (await one(name, station)).rstrip("\n")
            except (kinds_module.CommandError, store.StoreError, KeyError) as err:
                return name, "! %s" % err

        return dict(await asyncio.gather(
            *(run(i, n, s) for i, (n, s) in enumerate(targets))))

    def later(self, msg):
        """`after`: seconds on the run's clock before the message is acted on,
        so a script can put it at an instant of a virtual-time run. The wait
        is a task of its own: the page's next message is not held behind it.
        True when the message has been put off."""
        after = max(0.0, float(msg.get("after") or 0))
        if not after:
            return False

        async def go():
            await self.sleep(after)
            await self.handle(dict(msg, after=0))
        asyncio.ensure_future(go())
        return True

    def answered(self, msg, results, **what):
        self.broadcast({"type": "command_result", "id": msg.get("id"), "name": msg.get("name"),
                        "results": results, "t": self.ether.now(), **what})

    async def do_command(self, msg):
        """Run one line on the stations chosen and report what each said.

        The macros are expanded per station, so `lxmf create {name}` or
        `hostname {name}` does the right thing across the whole testbed in one
        go. A line is in one kind's language, so it goes only to stations of
        one kind: the chosen ones must be of one, or `kind` must say which.
        """
        self.need_run()
        line = (msg.get("line") or "").strip()
        if not line or self.later(msg):
            return
        targets = self.chosen(msg)
        types = sorted({station.kind.name for _, station in targets})
        if len(types) > 1:
            raise kinds_module.CommandError(
                "%r would go to %s stations, and a line is one kind's language: say which "
                "kind" % (line, " and ".join(types)))

        async def one(name, station):
            return await station.kind.run(station, kinds_module.expand(
                line, name, self.nodeset.nodes[name]["id"], self.ids()))

        results = await self.spread(msg, targets, one)
        self.answered(msg, results, line=line)
        log("ran %r on %d %s station(s)%s" % (line, len(results), types[0] if types else "",
            " over %.0fs" % float(msg.get("stagger") or 0) if msg.get("stagger") else ""))

    async def address_of(self, name):
        """A node's LXMF delivery address, asked of its station."""
        station = self.stations.get(name)
        if station is None or station.status != stations_module.UP:
            raise kinds_module.CommandError("%s is not up" % name)
        found = await station.kind.address(station)
        if not found:
            raise kinds_module.CommandError("%s has no address yet" % name)
        return found

    async def do_meta(self, msg):
        """An intent on the stations chosen, each in its own kind's lines.

        `message` and `path` name the other end by node (`to`), whose address
        is asked of it once; `peer_tcp` names it too, for its address in this
        run's network. `address` answers each station's own address.
        """
        self.need_run()
        verb = msg.get("verb")
        if not verb or self.later(msg):
            return
        args = dict(msg.get("args") or {})
        to = args.pop("to", None)
        if verb in ("message", "path"):
            if not to:
                raise kinds_module.CommandError("%s needs `to`, a node's name" % verb)
            args["dest"] = await self.address_of(to)
        elif verb == "peer_tcp":
            args["addr"] = stations_module.bind_addr(self.nodeset.node(to)["id"])
        targets = self.chosen(msg)

        async def one(name, station):
            if verb == ADDRESS:
                return await station.kind.address(station) or ""
            replies = []
            for line in self.expanded(name, station.kind.lines(verb, **args)):
                replies.append((await station.kind.run(station, line)).rstrip("\n"))
            return "\n".join(replies)

        results = await self.spread(msg, targets, one)
        self.answered(msg, results, verb=verb)
        log("%s on %d station(s)" % (verb, len(results)))

    async def do_plan(self, msg):
        """A driver says what the run is for: its phases and the T each ends at.

        simd knows T and the pace but not what the run means to do; the plan
        is what turns those into "traffic, 23 of 60 minutes" and a finish
        time. It goes out in every `clock`, and a driver sends it again when
        its recipe changes. No phases clears it.
        """
        phases = []
        for phase in msg.get("phases") or ():
            phases.append({"name": str(phase["name"]), "until": int(phase["until"])})
        self.plan = {"t": self.ether.now(), "phases": phases} if phases else None
        self.broadcast(self.clock_message())

    async def do_start_all(self, msg):
        self.begin_start_all()

    async def do_stop_all(self, msg):
        await self.stop_all()
        for name in (self.nodeset.nodes if self.nodeset else ()):
            self.broadcast(self.node_message(name))

    async def do_levels(self, msg):
        """What the others would hear from one station, for the hover card.

        At the carrier asked about, else the one the station last stated,
        else its declared one, else its table's centre: the table is at the
        band's centre and the ether corrects it to the carrier, so the answer
        is for the frequency the station is really on.
        """
        self.need_run()
        name = msg["name"]
        node = self.nodeset.node(name)
        radio = self.radio_of(node["id"])
        declared = node.get("radio") or {}
        freq = msg.get("freq") or radio.get("freq") or (
            declared["freq_mhz"] * 1e6 if declared.get("freq_mhz") else None)
        if not freq and self.tables:
            freq = next(iter(self.tables.values())).f0_hz
        if not freq:
            return
        bw = radio.get("bw") or (declared["bw_khz"] * 1e3 if declared.get("bw_khz") else 125_000)
        heard = self.ether.levels(node["id"], float(freq), bw_hz=bw)
        self.broadcast({"type": "levels", "name": name, "freq": freq,
                        "heard": {self.name_of(sid): round(level, 1)
                                  for sid, level in heard.items()
                                  if self.name_of(sid) is not None}})

    # ---- loading ---------------------------------------------------------

    async def adopt(self, run, sidecar=None):
        """Make this the loaded run: stop what was running, start what is.

        The geodata, the nodeset, the script and the tables are all the run's
        own copies. The builds are the ones the run was made with, or resolved
        now for a device the run names none for.
        """
        gd, ns = run.geodata(), run.nodeset()
        builds = dict(run.meta.get("builds") or {})
        wanted = {kinds_module.device_of(node) for node in ns.nodes.values()}
        if not all((builds.get(ref) or {}).get("elf") for ref in wanted):
            builds.update(kinds_module.resolve_builds(
                {ref: {"device": ref} for ref in wanted if ref not in builds}, self.args.build))
            run.set(builds=builds)
        kinds = kinds_module.make_kinds(builds)
        setup_fn = None
        if run.script_path:
            module = script_module.load(run.script_path, run.meta.get("script"))
            setup_fn = getattr(module, script_module.SETUP, None)
        tables = {band: slt.Table.read(run.table_path(band)) for band in run.bands()}
        await self.stop_all()
        self.stale, self.pending_rows = set(), set()
        if self.rows_task is not None:
            self.rows_task.cancel()
            self.rows_task = None
        self.run, self.geodata, self.nodeset = run, gd, ns
        self.kinds, self.builds, self.tables, self.setup_fn = kinds, builds, tables, setup_fn
        self.sidecar = sidecar or self.args.sidecar
        self.plan = None                    # a plan is for the run it was sent in
        await self.start_ether(run.dir)
        self.apply_to_ether()
        log("run %s: geodata %s (%s), nodeset %s (%d nodes), script %s, tables %s"
            % (run.dir, gd.name, gd.kind, ns.name, len(ns.nodes),
               run.meta.get("script") or "none", ", ".join(sorted(tables)) or "none"))
        for kind in kinds.values():
            log("device %s" % kind.describe())
        self.broadcast(self.snapshot())     # a new run is a fresh page
        self.begin_start_all()

    async def do_sim_load(self, msg):
        """A new run from geodata, a nodeset and optionally a script, factory
        fresh; or a run directory the front has made already.

        The tables come from the cache, computed into it on a miss: here on
        synthetic ground, through the sidecar for a pack. The front computes
        them before it passes this on, so behind the front this is a cache hit.
        """
        sidecar = msg.get("sidecar") or self.sidecar
        if msg.get("run"):
            await self.adopt(runs_module.open_run(msg["run"]), sidecar)
            return
        gd = geodata_module.load(msg["geodata"])
        ns = nodeset_module.load(msg["nodeset"])
        script_name = msg.get("script") or None
        if script_name:
            script_module.read(script_name)
        builds = kinds_module.resolve_builds(ns.nodes, msg.get("build") or self.args.build)
        tables = {}
        for band in losses_module.bands_of(ns, gd):
            tables[band], _ = await losses_module.compute(gd, ns, band, sidecar,
                                                          self.progress(band), notice=log)
        run = runs_module.create_run(free_run_dir(self.run_base), gd, ns, script_name,
                                     self.args.time, tables, builds)
        await self.adopt(run, sidecar)
        log("loaded geodata %s, nodeset %s, script %s"
            % (gd.name, ns.name, script_name or "none"))

    async def do_snapshot_load(self, msg):
        """A snapshot is a moment, so loading one brings its state back too:
        its nodeset, script and tables as they were, nothing recomputed."""
        run = runs_module.load_snapshot(msg["name"], free_run_dir(self.run_base),
                                        self.args.time)
        builds = kinds_module.resolve_builds(run.nodeset().nodes,
                                             msg.get("build") or self.args.build)
        run.set(snapshot_builds=run.meta.get("builds") or {}, builds=builds)
        await self.adopt(run, msg.get("sidecar") or self.sidecar)
        log("loaded snapshot %s" % msg["name"])

    async def do_snapshot_save_as(self, msg):
        run = self.need_run()
        await self.flush_all()   # the stations keep running; the copy is of now
        runs_module.save_snapshot(run, msg["name"], self.ether.now())
        self.broadcast({"type": "store", **store_lists()})
        log("saved snapshot %s" % msg["name"])

    # ---- the HTTP side ---------------------------------------------------

    async def ws_page(self, request):
        """One control websocket. `?quiet=1` leaves out what only a map draws
        (LOUD), which is most of the traffic on a busy network and nothing a
        driver or the front's registry reads."""
        socket = web.WebSocketResponse(heartbeat=30, max_msg_size=0)
        await socket.prepare(request)
        self.pages[socket] = request.query.get("quiet", "") not in ("", "0")
        await socket.send_str(json.dumps(self.snapshot()))
        try:
            async for message in socket:
                if message.type is WSMsgType.TEXT:
                    try:
                        msg = json.loads(message.data)
                    except ValueError:
                        continue
                    if isinstance(msg, dict):
                        await self.handle(msg)
        finally:
            self.pages.pop(socket, None)
        return socket

    async def ws_console(self, request):
        """The station's pty over a websocket.

        A **binary** frame is keystrokes and goes to the pty as it stands; a
        **text** frame is a control message, of which there is one — `resize`,
        carrying the terminal's size. Splitting them by frame type rather than
        by an escape in the stream means no byte a person can type is special.
        """
        name = request.match_info["name"]
        socket = web.WebSocketResponse(heartbeat=30)
        await socket.prepare(request)
        self.consoles.setdefault(name, set()).add(socket)
        self.watched(name)
        try:
            async for message in socket:
                station = self.stations.get(name)
                if station is None:
                    continue
                if message.type is WSMsgType.BINARY:
                    station.write(message.data)
                elif message.type is WSMsgType.TEXT:
                    try:
                        control = json.loads(message.data)
                    except ValueError:
                        continue
                    if control.get("type") == "resize":
                        station.resize(int(control.get("cols", 80)),
                                       int(control.get("rows", 24)))
        finally:
            self.consoles.get(name, set()).discard(socket)
            self.watched(name)
        return socket

    async def ws_signalling(self, request):
        """Stand in the middle of a station's WebRTC signalling, so the SDP
        answer can be pointed at the relay (webrtc.bridge)."""
        label = proxy.label_of(request.headers.get("Host", "").encode("latin-1"))
        name = self.node_for_label(label)
        if name is None:
            return web.Response(status=404, text="no such station\n")
        kind = self.kind_of(name)
        if kind is None or kind.web_port() is None:
            return web.Response(status=404, text="no web UI on this station\n")
        addr = stations_module.bind_addr(self.nodeset.nodes[name]["id"])
        return await webrtc_module.bridge(
            request, "http://%s:%d%s" % (addr, kind.web_port(), SIGNAL_PATH),
            self.relay, addr, WEBRTC_PORT, label)

    async def api_store(self, request):
        return web.json_response({**store_lists(),
                                  "run": self.run.as_dict() if self.run else None})

    def app(self):
        app = web.Application()
        app.router.add_get(SIGNAL_PATH, self.ws_signalling)
        app.router.add_get("/ws", self.ws_page)
        app.router.add_get("/ws/console/{name}", self.ws_console)
        app.router.add_get("/api/store", self.api_store)
        app.router.add_get("/{tail:.*}", serve_page)
        return app

    async def start_http(self):
        """The control app on a loopback port, and the front listener on the bind.

        The app is never bound to the outside: the front listener is what the
        world reaches, and it dials the app for anything that is not a station.
        One port, and `Host` decides.
        """
        self.runner = web.AppRunner(self.app(), access_log=None)
        await self.runner.setup()
        site = web.TCPSite(self.runner, "127.0.0.1", 0)
        await site.start()
        self.control_port = self.runner.addresses[0][1]

        host, _, port = self.args.bind.rpartition(":")
        self.front = await proxy.serve(host or "0.0.0.0", int(port),
                                       self.resolve_host)
        webrtc_module.log = log
        self.relay = await webrtc_module.serve(
            host or "0.0.0.0", self.args.relay_bind_port,
            self.args.relay_host, self.args.relay_port)
        log("webrtc relay on udp/%d, offered to browsers as %s:%d"
            % (self.args.relay_bind_port, self.args.relay_host,
               self.args.relay_port))
        log("control page on http://localhost:%s/" % self.args.public_port)
        log("stations at http://<name>.sim.localhost:%s/" % self.args.public_port)

    # ---- the run ---------------------------------------------------------

    async def run_forever(self):
        loop = asyncio.get_running_loop()
        done = loop.create_future()
        for sig in (signal.SIGINT, signal.SIGTERM):
            with contextlib.suppress(NotImplementedError):
                loop.add_signal_handler(
                    sig, lambda: done.done() or done.set_result(None))

        await self.start_ether()
        if os.path.isfile(os.path.join(self.args.run, runs_module.RUN_FILE)):
            try:
                await self.adopt(runs_module.open_run(self.args.run))
            except (store.StoreError, kinds_module.CommandError, OSError) as err:
                log("error: cannot load the run in %s: %s" % (self.args.run, err))
        await self.start_http()
        self.poller = asyncio.ensure_future(self.poll_roles())
        self.clock_watch = asyncio.ensure_future(self.watch_clock())
        try:
            await done
        finally:
            await self.shutdown()

    async def shutdown(self):
        """Stop everything, in the order that lets each step finish.

        The aiohttp side goes first because it owns the websockets, and a
        proxied websocket is a front-listener handler that will not return
        until its connection does — closing the front first would wait on a
        page that is still open, for as long as it stayed open. Every wait
        here is bounded for the same reason: a testbed told to stop stops, and
        the sockets go with the process regardless.
        """
        self.stopping = True
        log("stopping")
        for task in (self.poller, self.clock_watch, self.rows_task):
            if task is not None:
                task.cancel()
        await self.stop_all()
        if self.runner is not None:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.runner.cleanup(), SHUTDOWN_TIMEOUT_S)
        if self.front is not None:
            self.front.close()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.front.wait_closed(), SHUTDOWN_TIMEOUT_S)
        if self.relay is not None:
            self.relay.close()
        if self.ether is not None:
            self.ether.close()


def parse_args(argv):
    ap = argparse.ArgumentParser(
        description="the simulated testbed: ether, stations, proxy and control page")
    ap.add_argument("--bind", default="0.0.0.0:9011",
                    help="host:port for the page and the stations (default 0.0.0.0:9011)")
    ap.add_argument("--ether", default="127.0.0.1:7000",
                    help="host:port for the ether's UDP endpoint")
    ap.add_argument("--run", default=DEFAULT_RUN,
                    help="the run directory: loaded at start when it holds a run "
                         "(run.yaml), and where a run loaded later goes, or beside "
                         "it as <dir>-2, -3… (default %s)" % os.path.relpath(DEFAULT_RUN))
    ap.add_argument("--build",
                    help="a device (catalogue, package, stamp, local device or path) "
                         "every node whose device is of its kind runs instead")
    ap.add_argument("--sidecar",
                    help="the planner-web base URL a pack's rows are recomputed "
                         "through after a move")
    ap.add_argument("--noise-figure", type=float, default=ether_module.DEFAULT_NOISE_FIGURE_DB,
                    help="the receivers' noise figure in dB (default %g)"
                         % ether_module.DEFAULT_NOISE_FIGURE_DB)
    ap.add_argument("--pairwise", action="store_true",
                    help="rule on collisions pairwise, per interferer by the capture "
                         "margin, instead of on the summed interference")
    ap.add_argument("--seed", type=int,
                    help="the ether's seed, which its welcome hands every station "
                         "(default: drawn at random)")
    ap.add_argument("--epoch", type=int, metavar="US",
                    help="in a virtual-time run, the wall-clock microseconds T 0 "
                         "stands for (default: the wall clock when the ether "
                         "starts); two runs given the same one give their "
                         "stations the same wall clock")
    ap.add_argument("--relay-port", type=int, default=0,
                    help="the UDP port a browser sends the DataChannel to, as "
                         "the browser sees it (default: the bind port)")
    ap.add_argument("--stagger", type=float, default=60.0,
                    help="seconds to spread a whole nodeset's start over, so the "
                         "stations do not boot and announce in lockstep "
                         "(default 60)")
    ap.add_argument("--relay-host", default="127.0.0.1",
                    help="the address a browser sends the DataChannel to "
                         "(default 127.0.0.1)")
    ap.add_argument("--time", default="real",
                    help="real (default): the stations and the medium run on the "
                         "wall clock; max: virtual time, as fast as the stations "
                         "allow; <k>x: virtual time paced at k times the wall")
    ap.add_argument("--net", default=stations_module.NET,
                    help="the network the stations' addresses come from, one "
                         "/24 at a time with hosts 5 to 254 (default %s: 1000 "
                         "stations); a second testbed on the same host needs "
                         "its own, 127.0.4.0/22 say" % stations_module.NET)
    args = ap.parse_args(argv)
    try:
        stations_module.set_net(args.net)
        ether_module.parse_time_mode(args.time)
    except ValueError as err:
        ap.error(str(err))
    args.run = os.path.abspath(args.run)
    args.public_port = args.bind.rpartition(":")[2]
    # The relay binds the same number as the page, on UDP — one number to
    # publish and one to remember. What the browser is told may differ, since
    # the container's mapping is the host's business, not simd's.
    args.relay_bind_port = int(args.public_port)
    if not args.relay_port:
        args.relay_port = args.relay_bind_port
    return args


def main(argv=None):
    args = parse_args(argv)
    try:
        asyncio.run(Simd(args).run_forever())
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
