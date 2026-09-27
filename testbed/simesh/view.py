"""A run directory as the analysis tools see it.

Everything a tool needs beyond the record comes from the run, never from
the nodeset or geodata files as they stand now: the run's own nodeset copy
(with the edits made during the run), its geodata copy, the builds it
resolved, and the loss tables the ether read.

- **Names.** The record knows stations by id; the nodeset maps id to node
  name (`names`) and back (`ids`).
- **Positions** are the geodata's metres (`geodata.to_xy`): synthetic
  ground's nautical-mile metres, or a pack's easting and northing. A pack's
  copy points at its pack, which has to be where it points.
- **Radio, role.** A node's radio and role are its declared ones, each radio
  figure falling back to the defaults in `simesh` where the node declares
  none, and a node declaring no role a `client`. What a script's setup
  changed beyond them is not seen here.
- **Kind.** A node's kind is its device's, as the run resolved it; what its
  frames mean belongs to that kind's protocol (`simesh.protocol_for`).
- **Levels** are the medium's own: an `Ether` holding the run's tables with
  the nodeset's offsets, names and antenna gains, and the noise figure from
  the run's `physics` (the ether's default when the run names none), asked
  through the ether's own `level` and `audible`. Nothing here recomputes a
  loss from positions.
"""

import collections
import math
import os
import sys

import kinds as kinds_module
import losses as losses_module
import runs
import store

sys.path.insert(0, os.path.join(store.SIM_DIR, "..", "ether"))
import ether as ether_module  # noqa: E402 - the path is set just above
import slt  # noqa: E402

import simesh  # noqa: E402
from simesh import record as record_module  # noqa: E402


class RunView:
    """One run, opened for analysis."""

    def __init__(self, directory):
        self.run = runs.open_run(directory)
        self.dir = self.run.dir
        self.nodeset = self.run.nodeset()
        self.nodes = self.nodeset.nodes
        self.builds = self.run.meta.get("builds") or {}
        self.names = {int(n["id"]): name for name, n in self.nodes.items()}
        self.ids = {name: int(n["id"]) for name, n in self.nodes.items()}
        self._geodata = None
        self._positions = None
        self._medium = None

    @property
    def record_path(self):
        return record_module.path_in(self.dir)

    def geodata(self):
        if self._geodata is None:
            self._geodata = self.run.geodata()
        return self._geodata

    # ---- geometry --------------------------------------------------------

    def positions(self):
        """Station id -> (x, y) in the geodata's metres."""
        if self._positions is None:
            gd = self.geodata()
            self._positions = {int(n["id"]): gd.to_xy(n["lat"], n["lon"])
                               for n in self.nodes.values()}
        return self._positions

    def distance(self, a, b):
        """Metres between two stations, by id, in the geodata's plane."""
        pos = self.positions()
        return math.hypot(pos[a][0] - pos[b][0], pos[a][1] - pos[b][1])

    # ---- what each node is ----------------------------------------------

    def kind_type(self, name):
        ref = kinds_module.device_of(self.nodes[name])
        return (self.builds.get(ref) or {}).get("kind_type")

    def protocol(self, name):
        return simesh.protocol_for(self.kind_type(name))

    def protocols(self):
        """The protocol modules any node of the run is read by."""
        found = []
        for name in self.nodes:
            module = self.protocol(name)
            if module is not None and module not in found:
                found.append(module)
        return found

    def radio(self, name):
        """Slot 0 of a node as it declares it: freq_hz, sf, bw_hz, power_dbm."""
        radio = self.nodes[name].get("radio") or {}
        said = {"freq_hz": int(round(radio["freq_mhz"] * 1e6)) if "freq_mhz" in radio else None,
                "sf": radio.get("sf"),
                "bw_hz": int(round(radio["bw_khz"] * 1e3)) if "bw_khz" in radio else None,
                "power_dbm": radio.get("tx_dbm")}
        defaults = {"freq_hz": simesh.DEFAULT_FREQ_HZ, "sf": simesh.DEFAULT_SF,
                    "bw_hz": simesh.DEFAULT_BW_HZ, "power_dbm": simesh.DEFAULT_POWER_DBM}
        return {k: (said[k] if said[k] is not None else v) for k, v in defaults.items()}

    def roles(self):
        """Station id -> role, for every node: its declared one, else client."""
        return {sid: self.nodes[name].get("role") or "client" for name, sid in self.ids.items()}

    def forwarders(self):
        """The station ids whose role forwards for others."""
        return {sid for sid, role in self.roles().items() if role in simesh.FORWARDING}

    def calling_hz(self):
        """The carrier most nodes are set to: the calling channel."""
        count = collections.Counter(self.radio(name)["freq_hz"] for name in self.nodes)
        return count.most_common(1)[0][0] if count else simesh.DEFAULT_FREQ_HZ

    # ---- the medium ------------------------------------------------------

    def medium(self):
        """An `Ether` with the run's tables and offsets, names, gains and
        noise figure, for its `level` and `audible`; it carries no traffic."""
        if self._medium is None:
            e = ether_module.Ether.__new__(ether_module.Ether)
            e.physics = ether_module.Physics.from_dict(self.run.meta.get("physics"))
            e.stations = {}
            tables = {band: slt.Table.read(self.run.table_path(band))
                      for band in self.run.bands()}
            gains = {int(n["id"]): float((n.get("antenna") or {}).get("gain_dbi", 0.0))
                     for n in self.nodes.values()}
            e.set_losses(losses_module.with_offsets(tables, self.nodeset), self.ids, gains)
            self._medium = e
        return self._medium

    def has_table(self, freq_hz):
        return slt.band_for(freq_hz) in self.run.bands()

    def audible(self, a, b, freq_hz=None):
        """True when station b decodes station a on this carrier (a's own by
        default) at a's declared power, SF and bandwidth."""
        r = self.radio(self.names[a])
        e = self.medium()
        level = e.level(a, b, freq_hz or r["freq_hz"], r["power_dbm"])
        return e.audible(level, r["bw_hz"], r["sf"])

    def radio_graph(self, freq_hz=None):
        """Station id -> the ids that decode it, per `audible`."""
        adj = collections.defaultdict(set)
        for a in self.names:
            for b in self.names:
                if a != b and self.audible(a, b, freq_hz):
                    adj[a].add(b)
        return adj
