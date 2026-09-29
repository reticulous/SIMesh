#!/usr/bin/env python3
"""Loss tables: every ordered pair's path loss, for one nodeset on one geodata.

    python3 losses.py --geodata G --nodeset N --band 868 [--sidecar URL] [--out PATH]
    python3 losses.py --geodata G --nodeset N --band 868 --table PATH --node NAME ...

A table is derived, never edited: it follows from the geodata and the
nodeset's geometry, and is cached as

    testbed/losses/<geodata>/<nodeset geometry hash>/<band>.bin

in the SLT1 format (`ether/slt.py`). A run works on its own copy; a node
moved during a run has its row and column recomputed into that copy, never
into the cache.

A nodeset's **antennas** and **offsets** are not in the table: they are
layers over it, put on when the medium is given the tables
(`medium_tables`), so the cached table is the model's own and neither a new
antenna nor an offset forces a recompute. The antenna layer takes each
pair's gains off its loss, each end's pattern toward the other in three
dimensions, which needs the ground under each node (`grounds`: 0 on
synthetic ground, the pack's terrain through the sidecar).

Synthetic ground is computed here:

    loss = FSPL(1 m, f0) + 10·n·log10(max(d, 1 m))

with d measured on the ground's plane, heights playing no part on flat
terrain. The table is symmetric and its model is "log-distance".

Packs are computed through a running planner sidecar's `/link.json`,
one request per ordered pair: the answer is not reciprocal, because the
terminal clutter and heights at the two ends enter P.1812 differently (2.5 dB
apart on a 300 m Mitte path with antennas at 38 m and 30 m), so each
direction's cell is what `link.json` says of that direction. Each
request gives both ends' antenna heights from the nodeset, in the pack's own
CRS, and takes `lb_db`, the loss with the terminal clutter at both ends.
Gains are not sent: they are the antenna layer's to add. What the sidecar cannot
answer is decided here, without asking:

- an end outside the pack's extent: never heard, FLAG_OFF_PACK (the sidecar
  would clamp the point onto the pack's edge and answer for a place the
  node is not);
- farther apart than the compute radius (30 km unless told otherwise):
  never heard, FLAG_BEYOND_RADIUS. A pair too weak to carry a frame is still
  computed, because it still adds to a receiver's interference; the radius
  is only a bound on the work;
- under 20 m apart, which the sidecar refuses: free space at that distance,
  FLAG_NEAR_FIELD.

From a reply, FLAG_NEAR_FIELD when its model is the near-field one rather
than P.1812, and FLAG_LOS_CLEAR when its Fresnel verdict is "clear". A
refusal for a path that leaves the pack or exceeds the sidecar's window cap
is never heard with the matching flag; "path too short for a §3.2 profile",
which the sidecar says of pairs a few tens of metres apart whose profile
decimates to fewer than three points, is free space at that distance,
FLAG_NEAR_FIELD, as under 20 m; "renderer busy" (429) is retried after a
growing pause. Requests run concurrently up to a bound near the sidecar's
render slots, though the sidecar holds its raster locks for the whole of a
`link.json`, so requests are answered one at a time whatever the bound:
about 2.5 ms a request, two requests a pair.

A sidecar indexes its pack's buildings in the background after it starts
(a second for the berlin-city pack, half a minute for all of Berlin), and
until then `link.json` answers from the clutter raster alone: a different
number, 40 dB off on a 300 m Mitte path,
which a cache would keep. So before its first pair a table asks a probe pair
until the reply's `profile_evidence.buildings_index` is "ready" or "absent",
saying so as a notice while it waits (bounded; past the bound the table is
refused), and a cell answered "loading" all the same is thrown away and
asked again once the index is in. The same holds for a node's row and
column.

The command prints JSON lines: `{"event": "progress", "done", "total"}`,
`{"event": "notice", "message"}` while it waits on the sidecar, then
`{"event": "done", "path", "cached", "n"}` or `{"event": "error", "message"}`.

`link.json` takes no carrier: it always judges at the planner's EU868 link
parameters, 869.525 MHz, 50 % of time and 90 % of locations. So a pack
has an 868 table only, its header's `f0_hz` is 869.525 MHz, the carrier the
numbers are really for, and asking for another band is refused.
"""

