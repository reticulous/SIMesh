"""Loss tables: synthetic ground's model, offsets, the cache, one node's row,
the command, and a pack through a planner-web of the test's own."""

import asyncio
import json
import math
import os
import socket
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "ether"))

import ether  # noqa: E402
import geodata  # noqa: E402
import losses  # noqa: E402
import nodeset  # noqa: E402
import slt  # noqa: E402
import store  # noqa: E402

PLANNER_WEB = os.path.join(HERE, "..", "planner", "target", "release", "planner-web")
BERLIN_PACK = os.path.join(HERE, "..", "packs", "berlin-city")


@pytest.fixture
def stores(tmp_path, monkeypatch):
    for key, sub in (("GEODATA_DIR", "geodata"), ("NODESETS_DIR", "nodesets"),
                     ("LOSSES_DIR", "losses")):
        monkeypatch.setattr(store, key, str(tmp_path / sub))
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"exponent": 3.1}})
    ns = nodeset.create("three")
    ns.add_node("a", 0.0, 0.0)
    ns.add_node("b", 0.006, 0.0)
    ns.add_node("c", 0.0, 0.0203)
    ns.add_node("d", 0.0, 0.0)                    # on top of a: a metre apart
    ns.set_offset("a", "c", 25)
    ns.save()
    return tmp_path


def reference_loss(f_hz, exponent, a, b):
    """The log-distance formula on synthetic ground, spelled out here independently."""
    ax, ay = a[1] * 111120, a[0] * 111120
    bx, by = b[1] * 111120, b[0] * 111120
    d = max(1.0, math.hypot(ax - bx, ay - by))
    return 20 * math.log10(4 * math.pi * f_hz / 299_792_458.0) + 10 * exponent * math.log10(d)


def test_a_synthetic_table_is_the_formula_without_offsets(stores):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    table = losses.synthetic_table(gd, ns, "868")
    assert table.header["model"] == "log-distance"
    assert table.header["geodata"] == "flat" and table.header["nodes"][0]["name"] == "a"
    assert table.f0_hz == slt.f0_of("868")
    pos = {n: (d["lat"], d["lon"]) for n, d in ns.nodes.items()}
    for a in pos:
        for b in pos:
            if a == b:
                continue
            want = reference_loss(table.f0_hz, 3.1, pos[a], pos[b])
            assert table.get(a, b) == pytest.approx(want, abs=1e-4)
            assert table.get(a, b) == table.get(b, a)
            assert table.flag(a, b) == 0
    # Within the band a frame's own carrier is the free-space correction.
    f = 869_525_000
    assert table.at("a", "b", f) == pytest.approx(
        reference_loss(f, 3.1, pos["a"], pos["b"]), abs=1e-4)


def test_offsets_are_a_layer_added_both_ways(stores):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    table = losses.synthetic_table(gd, ns, "868")
    layered = losses.with_offsets({"868": table}, ns)["868"]
    assert layered is not table
    assert layered.get("a", "c") == pytest.approx(table.get("a", "c") + 25)
    assert layered.get("c", "a") == pytest.approx(table.get("c", "a") + 25)
    assert layered.get("a", "b") == table.get("a", "b")
    ns.set_offset("a", "c", 0)
    assert losses.with_offsets({"868": table}, ns)["868"] is table


def test_the_band_is_the_one_globals_carrier_falls_in(stores):
    ns = nodeset.load("three")
    assert losses.bands_of(ns) == ["868"]                # the store's globals.py
    radio = {"freq_mhz": 433.92, "sf": 8, "bw_khz": 125.0, "cr": 5}
    assert losses.bands_of(ns, radio=radio) == ["433"]
    assert losses.bands_of(ns, radio=dict(radio, freq_mhz=915.0)) == ["915"]


def test_a_table_round_trips_through_slt1(stores, tmp_path):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    table = losses.synthetic_table(gd, ns, "433")
    table.put("a", "b", slt.NEVER, slt.FLAG_OFF_PACK | slt.FLAG_BEYOND_RADIUS, 3)
    path = str(tmp_path / "t.bin")
    losses.write_table(table, path)
    back = slt.Table.read(path)
    assert back.header == table.header
    assert back.names == ["a", "b", "c", "d"]
    assert list(back.loss) == list(table.loss)
    assert list(back.flags) == list(table.flags)
    assert list(back.samples) == list(table.samples)
    assert back.get("a", "b") == math.inf and back.samples[back.cell("a", "b")] == 3


