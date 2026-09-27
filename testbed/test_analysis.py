"""The analysis tools against a small run laid out here: the repository's
smoke4 nodeset on plain-27, its loss table computed there, and a
hand-written record and station logs; and the traffic driver against a
stand-in simulation."""

import asyncio
import base64
import calendar
import contextlib
import io
import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "ether"))

import airtime  # noqa: E402
import compare  # noqa: E402
import delivery  # noqa: E402
import geodata  # noqa: E402
import links  # noqa: E402
import losses  # noqa: E402
import nodeset  # noqa: E402
import runs  # noqa: E402
import seq  # noqa: E402
import slt  # noqa: E402

import simesh  # noqa: E402
from simesh import reticulum  # noqa: E402
from simesh.reticulum import frames  # noqa: E402
from simesh.reticulum import traffic as rtraffic  # noqa: E402
from simesh.view import RunView  # noqa: E402

CALLING = 869_525_000
TRAFFIC_CH = 869_100_000
EPOCH = calendar.timegm((2026, 9, 25, 10, 0, 0))        # T = 0, wall clock
DEST = bytes(range(16))


def announce(hops):
    """An RNode-framed Reticulum announce of an lxmf.delivery destination."""
    data = b"\x11" * 64 + frames.name_hash("lxmf.delivery") + b"\x22" * 20
    return b"\x00" + bytes([0x01, hops]) + DEST + b"\x00" + data


def data_packet():
    return b"\x00" + bytes([0x00, 0]) + bytes(16) + b"\x00" + b"payload" * 4


class Record:
    """A record written line by line, the way the ether writes one."""

    def __init__(self):
        self.lines = ["# 2026-09-25T10:00:00+00:00\tether record: stamp\tdir\tsid\tjson"]
        self.eid = 0

    def add(self, t, direction, sid, msg):
        self.lines.append("%.6f\t%s\t%d\t%s" % (t, direction, sid,
                                                 json.dumps(msg, separators=(",", ":"))))

    def tx(self, t, sid, payload, heard, freq=CALLING, power=14, span=0.2):
        t0, t_end = int(round(t * 1e6)), int(round((t + span) * 1e6))
        b64 = base64.b64encode(payload).decode()
        self.add(t, "in", sid, {"type": "tx", "t0": t0, "t_end": t_end, "freq": freq,
                                "power_dbm": power, "sf": 8, "bw": 125000, "payload": b64})
        for rsid, verdict in heard.items():
            self.eid += 1
            self.add(t, "out", rsid, {"type": "rx_begin", "id": self.eid, "t0": t0,
                                      "t_end": t_end})
            self.add(t + span, "out", rsid, {"type": "rx_end", "id": self.eid,
                                             "verdict": verdict, "payload": b64})

    def write(self, path):
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("\n".join(self.lines) + "\n")


def lay_out(tmp_path, kind="reticulous", bare=False, name="run"):
    gd, ns = geodata.load("plain-27"), nodeset.load("smoke4")
    if bare:
        for each in ns.nodes:
            ns.set_node(each, role="", radio={})
    table = tmp_path / "868.bin"
    if not table.exists():
        losses.synthetic_table(gd, ns, "868").write(str(table))
    run = runs.create_run(str(tmp_path / name), gd, ns, None, "max", {"868": str(table)},
                          builds={"stable": {"kind_type": kind}})

    rec = Record()
    rec.add(0, "in", 1, {"type": "hello", "sid": 1, "slots": [0], "t": 0})
    rec.add(0, "out", 1, {"type": "welcome", "epoch": EPOCH * 1_000_000, "mode": "virtual",
                          "t": 0})
    for sid in (2, 3, 4):
        rec.add(0.1 * sid, "in", sid, {"type": "hello", "sid": sid, "slots": [0], "t": 0})
    for sid in (1, 2, 3, 4):
        rec.add(1.0, "in", sid, {"type": "state", "mode": "RX", "freq": CALLING, "sf": 8})
    rec.tx(2.0, 1, announce(0), {2: "clean", 3: "clean", 4: "crc"})
    rec.tx(3.0, 2, announce(1), {1: "clean", 3: "clean"})
    rec.tx(4.0, 2, bytes([0xC2, 1, 2, 3]), {1: "clean"})
    rec.tx(5.0, 2, data_packet(), {1: "clean"}, freq=TRAFFIC_CH, power=8, span=0.1)
    rec.tx(5.2, 1, data_packet(), {2: "clean"}, freq=TRAFFIC_CH, power=14, span=0.1)
    rec.tx(6.0, 4, announce(0)[:-1] + b"\x33", {})
    rec.write(os.path.join(run.dir, "record.tsv"))

    for dev, lines in (("n01", ["Sep 25 10:00:07.000 I [lxmf] id 0: DIRECT delivered mid=o_1_ab"]),
                       ("n02", ["Sep 25 10:00:08.000 W [lxmf] id 0: DIRECT failed mid=o_2_cd "
                                "tag=lxmf.id0.4502915b (no_path)"])):
        os.makedirs(run.node_dir(dev), exist_ok=True)
        with open(os.path.join(run.node_dir(dev), "log"), "w") as handle:
            handle.write("\n".join(lines) + "\n")
    return run