import argparse
import asyncio
import datetime
import json
import math
import os
import shutil
import sys
from array import array

import antennas as antennas_module
import geodata as geodata_module
import nodeset as nodeset_module
import store

sys.path.insert(0, os.path.join(store.SIM_DIR, "..", "ether"))
import ether as ether_module  # noqa: E402 - the path is set just above
import slt  # noqa: E402

DEFAULT_RADIUS_M = 30_000.0
MIN_DISTANCE_M = 1.0                # two nodes at one point are still a metre apart
NEAR_LIMIT_M = 20.0                 # the sidecar's floor for a modelled path
P1812_MODEL = "ITU-R P.1812-8"
# What link.json judges at: planner_core LinkParams::eu868_defaults.
LINK_F0_HZ = 869_525_000
LINK_P_TIME_PCT = 50.0
LINK_P_LOC_PCT = 90.0
PACK_BANDS = ("868",)
RETRY_FIRST_S, RETRY_MAX_S = 0.05, 2.0
# The sidecar's building index: a reply given while it is "loading" rests on
# the clutter raster alone and is a different number from the same request
# once it is "ready" ("absent": the pack has none, and never will).
INDEX_LOADING = "loading"
INDEX_WAIT_S = 300.0                # a whole Berlin index is about half a minute
INDEX_POLL_FIRST_S, INDEX_POLL_MAX_S = 0.25, 2.0
PROBE_SPAN_M = 500.0                # the probe pair, east-west across the pack's centre


class LossError(store.StoreError):
    """A table could not be computed, for a reason the page can show."""


def cache_path(geodata_name, geometry_hash, band):
    return os.path.join(store.LOSSES_DIR, store.check_name(geodata_name, "geodata"),
                        geometry_hash, "%s.bin" % band)


def _check_band(band):
    band = str(band)
    if band not in slt.BANDS:
        raise LossError("no loss table for band %s: the bands are %s"
                        % (band, ", ".join(slt.BANDS)))
    return band


def _nodes_header(ns):
    return [{"name": name, "id": node["id"], "lat": node["lat"], "lon": node["lon"],
             "height_m": node["height_m"], "height_from": node["height_from"]}
            for name, node in ns.nodes.items()]


def header_for(gd, ns, band, radius_m=DEFAULT_RADIUS_M, planner_version=None):
    """A new table's header for this geodata, nodeset and band."""
    band = _check_band(band)
    now = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    head = {"geodata": gd.name, "geodata_hash": gd.content_hash,
            "nodeset_geometry": ns.geometry_hash(), "band": band,
            "nodes": _nodes_header(ns), "computed_at": now}
    if gd.is_pack:
        head.update(pack_manifest_hash=gd.pack_manifest_hash, f0_hz=LINK_F0_HZ,
                    model="P.1812-8", p_time_pct=LINK_P_TIME_PCT, p_loc_pct=LINK_P_LOC_PCT,
                    radius_m=radius_m, planner_version=planner_version)
    else:
        head.update(pack_manifest_hash=None, f0_hz=slt.f0_of(band), model="log-distance",
                    exponent=gd.exponent, p_time_pct=None, p_loc_pct=None,
                    planner_version=None)
    return head


def free_space_db(f_hz, d_m):
    return ether_module.fspl_1m_db(f_hz) + 20.0 * math.log10(max(d_m, MIN_DISTANCE_M))


def log_distance_loss(f_hz, exponent, d_m):
    """Log-distance path loss at one carrier."""
    return (ether_module.fspl_1m_db(f_hz)
            + 10.0 * exponent * math.log10(max(d_m, MIN_DISTANCE_M)))


def _xy(gd, ns):
    return {name: gd.to_xy(node["lat"], node["lon"]) for name, node in ns.nodes.items()}