def test_the_cache_is_keyed_by_geodata_and_geometry(stores):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    path, hit = asyncio.run(losses.compute(gd, ns, "868"))
    assert not hit
    assert path == os.path.join(store.LOSSES_DIR, "flat", ns.geometry_hash(), "868.bin")
    assert asyncio.run(losses.compute(gd, ns, "868")) == (path, True)
    ns.set_node("a", tags=["x"], antenna={"type": "yagi_directional"},
                max_dbm=27)                                      # not geometry: still a hit
    ns.set_offset("a", "b", 12)
    assert asyncio.run(losses.compute(gd, ns, "868"))[1]
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"exponent": 3.2}})
    gd = geodata.load("flat")
    assert losses.cached(gd, ns, "868") is None            # the ground changed
    assert slt.Table.read(asyncio.run(losses.compute(gd, ns, "868"))[0]).header["exponent"] == 3.2


def test_a_changed_nodeset_computes_only_the_pairs_the_cache_has_not(stores):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    asyncio.run(losses.compute(gd, ns, "868"))
    ns.move_node("c", 0.001, 0.02)
    ns.add_node("e", 0.003, 0.003)
    said, totals = [], []
    path, hit = asyncio.run(losses.compute(gd, ns, "868", progress=lambda d, t: totals.append(t),
                                           notice=said.append))
    assert not hit and path == losses.cache_path("flat", ns.geometry_hash(), "868")
    # a, b and d are unchanged: of the ten pairs only the seven touching c or e are asked.
    assert set(totals) == {7}
    assert said == ["3 of 5 nodes' pairs are from a cached table; computing those of the other 2"]
    got, full = slt.Table.read(path), losses.synthetic_table(gd, ns, "868")
    assert got.names == full.names and list(got.loss) == pytest.approx(list(full.loss))
    # Another geodata, or another ground under this one, gives nothing to reuse.
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"exponent": 3.3}})
    assert losses.nearest_cached(geodata.load("flat"), ns, "868") == (None, None)


def test_one_nodes_row_and_column_are_recomputed_into_a_copy(stores):
    gd, ns = geodata.load("flat"), nodeset.load("three")
    table = losses.synthetic_table(gd, ns, "868")
    table.put("b", "c", 1.0)                               # a marker that must survive
    table.put("c", "b", 1.0)
    ns.move_node("a", 0.016, 0.0)
    ns.remove_node("d")
    ns.add_node("e", 0.002, 0.0)
    done = []
    new = asyncio.run(losses.update_nodes(table, gd, ns, ["a"],
                                          progress=lambda d, t: done.append((d, t))))
    assert new.names == ["a", "b", "c", "e"]
    assert new.get("b", "c") == 1.0                        # untouched pair carried over
    assert new.get("a", "c") == pytest.approx(
        reference_loss(new.f0_hz, 3.1, (0.016, 0.0), (0.0, 0.0203)), abs=1e-4)
    assert new.get("e", "b") == pytest.approx(
        reference_loss(new.f0_hz, 3.1, (0.002, 0.0), (0.006, 0.0)), abs=1e-4)
    assert table.get("a", "b") != new.get("a", "b")        # the original is left alone
    assert done[-1] == (5, 5)                              # a-b a-c a-e b-e c-e
    assert new.header["nodes"][0]["lat"] == 0.016