@pytest.fixture
def run(tmp_path):
    return lay_out(tmp_path)


def call(main, argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = main(argv)
    return code, out.getvalue()


# ---- the run, as the tools see it ----------------------------------------

def test_a_run_view_takes_everything_from_the_run(run):
    view = RunView(run.dir)
    assert view.names == {1: "n01", 2: "n02", 3: "n03", 4: "n04"}
    assert view.calling_hz() == CALLING
    assert view.radio("n01") == {"freq_hz": CALLING, "sf": 8, "bw_hz": 125000, "power_dbm": 14.0}
    assert view.roles() == {1: "transport", 2: "transport", 3: "transport", 4: "transport"}
    # Levels are the table's, less nothing: n01's frame at n02 is its power
    # plus both gains less the loss the run's table holds.
    table = slt.Table.read(run.table_path("868"))
    e = view.medium()
    assert e.level(1, 2, CALLING, 14) == pytest.approx(14 - table.at("n01", "n02", CALLING))
    need = e.noise(125000) + e.sensitivity(8)
    assert view.audible(1, 2) == (e.level(1, 2, CALLING, 14) >= need)
    n1, n2 = view.nodes["n01"], view.nodes["n02"]
    assert view.distance(1, 2) == pytest.approx(geodata.load("plain-27").distance_m(
        (n1["lat"], n1["lon"]), (n2["lat"], n2["lon"])))
    assert view.protocols() == [reticulum] and view.kind_type("n01") == "reticulous"


def test_offsets_reach_the_medium_the_tools_read(tmp_path):
    run = lay_out(tmp_path)
    ns = run.nodeset()
    before = RunView(run.dir).medium().level(1, 2, CALLING, 14)
    ns.set_offset("n01", "n02", 20)
    ns.save()
    assert RunView(run.dir).medium().level(1, 2, CALLING, 14) == pytest.approx(before - 20)


def test_a_run_with_no_reticulous_station_reads_no_protocol(tmp_path):
    other = lay_out(tmp_path, kind="berlinmesh", bare=True, name="bm")
    view = RunView(other.dir)
    assert set(view.roles().values()) == {"client"} and view.forwarders() == set()
    assert view.protocols() == []
    assert view.radio("n01")["sf"] == simesh.DEFAULT_SF
    assert simesh.protocol_for("berlinmesh") is None
    code, text = call(airtime.main, [other.dir, "--roles"])
    out = json.loads(text)
    assert out["roles"]["client"]["stations"] == 4
    assert set(out["by_kind"]) == {"frame"}
    code, text = call(seq.main, [other.dir, "--tail", "1"])
    assert code == 0 and "MHz" in text and "ANNOUNCE" not in text


# ---- each tool's main path ----------------------------------------------

def test_airtime(run):
    code, text = call(airtime.main, [run.dir, "--busy", "--roles"])
    out = json.loads(text)
    assert code == 0 and out["frames"] == 6 and out["calling_hz"] == CALLING
    assert out["by_kind"]["announce lxmf.delivery"]["frames"] == 3
    assert out["by_kind"]["SUPE HAIL"]["frames"] == 1
    assert out["airtime"]["traffic_channels_s"] == pytest.approx(0.2)
    assert out["by_carrier_mhz"] == {"869.1": pytest.approx(0.2), "869.5": pytest.approx(0.8)}
    assert out["losses"]["receptions_crc"] == 1 and out["losses"]["frames_nobody_received"] == 1
    assert out["exchanges"] == {"count": 1, "reduced_power": 1, "frames_mean": 2.0}
    assert out["roles"]["transport"]["stations"] == 4
    assert out["roles"]["traffic_channels"] == 1
    assert out["busy_calling"]["top"][0][0] in ("n01", "n02", "n03", "n04")


def test_links(run):
    code, text = call(links.main, [run.dir, "--min-clean", "1", "--power"])
    out = json.loads(text)
    assert code == 0 and out["usable_links_one_way"] == 4     # the calling channel's
    assert out["both_ways"] == 1                     # n01 <-> n02
    assert out["stations_hearing_nobody"] == ["n04"]
    assert out["diameter_both_ways_through_forwarders"]["hops"] == 1
    model = out["model"]
    view = RunView(run.dir)
    expected = sum(1 for a in view.names for b in view.names if a != b and view.audible(a, b))
    assert model["links_one_way"] == expected and model["sf"] == 8 and model["power_dbm"] == 14
    assert out["power_traffic_channels"]["frames"] == 2
    assert out["power_traffic_channels"]["frames_with_peer"] == 2


def test_seq(run):
    code, text = call(seq.main, [run.dir])
    assert code == 0
    assert "n01" in text.splitlines()[0] and "n04" in text.splitlines()[0]
    assert "ANNOUNCE    single/00010203  of lxmf.delivery" in text
    assert "SUPE HAIL" in text and "→ nobody" in text
    code, text = call(seq.main, ["--record", os.path.join(run.dir, "record.tsv"),
                                 "--names", "1=alpha", "--only", "supe"])
    assert code == 0 and "alpha" in text and len(text.splitlines()) == 4


def test_compare(run, tmp_path):
    code, text = call(compare.main, [run.dir, run.dir, "--logs"])
    assert code == 0
    assert "every station announced" in text
    assert "first 1-hop path" in text and "first 2-hop path" in text
    assert "n01" in text and "LXMF messages proven delivered" in text
    rows = [line.split() for line in text.splitlines() if line.startswith("n01 ")]
    assert rows[0] == ["n01", "7", "7"]            # delivered 7 s after the first hello


def test_delivery(run, tmp_path):
    drive = {"sends": [
        {"marker": "G0001", "src": "n01", "dst": "n02", "cls": "short", "hops": 1,
         "mid": "o_1_ab", "t_sent": 5_000_000},
        {"marker": "G0002", "src": "n02", "dst": "n01", "cls": "two", "hops": None,
         "mid": "o_2_cd", "t_sent": 6_000_000},
        {"marker": "G0003", "src": "n03", "dst": "n04", "cls": "big", "mid": None}]}
    path = tmp_path / "traffic.json"
    path.write_text(json.dumps(drive))
    out_json = tmp_path / "delivery.json"
    code, text = call(delivery.main, [str(path), run.dir, "--json", str(out_json)])
    out = json.loads(text)
    assert code == 0 and out["sent"] == 3 and out["delivered"] == 1 and out["no_mid"] == 1
    assert out["by_route_hops"] == {"1": "1/1 (100.0%)", "no path": "0/2 (0.0%)"}
    assert out["latency_s"]["median"] == pytest.approx(2.0)
    words = dict(out["undelivered_last_word"])
    assert "no mid" in words and any("failed" in w for w in words)
    rows = json.loads(out_json.read_text())["messages"]
    # On plain-27 n01 and n02 (2.8 km) hear each other; n03 and n04 (19 km)
    # do not, and meet through n02, a transport.
    graph = RunView(run.dir).radio_graph()
    assert 2 in graph[1] and 4 not in graph[3] and {3, 4} <= graph[2]
    assert [r["radio_hops"] for r in rows] == [1, 1, 2]


# ---- the traffic driver, against a stand-in simd ------------------------

class FakeSim:
    """Just enough of a simulation's control socket for a driver: its
    snapshot with the stations up, and commands and intents answered by the
    stations chosen, each answer carrying the asker's id."""

    def __init__(self, names):
        self.names = names
        self.got = []

    async def handle(self, request):
        from aiohttp import web
        ws = web.WebSocketResponse()
        await ws.prepare(request)
        await ws.send_json({"type": "snapshot", "clock": {"t": 1_000_000},
                            "run": {"dir": "/runs/fake"},
                            "nodes": [{"name": n, "id": i + 1, "status": "up", "kind": "reticulous",
                                       "tags": ["even"] if i % 2 else []}
                                      for i, n in enumerate(self.names)]})
        async for msg in ws:
            m = json.loads(msg.data)
            self.got.append(m)
            if m["type"] in ("command", "meta"):
                who = [m["name"]] if m.get("name") else m.get("names") or self.names
                line = m.get("line") or m.get("verb")
                results = {n: ("* 0 %s %s" % (n, ("%02d" % i) * 16) if line == "lxmf"
                               else "3 paths total") for i, n in enumerate(who)}
                await ws.send_json({"type": "command_result", "id": m.get("id"),
                                    "name": m.get("name"), "t": 2_000_000, "results": results})
        return ws


def with_fake_sim(fake, drive):
    from aiohttp import web

    async def go():
        app = web.Application()
        app.router.add_get("/ws", fake.handle)
        runner = web.AppRunner(app)
        await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]
        sim = await simesh.attach("fake", port)
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                return await drive(sim)
        finally:
            await sim.close()
            await sim.session.close()
            await runner.cleanup()
    return asyncio.run(go())