def _pairs_to_do(table, names, only=None):
    """Unordered pairs (a, b) of the table's nodes, each once; with `only`,
    those touching one of `only`."""
    out = []
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            if only is None or a in only or b in only:
                out.append((a, b))
    return out


# ---- offsets -------------------------------------------------------------

def with_antennas(tables, gd, ns, grounds=None):
    """Copies of `tables` (band -> slt.Table) with each pair's antenna gains
    taken off its loss, both ways: each end's gain toward the other, in
    three dimensions (antennas.pair_gain), from the nodes' positions and
    antenna heights over the ground under them. `grounds` is {node: metres
    above sea level of the ground}; a node it lacks stands at 0, which is
    synthetic ground's own. A pair either table does not hold is left out."""
    grounds = grounds or {}
    xy = _xy(gd, ns)
    ends = {name: {"xy": xy[name], "antenna": node["antenna"],
                   "top": float(grounds.get(name) or 0.0) + float(node["height_m"])}
            for name, node in ns.nodes.items()}
    gains = {}
    out = {}
    for band, table in tables.items():
        new = slt.Table(table.header, array("f", table.loss), array("B", table.flags),
                        array("H", table.samples))
        names = [n for n in table.names if n in ends]
        for i, a in enumerate(names):
            for b in names[i + 1:]:
                if (a, b) not in gains:
                    gains[(a, b)] = antennas_module.pair_gain(ends[a], ends[b])
                g = gains[(a, b)]
                new.loss[new.cell(a, b)] -= g
                new.loss[new.cell(b, a)] -= g
        out[band] = new
    return out


def medium_tables(tables, gd, ns, grounds=None):
    """What the medium is given: the model's tables with the antennas and
    the offsets on them."""
    return with_offsets(with_antennas(tables, gd, ns, grounds), ns)


async def grounds(gd, ns, sidecar, names=None, session=None):
    """{node: metres above sea level of the ground under it}: 0 on synthetic
    ground, from the pack's terrain through the sidecar's `tile.bin` on a
    pack, a node off the pack or with no terrain left out."""
    names = list(ns.nodes if names is None else names)
    if not gd.is_pack:
        return {name: 0.0 for name in names}
    if not sidecar:
        return {}
    import aiohttp

    own = session is None
    if own:
        session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=30))
    out = {}
    try:
        for name in names:
            node = ns.nodes.get(name)
            if node is None:
                continue
            x, y = gd.to_xy(node["lat"], node["lon"])
            params = {"minx": "%.3f" % (x - 2), "miny": "%.3f" % (y - 2),
                      "maxx": "%.3f" % (x + 2), "maxy": "%.3f" % (y + 2), "w": "1", "h": "1",
                      "terrain_only": "1"}
            for _ in range(6):
                async with session.get(sidecar + "/tile.bin", params=params) as resp:
                    if resp.status == 429:
                        await asyncio.sleep(0.2)
                        continue
                    if resp.status == 200:
                        value = terrain_of_tile(await resp.read())
                        if value is not None:
                            out[name] = value
                    break
    except (aiohttp.ClientError, asyncio.TimeoutError):
        pass
    finally:
        if own:
            await session.close()
    return out


def terrain_of_tile(data):
    """The first cell's terrain, in metres, of a sidecar `tile.bin` ("PTL2",
    u32 w, u32 h, six f64, u8 flags, then i16 decimetres per cell), or None."""
    import struct

    if len(data) < 47 or data[:4] != b"PTL2":
        return None
    raw = struct.unpack_from("<h", data, 45)[0]
    return None if raw == -32768 else raw / 10.0