def test_the_command_prints_progress_as_json_lines(stores, tmp_path):
    env = dict(os.environ)
    code = ("import sys, store; store.GEODATA_DIR, store.NODESETS_DIR, store.LOSSES_DIR = "
            "sys.argv[1:4]; import losses; sys.exit(losses.main(sys.argv[4:]))")
    out = str(tmp_path / "run" / "losses" / "868.bin")
    proc = subprocess.run(
        [sys.executable, "-c", code, store.GEODATA_DIR, store.NODESETS_DIR, store.LOSSES_DIR,
         "--geodata", "flat", "--nodeset", "three", "--band", "868", "--out", out],
        cwd=HERE, env=env, capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr
    events = [json.loads(line) for line in proc.stdout.splitlines()]
    assert events[-2] == {"event": "progress", "done": 6, "total": 6}
    assert events[-1]["event"] == "done" and events[-1]["path"] == out
    assert slt.Table.read(out).n == 4
    bad = subprocess.run(
        [sys.executable, "-c", code, store.GEODATA_DIR, store.NODESETS_DIR, store.LOSSES_DIR,
         "--geodata", "nothere", "--nodeset", "three"],
        cwd=HERE, capture_output=True, text=True, timeout=60)
    assert bad.returncode == 1
    assert json.loads(bad.stdout)["event"] == "error"


def test_link_json_replies_become_cells():
    near = {"lb_db": 99.5, "fresnel": {"verdict": "clear"},
            "profile_evidence": {"model": "free space + P.526 diffraction (inside ...)"}}
    assert losses.cell_from_reply(near, 869.525e6, 100) == (
        99.5, slt.FLAG_NEAR_FIELD | slt.FLAG_LOS_CLEAR)
    far = {"lb_db": 140.0, "fresnel": {"verdict": "obstructed"},
           "profile_evidence": {"model": "ITU-R P.1812-8"}}
    assert losses.cell_from_reply(far, 869.525e6, 1000) == (140.0, 0)
    assert losses.cell_from_reply((400, "path leaves the pack"), 1, 1) == (
        slt.NEVER, slt.FLAG_OFF_PACK)
    assert losses.cell_from_reply((400, "path longer than the pack window cap"), 1, 1) == (
        slt.NEVER, slt.FLAG_BEYOND_RADIUS)
    loss, flags = losses.cell_from_reply((400, "the two ends are within 20 m"), 869.525e6, 10)
    assert flags == slt.FLAG_NEAR_FIELD
    assert loss == pytest.approx(ether.fspl_1m_db(869.525e6) + 20)
    with pytest.raises(losses.LossError):
        losses.cell_from_reply((500, "boom"), 1, 1)


# ---- a pack, through a real sidecar --------------------------------------

def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run_sidecar(ready):
    if not (os.path.isfile(PLANNER_WEB) and os.path.isfile(os.path.join(BERLIN_PACK, "manifest.json"))):
        pytest.skip("no planner-web build in planner/ or no berlin-city pack in packs/")
    port = free_port()
    proc = subprocess.Popen([PLANNER_WEB, "--pack", BERLIN_PACK, "--host", "127.0.0.1",
                             "--port", str(port)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = "http://127.0.0.1:%d" % port
    try:
        asyncio.run(ready(base))
        yield base
    finally:
        proc.terminate()
        proc.wait(timeout=10)


@pytest.fixture
def sidecar():
    yield from run_sidecar(wait_ready)


@pytest.fixture
def fresh_sidecar():
    """A sidecar that answers, its building index still loading."""
    yield from run_sidecar(wait_answering)


async def wait_answering(base):
    import aiohttp
    async with aiohttp.ClientSession() as session:
        for _ in range(600):
            try:
                async with session.get(base + "/api/pack") as resp:
                    if resp.status == 200:
                        return
            except aiohttp.ClientError:
                pass
            await asyncio.sleep(0.1)
    raise RuntimeError("planner-web at %s never answered" % base)


async def wait_ready(base):
    """Until a link reply rests on the building index, not the raster alone:
    before that, the same pair can come back with a different number."""
    import aiohttp
    async with aiohttp.ClientSession() as session:
        for _ in range(600):
            try:
                async with session.get(base + "/link.json", params={
                        "ax": 392000, "ay": 5820000, "bx": 392500, "by": 5820200}) as resp:
                    if resp.status == 200:
                        reply = await resp.json(content_type=None)
                        if reply["profile_evidence"].get("buildings_index") == "ready":
                            return
            except aiohttp.ClientError:
                pass
            await asyncio.sleep(0.1)
    raise RuntimeError("planner-web at %s never became ready" % base)


async def link(base, params):
    import aiohttp
    async with aiohttp.ClientSession() as session:
        async with session.get(base + "/link.json", params=params) as resp:
            return resp.status, await resp.json(content_type=None)


def test_a_pack_table_cell_is_what_link_json_says(stores, sidecar):
    geodata.write(geodata.geodata_path("berlin"), {"pack": BERLIN_PACK})
    gd = geodata.load("berlin")
    ns = nodeset.create("mitte")
    ns.add_node("alex", 52.5219, 13.4132, height_m=12)
    ns.add_node("hack", 52.5245, 13.4020, height_m=8)
    ns.add_node("jann", 52.5170, 13.4190, height_m=4)
    ns.add_node("near", 52.52195, 13.41325, height_m=3)  # a few metres from alex
    steps = []
    table = asyncio.run(losses.pack_table(gd, ns, "868", sidecar,
                                          progress=lambda d, t: steps.append((d, t))))
    assert steps[-1] == (6, 6)
    assert table.header["model"] == "P.1812-8" and table.f0_hz == losses.LINK_F0_HZ
    assert table.header["pack_manifest_hash"] == gd.pack_manifest_hash

    (ax, ay), (bx, by) = (gd.to_xy(ns.nodes[n]["lat"], ns.nodes[n]["lon"])
                          for n in ("alex", "hack"))
    status, reply = asyncio.run(link(sidecar, {
        "ax": "%.3f" % ax, "ay": "%.3f" % ay, "bx": "%.3f" % bx, "by": "%.3f" % by,
        "tx_h": 12, "rx_h": 8}))
    assert status == 200
    assert table.get("alex", "hack") == pytest.approx(reply["lb_db"], abs=1e-3)
    status, reply = asyncio.run(link(sidecar, {
        "ax": "%.3f" % bx, "ay": "%.3f" % by, "bx": "%.3f" % ax, "by": "%.3f" % ay,
        "tx_h": 8, "rx_h": 12}))
    assert status == 200
    assert table.get("hack", "alex") == pytest.approx(reply["lb_db"], abs=1e-3)
    for a in table.names:
        for b in table.names:
            if a != b:
                assert math.isfinite(table.get(a, b))
    d = gd.distance_m((52.5219, 13.4132), (52.52195, 13.41325))
    assert d < 20
    assert table.flag("alex", "near") & slt.FLAG_NEAR_FIELD
    assert table.get("alex", "near") == pytest.approx(losses.free_space_db(losses.LINK_F0_HZ, d),
                                                      abs=1e-3)

    # Off the pack, and beyond the radius, are never heard, flagged, and not asked.
    ns.add_node("potsdam", 52.3906, 13.0645)
    moved = asyncio.run(losses.update_nodes(table, gd, ns, [], sidecar, radius_m=2000))
    assert moved.get("alex", "hack") == table.get("alex", "hack")
    assert moved.get("potsdam", "alex") == slt.NEVER
    assert moved.flag("potsdam", "alex") == slt.FLAG_OFF_PACK


def test_a_table_begun_while_the_sidecar_indexes_waits_for_the_index(stores, fresh_sidecar):
    """A sidecar just started answers from the clutter raster until its
    building index is in; a table begun then waits, says why, and its cells
    are the numbers link.json gives once the index is ready."""
    geodata.write(geodata.geodata_path("berlin"), {"pack": BERLIN_PACK})
    gd = geodata.load("berlin")
    ns = nodeset.create("mitte")
    ns.add_node("alex", 52.5219, 13.4132, height_m=12)
    ns.add_node("hack", 52.5245, 13.4020, height_m=8)
    ns.add_node("jann", 52.5170, 13.4190, height_m=4)
    notices = []
    table = asyncio.run(losses.pack_table(gd, ns, "868", fresh_sidecar,
                                          notice=notices.append))
    assert notices and "indexing" in notices[0]
    xy = {n: gd.to_xy(ns.nodes[n]["lat"], ns.nodes[n]["lon"]) for n in ns.nodes}
    for a in ns.nodes:
        for b in ns.nodes:
            if a == b:
                continue
            status, reply = asyncio.run(link(fresh_sidecar, {
                "ax": "%.3f" % xy[a][0], "ay": "%.3f" % xy[a][1],
                "bx": "%.3f" % xy[b][0], "by": "%.3f" % xy[b][1],
                "tx_h": "%g" % ns.nodes[a]["height_m"],
                "rx_h": "%g" % ns.nodes[b]["height_m"]}))
            assert status == 200 and reply["profile_evidence"]["buildings_index"] == "ready"
            assert table.get(a, b) == pytest.approx(reply["lb_db"], abs=1e-3)


def test_a_pack_has_only_the_868_table(stores):
    if not os.path.isfile(os.path.join(BERLIN_PACK, "manifest.json")):
        pytest.skip("no berlin-city pack in packs/")
    geodata.write(geodata.geodata_path("berlin"), {"pack": BERLIN_PACK})
    gd = geodata.load("berlin")
    ns = nodeset.create("empty")
    assert losses.bands_of(ns, gd) == ["868"]
    with pytest.raises(losses.LossError, match="869.525 MHz only"):
        asyncio.run(losses.pack_table(gd, ns, "433", "http://127.0.0.1:1"))
    with pytest.raises(losses.LossError, match="sidecar"):
        asyncio.run(losses.pack_table(gd, ns, "868", None))