def test_a_driver_chooses_stations_and_asks_them(tmp_path):
    fake = FakeSim(["n01", "n02", "n03"])

    async def drive(sim):
        assert sim.run_dir == "/runs/fake" and sim.run_s == 0
        await sim.all_up()
        evens = sim.nodes(tag="even")
        assert evens.names == ["n02"] and len(sim.nodes(kind="reticulous")) == 3
        assert await evens.announce(spread=30) == {"n02": "3 paths total"}
        assert await sim.node("n01").run("x", after=2) == {"n01": "3 paths total"}
        assert sim.pairs(sample=2, seed=1) == sim.pairs(sample=2, seed=1)
        assert len(sim.pairs()) == 6
        with pytest.raises(simesh.SimError):
            sim.node("nobody")
        await sim.plan(("warm", 10))

    with_fake_sim(fake, drive)
    meta = [m for m in fake.got if m["type"] == "meta"][0]
    assert meta["verb"] == "announce" and meta["names"] == ["n02"] and meta["stagger"] == 30
    command = [m for m in fake.got if m["type"] == "command"][0]
    assert command["names"] == ["n01"] and command["after"] == 2
    assert [m for m in fake.got if m["type"] == "plan"][0]["phases"] == [
        {"name": "warm", "until": 11_000_000}]


def test_the_traffic_driver_runs_on_a_sim(tmp_path):
    fake = FakeSim(["n01", "n02"])
    out = tmp_path / "out.json"
    opts = {"warm_rounds": 0, "traffic": 0, "drain": 0, "gather": ["rnpath -s"]}
    result = with_fake_sim(fake, lambda sim: rtraffic.run_on(sim, opts, str(out)))
    commands = [m for m in fake.got if m["type"] == "command"]
    assert commands and all(m["kind"] == "reticulous" for m in commands)
    assert sorted(result["dests"]) == ["n01", "n02"]
    assert result["gathered"]["rnpath -s"]["results"]["n01"] == "3 paths total"
    assert json.loads(out.read_text())["phases"][-1][0] == "gathered"
    with pytest.raises(ValueError, match="no traffic option"):
        rtraffic.Options(colour="blue")


def test_the_schedule_is_the_seed_s():
    one = rtraffic.schedule(["a", "b", "c"], 17, 30, 5, "G")
    assert one == rtraffic.schedule(["c", "b", "a"], 17, 30, 5, "G")
    assert [s[0] for s in one] == [1, 2, 3, 4, 5, 6] and all(s[2] != s[3] for s in one)