def with_offsets(tables, ns):
    """Copies of `tables` (band -> slt.Table) with the nodeset's offsets
    added to both directions of each pair; the tables themselves when it
    has none. A pair either table does not hold is left out."""
    offsets = [o for o in ns.offsets if o.get("db")]
    if not offsets:
        return dict(tables)
    out = {}
    for band, table in tables.items():
        new = slt.Table(table.header, array("f", table.loss), array("B", table.flags),
                        array("H", table.samples))
        for offset in offsets:
            a, b = offset["between"]
            if a in new.index and b in new.index and a != b:
                new.loss[new.cell(a, b)] += offset["db"]
                new.loss[new.cell(b, a)] += offset["db"]
        out[band] = new
    return out


# ---- synthetic ground ----------------------------------------------------

def _fill_synthetic(table, gd, ns, pairs, progress=None):
    xy = _xy(gd, ns)
    f0 = table.f0_hz
    for done, (a, b) in enumerate(pairs, 1):
        d = math.hypot(xy[a][0] - xy[b][0], xy[a][1] - xy[b][1])
        loss = log_distance_loss(f0, gd.exponent, d)
        table.put(a, b, loss)
        table.put(b, a, loss)
        if progress:
            progress(done, len(pairs))


def synthetic_table(gd, ns, band, progress=None):
    """Synthetic ground's table, computed here."""
    if gd.is_pack:
        raise LossError("geodata %s is a pack" % gd.name)
    table = slt.Table(header_for(gd, ns, band))
    _fill_synthetic(table, gd, ns, _pairs_to_do(table, table.names), progress)
    return table


# ---- pack ----------------------------------------------------------------

class Sidecar:
    """The questions a table asks a running planner-web, over one session."""

    def __init__(self, base_url, session, concurrency, notice=None):
        self.base = base_url.rstrip("/")
        self.session = session
        self.gate = asyncio.Semaphore(concurrency)
        self.extent = None
        self.notice = notice
        self.indexed = False
        self.index_lock = asyncio.Lock()

    async def pack(self):
        async with self.session.get(self.base + "/api/pack") as resp:
            if resp.status != 200:
                raise LossError("sidecar %s: /api/pack answered %d" % (self.base, resp.status))
            info = await resp.json(content_type=None)
        ext = info["extent"]
        self.extent = (ext["minx"], ext["miny"], ext["maxx"], ext["maxy"])
        return info

    def inside(self, x, y):
        minx, miny, maxx, maxy = self.extent
        return minx <= x <= maxx and miny <= y <= maxy

    async def link(self, a_xy, b_xy, tx_h, rx_h):
        """One pair's reply, or (status, text) for a refusal other than busy."""
        params = {"ax": "%.3f" % a_xy[0], "ay": "%.3f" % a_xy[1],
                  "bx": "%.3f" % b_xy[0], "by": "%.3f" % b_xy[1],
                  "tx_h": "%g" % tx_h, "rx_h": "%g" % rx_h}
        pause = RETRY_FIRST_S
        while True:
            async with self.gate:
                async with self.session.get(self.base + "/link.json", params=params) as resp:
                    if resp.status == 200:
                        return await resp.json(content_type=None)
                    text = await resp.text()
                    if resp.status != 429:
                        return resp.status, text
            await asyncio.sleep(pause)
            pause = min(pause * 2, RETRY_MAX_S)

    async def settled_link(self, a_xy, b_xy, tx_h, rx_h):
        """`link` for a reply that rests on the building index: one answered
        while the index is still loading is thrown away, and asked again
        once it is not."""
        while True:
            reply = await self.link(a_xy, b_xy, tx_h, rx_h)
            if index_state(reply) != INDEX_LOADING:
                return reply
            self.indexed = False
            await self.wait_indexed((a_xy, b_xy, tx_h, rx_h))

    async def wait_indexed(self, probe=None):
        """Until the sidecar's building index is ready or absent, asked of
        `probe` (a pair that has just come back "loading"), or of a pair
        across the pack's centre; every worker waits on the one probe. A
        sidecar started moments ago is still indexing, and a table begun then
        would hold raster-only numbers for its first pairs. Bounded by
        INDEX_WAIT_S, after which the table is refused."""
        async with self.index_lock:
            if self.indexed:
                return
            if probe is None:
                minx, miny, maxx, maxy = self.extent
                cx, cy = (minx + maxx) / 2.0, (miny + maxy) / 2.0
                probe = ((cx - PROBE_SPAN_M / 2, cy), (cx + PROBE_SPAN_M / 2, cy), 10.0, 10.0)
            loop = asyncio.get_running_loop()
            began = loop.time()
            pause = INDEX_POLL_FIRST_S
            told = False
            while True:
                state = index_state(await self.link(*probe))
                if state != INDEX_LOADING:
                    break
                waited = loop.time() - began
                if waited > INDEX_WAIT_S:
                    raise LossError("the planner sidecar at %s is still indexing the pack's "
                                    "buildings after %.0f s" % (self.base, waited))
                if not told and self.notice:
                    self.notice("the planner sidecar is indexing the pack's buildings: "
                                "waiting for it before asking any pair")
                    told = True
                await asyncio.sleep(pause)
                pause = min(pause * 2, INDEX_POLL_MAX_S)
            if told and self.notice:
                self.notice("the building index is %s after %.0f s"
                            % (state or "unknown", loop.time() - began))
            self.indexed = True


