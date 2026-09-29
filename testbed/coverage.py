"""Coverage: one node's path loss to every point around it, on a pack, cached.

```
page ── coverage {geodata, nodes: [{name, lat, lon, height_m}]} ─► front
front ── coverage {tiles: [{node, key, cached}]} ────────────────► page       at once
front ── GET /loss/start?lat&lon&tx_h&rx_h&radius_km ────────────► planner-web  one node at a time
front ── GET /loss/status … until the sweep is whole ────────────► planner-web
front ── GET /loss.bin?lat&lon&minx&miny&maxx&maxy&w&h ───────────► planner-web
front: testbed/coverage/<geodata>/<key>.bin
front ── coverage_tile {geodata, node, key} ─────────────────────► page       as each lands
page ── GET /api/coverage?geodata=&key= ─────────────────────────► front      the raster
```

A raster is the planner's own point-to-area sweep (`planner-coverage`) from
the node's antenna to a receiver `RX_HEIGHT_M` above the ground everywhere
within `RADIUS_KM`, as its `loss.bin` serves it:

    "PLS2" | u32 w | u32 h | f64 ox | f64 oy | f64 rx | f64 ry | u16 loss_db·100 [w·h]

in the pack's CRS, row-major from the north-west, `(ox, oy)` the centre of
the first cell, 65535 where nothing was evaluated. Its cells are the pack's
own, up to `CELLS` across the square (planner-web answers no finer than the
sweep, and no more than 2048 a side): 10 m on a 10 m pack, 30 m on a 30 m
one, about 10 m on anything finer. It is path loss only:
the page adds the node's transmit power and antenna gain, and draws the best
level at each point over the nodes on show.

A raster depends on the pack and the node's position and height alone, so
its key is those (`key`): moving a node or changing its height is a new
raster, and changing its power, gain or radio is not. The sidecar computes
one sweep at a time and a new one cancels the last, so the front asks for
one node at a time per sidecar. Synthetic ground has no rasters: its
log-distance loss is a formula the page works out itself.
"""

import asyncio
import hashlib
import json
import math
import os

import store

MAGIC = b"PLS2"
RX_HEIGHT_M = 2.0
RADIUS_KM = 10.0
CELLS = 2048                        # asked across the square, each way; planner-web's most
POLL_S = 0.5
SWEEP_TIMEOUT_S = 600.0


def key(gd, node, rx_h=RX_HEIGHT_M, radius_km=RADIUS_KM):
    """What a node's raster depends on, hashed: the pack's content, the
    node's position and antenna height, the receiver's height, the radius
    and the cells asked for."""
    text = json.dumps([gd.content_hash, round(float(node["lat"]), 7), round(float(node["lon"]), 7),
                       round(float(node["height_m"]), 2), rx_h, radius_km, CELLS],
                      separators=(",", ":"))
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def cache_path(geodata_name, raster_key):
    if not all(c in "0123456789abcdef" for c in raster_key) or len(raster_key) != 16:
        raise store.StoreError("not a coverage key: %r" % raster_key)
    return os.path.join(store.COVERAGE_DIR, store.check_name(geodata_name, "geodata"),
                        raster_key + ".bin")


def cached(geodata_name, raster_key):
    path = cache_path(geodata_name, raster_key)
    return path if os.path.isfile(path) else None


class Sweeps:
    """The front's coverage work: one queue per sidecar, one sweep at a time."""

    def __init__(self, session):
        self.session = session
        self.locks = {}
        self.busy = set()               # (geodata, key) being computed or queued

    async def raster(self, sidecar, gd, node, rx_h=RX_HEIGHT_M, radius_km=RADIUS_KM):
        """The node's raster, computed through the sidecar into the cache
        unless it is there already: its path."""
        raster_key = key(gd, node, rx_h, radius_km)
        path = cached(gd.name, raster_key)
        if path:
            return path
        lock = self.locks.setdefault(sidecar, asyncio.Lock())
        async with lock:
            path = cached(gd.name, raster_key)
            if path:
                return path
            data = await self.sweep(sidecar, gd, node, rx_h, radius_km)
            path = cache_path(gd.name, raster_key)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            tmp = path + ".tmp"
            with open(tmp, "wb") as handle:
                handle.write(data)
            os.replace(tmp, path)
            return path

    async def sweep(self, sidecar, gd, node, rx_h, radius_km):
        import aiohttp

        x, y = gd.to_xy(float(node["lat"]), float(node["lon"]))
        where = {"x": "%.3f" % x, "y": "%.3f" % y, "tx_h": "%g" % float(node["height_m"]),
                 "rx_h": "%g" % rx_h, "radius_km": "%g" % radius_km}
        timeout = aiohttp.ClientTimeout(total=60)
        try:
            async with self.session.get(sidecar + "/loss/start", params=where,
                                        timeout=timeout) as resp:
                if resp.status != 200:
                    raise store.StoreError("coverage: /loss/start answered %d: %s"
                                           % (resp.status, (await resp.text())[:200]))
            loop = asyncio.get_running_loop()
            deadline = loop.time() + SWEEP_TIMEOUT_S
            while True:
                async with self.session.get(sidecar + "/loss/status", timeout=timeout) as resp:
                    status = await resp.json(content_type=None)
                state = status.get("state")
                if state == "failed":
                    raise store.StoreError("coverage: the sweep failed: %s" % status.get("error"))
                if state == "idle" and status.get("done") == status.get("bands") \
                        and math.isclose(float(status.get("band_km") or 0), radius_km,
                                         rel_tol=1e-6):
                    break
                if loop.time() > deadline:
                    raise store.StoreError("coverage: the sweep took over %.0f s"
                                           % SWEEP_TIMEOUT_S)
                await asyncio.sleep(POLL_S)
            span = radius_km * 1000.0
            view = dict(where, minx="%.3f" % (x - span), maxx="%.3f" % (x + span),
                        miny="%.3f" % (y - span), maxy="%.3f" % (y + span),
                        w=str(CELLS), h=str(CELLS))
            async with self.session.get(sidecar + "/loss.bin", params=view,
                                        timeout=timeout) as resp:
                if resp.status != 200:
                    raise store.StoreError("coverage: /loss.bin answered %d" % resp.status)
                data = await resp.read()
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            raise store.StoreError("coverage: the planner sidecar at %s: %s"
                                   % (sidecar, err or type(err).__name__)) from err
        if data[:4] != MAGIC:
            raise store.StoreError("coverage: loss.bin is not a PLS2 raster")
        return data