def index_state(reply):
    """What a link.json reply says of the sidecar's building index: "loading",
    "ready", "absent", or None for a refusal, which says nothing of it."""
    if isinstance(reply, tuple):
        return None
    return (reply.get("profile_evidence") or {}).get("buildings_index")


def cell_from_reply(reply, f0_hz, d_m):
    """(loss, flags) for one pair from what link.json said about it."""
    if isinstance(reply, tuple):
        status, text = reply
        if "leaves the pack" in text:
            return slt.NEVER, slt.FLAG_OFF_PACK
        if "window cap" in text:
            return slt.NEVER, slt.FLAG_BEYOND_RADIUS
        if "within 20 m" in text or "too short" in text:
            return free_space_db(f0_hz, d_m), slt.FLAG_NEAR_FIELD
        raise LossError("link.json answered %d: %s" % (status, text.strip()[:200]))
    flags = 0
    model = (reply.get("profile_evidence") or {}).get("model", P1812_MODEL)
    if model != P1812_MODEL:
        flags |= slt.FLAG_NEAR_FIELD
    if (reply.get("fresnel") or {}).get("verdict") == "clear":
        flags |= slt.FLAG_LOS_CLEAR
    return float(reply["lb_db"]), flags


async def _fill_pack(table, gd, ns, pairs, base_url, progress=None,
                     radius_m=DEFAULT_RADIUS_M, concurrency=None, session=None, notice=None):
    import aiohttp

    xy = _xy(gd, ns)
    f0 = table.f0_hz
    concurrency = concurrency or max(4, (os.cpu_count() or 4) - 2)
    own = session is None
    if own:
        session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=120))
    try:
        car = Sidecar(base_url, session, concurrency, notice)
        try:
            await car.pack()
            if pairs:
                if progress:
                    progress(0, len(pairs))
                await car.wait_indexed()
        except aiohttp.ClientError as err:
            raise LossError("no planner sidecar at %s (%s)" % (base_url, err)) from err
        done = 0

        async def one(a, b):
            nonlocal done
            (ax, ay), (bx, by) = xy[a], xy[b]
            d = math.hypot(bx - ax, by - ay)
            if not (car.inside(ax, ay) and car.inside(bx, by)):
                cell = (slt.NEVER, slt.FLAG_OFF_PACK)
            elif d > radius_m:
                cell = (slt.NEVER, slt.FLAG_BEYOND_RADIUS)
            elif d < NEAR_LIMIT_M:
                cell = (free_space_db(f0, d), slt.FLAG_NEAR_FIELD)
            else:
                cell = None
            if cell is not None:
                table.put(a, b, *cell)
                table.put(b, a, *cell)
            else:
                ha, hb = ns.nodes[a]["height_m"], ns.nodes[b]["height_m"]
                there = await car.settled_link((ax, ay), (bx, by), ha, hb)
                back = await car.settled_link((bx, by), (ax, ay), hb, ha)
                table.put(a, b, *cell_from_reply(there, f0, d))
                table.put(b, a, *cell_from_reply(back, f0, d))
            done += 1
            if progress:
                progress(done, len(pairs))

        queue = asyncio.Queue()
        for pair in pairs:
            queue.put_nowait(pair)

        async def worker():
            while True:
                try:
                    a, b = queue.get_nowait()
                except asyncio.QueueEmpty:
                    return
                await one(a, b)

        try:
            await asyncio.gather(*(worker() for _ in range(min(concurrency, len(pairs)) or 1)))
        except aiohttp.ClientError as err:
            raise LossError("planner sidecar at %s: %s" % (base_url, err)) from err
    finally:
        if own:
            await session.close()


def _check_pack(gd, band, base_url):
    if band not in PACK_BANDS:
        raise LossError("geodata %s is a planner pack, and the planner's link.json computes at "
                        "869.525 MHz only: band %s has no table on it" % (gd.name, band))
    if not base_url:
        raise LossError("geodata %s is a planner pack: its losses need a running sidecar"
                        % gd.name)


async def pack_table(gd, ns, band, base_url, progress=None, radius_m=DEFAULT_RADIUS_M,
                     concurrency=None, session=None, planner_version=None, notice=None):
    """A pack's table, through the sidecar at `base_url`. `notice` hears, as
    a sentence, why the table is waiting before its first pair."""
    band = _check_band(band)
    _check_pack(gd, band, base_url)
    table = slt.Table(header_for(gd, ns, band, radius_m, planner_version))
    await _fill_pack(table, gd, ns, _pairs_to_do(table, table.names), base_url, progress,
                     radius_m, concurrency, session, notice)
    return table


# ---- either --------------------------------------------------------------

def bands_of(ns, gd=None, radio=None):
    """The loss-table bands a simulation needs: the one globals.py's carrier
    (`radio`, script.shared_radio's, else the store's) falls in, 868 when
    it falls in none. On a pack, 868 only: the planner judges nothing else.
    `ns` is the nodeset the tables are for."""
    if gd is not None and gd.is_pack:
        return list(PACK_BANDS)
    import script as script_module
    radio = radio or script_module.shared_radio()
    return [slt.band_for(radio["freq_mhz"] * 1e6) or "868"]


async def build(gd, ns, band, base_url=None, progress=None, radius_m=DEFAULT_RADIUS_M,
                concurrency=None, session=None, planner_version=None, notice=None):
    """A fresh table for any geodata, not touching the cache."""
    if gd.is_pack:
        return await pack_table(gd, ns, band, base_url, progress, radius_m,
                                concurrency, session, planner_version, notice)
    return synthetic_table(gd, ns, band, progress)


def cached(gd, ns, band):
    """The cached table's path when there is a usable one, else None. A
    cached table counts only while its geodata is unchanged: the geodata
    file, and for a pack its manifest (`Geodata.content_hash`)."""
    path = cache_path(gd.name, ns.geometry_hash(), _check_band(band))
    if not os.path.isfile(path):
        return None
    try:
        table = slt.Table.read(path)
    except (OSError, ValueError):
        return None
    if table.header.get("geodata_hash") != gd.content_hash:
        return None
    return path


# What a cached table's losses depend on beyond its nodes: another table
# agreeing on all of these gives the same loss for the same two ends.
SAME_MODEL = ("geodata_hash", "pack_manifest_hash", "f0_hz", "model", "exponent",
              "p_time_pct", "p_loc_pct", "radius_m")


def _spot(node):
    """Where a node stands, as the geometry hash rounds it."""
    return (round(node["lat"], 7), round(node["lon"], 7), round(node["height_m"], 3))


def nearest_cached(gd, ns, band, radius_m=DEFAULT_RADIUS_M, planner_version=None):
    """The cached table of this geodata and band that shares the most
    unchanged nodes with `ns` (same name, position and height, under the
    same model), and the names of `ns`'s nodes it has no pairs for: (table,
    names), or (None, None) when no cached table shares two."""
    want = header_for(gd, ns, band, radius_m, planner_version)
    root = os.path.join(store.LOSSES_DIR, store.check_name(gd.name, "geodata"))
    here = {name: _spot(node) for name, node in ns.nodes.items()}
    best, best_same = None, set()
    for entry in sorted(os.listdir(root)) if os.path.isdir(root) else ():
        path = os.path.join(root, entry, "%s.bin" % band)
        if entry == ns.geometry_hash() or not os.path.isfile(path):
            continue
        try:
            table = slt.Table.read(path)
        except (OSError, ValueError):
            continue
        head = table.header
        if any(head.get(key) != want.get(key) for key in SAME_MODEL):
            continue
        if planner_version is not None and head.get("planner_version") != planner_version:
            continue
        same = {n["name"] for n in head.get("nodes") or ()
                if here.get(n["name"]) == _spot(n)}
        if len(same) > len(best_same):
            best, best_same = table, same
    if best is None or len(best_same) < 2:
        return None, None
    return best, set(ns.nodes) - best_same


async def compute(gd, ns, band, base_url=None, progress=None, radius_m=DEFAULT_RADIUS_M,
                  concurrency=None, use_cache=True, planner_version=None, notice=None):
    """The table for this geodata, nodeset and band, from the cache or
    computed into it. Returns (path of the cached file, whether it was
    already there).

    A nodeset the cache has no table for is usually one it has a table for
    with a few nodes moved, added or taken away: the nearest cached table
    (`nearest_cached`) gives every pair whose two ends it has unchanged, and
    only the pairs touching the rest are computed (`update_nodes`)."""
    band = _check_band(band)
    path = cached(gd, ns, band) if use_cache else None
    if path:
        return path, True
    donor, changed = nearest_cached(gd, ns, band, radius_m, planner_version) \
        if use_cache else (None, None)
    if donor is not None:
        if notice:
            notice("%d of %d nodes' pairs are from a cached table; computing those of the "
                   "other %d" % (len(ns.nodes) - len(changed), len(ns.nodes), len(changed)))
        table = await update_nodes(donor, gd, ns, changed, base_url, progress, radius_m,
                                   concurrency, notice=notice)
    else:
        table = await build(gd, ns, band, base_url, progress, radius_m, concurrency,
                            planner_version=planner_version, notice=notice)
    path = cache_path(gd.name, ns.geometry_hash(), band)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    write_table(table, path)
    return path, False


def write_table(table, path):
    """Write a table whole, through a temporary beside it."""
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = path + ".tmp"
    table.write(tmp)
    os.replace(tmp, path)


async def row(gd, ns, name, band, base_url=None, others=None, session=None, notice=None):
    """One node's row and column against `others` (every other node by
    default), computed now and cached nowhere: {other: (loss to it, loss from
    it, flags)}, a pair never heard as +inf. The links a node's editor draws
    before there is a table, from the nodes as they stand."""
    band = _check_band(band)
    table = slt.Table(header_for(gd, ns, band))
    pairs = [(name, o) for o in (others if others is not None else ns.nodes)
             if o != name and o in ns.nodes]
    if gd.is_pack:
        _check_pack(gd, band, base_url)
        await _fill_pack(table, gd, ns, pairs, base_url, session=session, notice=notice)
    else:
        _fill_synthetic(table, gd, ns, pairs)
    return {o: (table.loss[table.cell(name, o)], table.loss[table.cell(o, name)],
                table.flags[table.cell(name, o)]) for _, o in pairs}


async def update_nodes(table, gd, ns, names, base_url=None, progress=None,
                       radius_m=DEFAULT_RADIUS_M, concurrency=None, session=None,
                       notice=None):
    """A copy of `table` re-indexed by the nodeset as it is now, with every
    pair touching one of `names` recomputed and every other pair carried
    over by name.

    This is one node's row and column after a move, and equally a node
    added (its row is new) or removed (it is simply gone from the copy). A
    pair whose ends were both in the old table and neither is in `names`
    keeps its old cell.
    """
    names = set(names)
    head = dict(table.header)
    head["nodes"] = _nodes_header(ns)
    head["nodeset_geometry"] = ns.geometry_hash()
    head["computed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat(
        timespec="seconds")
    new = slt.Table(head)
    fresh = set(names) | {n for n in new.names if n not in table.index}
    for a in new.names:
        for b in new.names:
            if a != b and a not in fresh and b not in fresh:
                i = table.cell(a, b)
                new.put(a, b, table.loss[i], table.flags[i], table.samples[i])
    pairs = _pairs_to_do(new, new.names, fresh)
    if gd.is_pack:
        _check_pack(gd, new.band, base_url)
        await _fill_pack(new, gd, ns, pairs, base_url, progress, radius_m, concurrency, session,
                         notice)
    else:
        _fill_synthetic(new, gd, ns, pairs, progress)
    return new


def copy_into(src_path, run_dir, band):
    """Put a cached table into a run directory as the copy its ether reads."""
    target = os.path.join(run_dir, "losses", "%s.bin" % band)
    os.makedirs(os.path.dirname(target), exist_ok=True)
    shutil.copyfile(src_path, target)
    return target


# ---- the command ---------------------------------------------------------

def _emit(event, **fields):
    sys.stdout.write(json.dumps({"event": event, **fields}) + "\n")
    sys.stdout.flush()


def _progress_printer():
    """Progress as JSON lines, at most about two hundred of them a table."""
    def show(done, total):
        step = max(1, total // 200)
        if done == total or done % step == 0:
            _emit("progress", done=done, total=total)
    return show


def _load_geodata(text):
    if text.endswith(".yaml") or os.sep in text:
        return geodata_module.read(text)
    return geodata_module.load(text)


def _load_nodeset(text):
    if text.endswith(".yaml") or os.sep in text:
        return nodeset_module.open_path(text)
    return nodeset_module.load(text)


async def _main(args):
    gd = _load_geodata(args.geodata)
    ns = _load_nodeset(args.nodeset)
    radius_m = args.radius_km * 1000.0
    show = _progress_printer()

    def notice(text):
        _emit("notice", message=text)

    if args.table:
        table = slt.Table.read(args.table)
        new = await update_nodes(table, gd, ns, args.node or (), args.sidecar, show,
                                 radius_m, args.concurrency, notice=notice)
        write_table(new, args.out or args.table)
        _emit("done", path=os.path.abspath(args.out or args.table), cached=False, n=new.n)
        return
    path, hit = await compute(gd, ns, args.band, args.sidecar, show, radius_m,
                              args.concurrency, use_cache=not args.no_cache, notice=notice)
    if args.out:
        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        shutil.copyfile(path, args.out)
        path = args.out
    _emit("done", path=os.path.abspath(path), cached=hit, n=len(ns.nodes))


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Compute a loss table for a nodeset on geodata; progress as JSON lines.")
    parser.add_argument("--geodata", required=True, help="a geodata name, or a geodata file")
    parser.add_argument("--nodeset", required=True, help="a nodeset name, or a nodeset file")
    parser.add_argument("--band", default="868", choices=sorted(slt.BANDS))
    parser.add_argument("--sidecar", help="the planner-web base URL, for a pack")
    parser.add_argument("--out", help="also copy the table here (a run's losses/<band>.bin)")
    parser.add_argument("--radius-km", type=float, default=DEFAULT_RADIUS_M / 1000.0)
    parser.add_argument("--concurrency", type=int, default=None)
    parser.add_argument("--no-cache", action="store_true", help="compute even on a cache hit")
    parser.add_argument("--table", help="update this table in place (or into --out) instead")
    parser.add_argument("--node", action="append",
                        help="with --table: a node whose row and column to recompute")
    args = parser.parse_args(argv)
    try:
        asyncio.run(_main(args))
    except store.StoreError as err:
        _emit("error", message=str(err))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
