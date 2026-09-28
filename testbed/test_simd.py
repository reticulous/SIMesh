"""simd on synthetic ground with a stand-in station kind: loading, first boot
(declared settings, the first-boot rules, the radio last), the tables and offsets, a
move recomputed into the run's copy, an id change, levels, commands and
intents on chosen stations, snapshots."""

import asyncio
import json
import os
import socket
import stat
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import devices  # noqa: E402
import kinds  # noqa: E402
import simd  # noqa: E402
import slt  # noqa: E402
import store  # noqa: E402


class Stub(kinds.Kind):
    """A kind whose station is a shell script that sleeps, is up at once and
    writes down every line it is given."""

    type_name = "stub"

    async def wait_up(self, station, timeout):
        return True

    async def run(self, station, line, timeout=None):
        with open(os.path.join(station.dir, "lines"), "a") as out:
            out.write(line + "\n")
        return "did %s" % line

    async def role(self, station):
        return "client"

    async def address(self, station):
        return "%032x" % station.node_id

    def configured(self, station):
        return os.path.exists(os.path.join(station.dir, "lines"))

    def lines(self, verb, **args):
        if verb == "name":
            return ["name {name} {id}"]
        if verb == "radio":
            return ["radio %s" % " ".join("%s=%s" % kv for kv in sorted(args.items()))]
        if verb == "radio_up":
            return ["radio up"]
        if verb == "role":
            return ["role %s" % args["role"]]
        if verb == "announce":
            return ["announce"]
        if verb == "message":
            return ["send %s %s" % (args["dest"], args["text"])]
        return super().lines(verb, **args)


class Other(Stub):
    type_name = "other"


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


STUB = "stub_local_latest"
ALL_STUB = {"which": {"all": True}, "firmware": STUB}
# What a script's runner hands a new simulation: its firmware and first-boot rules.
FAR = {"script": "far", "firmware_rules": [ALL_STUB], "first_boot_rules": [
    {"which": {"where": {"tag": "transport"}}, "lines": [{"intent": "role",
                                                           "args": {"role": "transport"}}]},
    {"which": {"not": {"where": {"tag": "no-radio"}}},
     "lines": [{"intent": "radio", "args": {"freq_mhz": 869.525, "sf": 8, "tx_dbm": "max"}}]},
    {"which": {"where": {"tag": "far"}}, "lines": ["far {addr}", "role transport"]},
    {"which": {"names": ["c"]}, "lines": ["txp {max_dbm}"]},
    {"which": {"not": {"where": {"tag": "no-radio"}}}, "lines": [{"intent": "radio_up",
                                                                  "args": {}}]}]}
STUBS = {"firmware_rules": [ALL_STUB, {"which": {"where": {"tag": "odd"}},
                                       "firmware": "other_local_latest"}]}


def horizon_pair_gain():
    """What two default antennas at one height on flat ground add to a pair."""
    import antennas
    return 2 * antennas.gain({"type": antennas.DEFAULT_TYPE}, 0.0, 0.0)


@pytest.fixture
def stores(tmp_path, monkeypatch):
    for attr in ("GEODATA_DIR", "NODESETS_DIR", "SCRIPTS_DIR", "LOSSES_DIR", "RUNS_DIR",
                 "SNAPSHOTS_DIR"):
        path = tmp_path / attr.lower()
        path.mkdir()
        monkeypatch.setattr(store, attr, str(path))
    real = kinds.kind_types
    monkeypatch.setattr(kinds, "kind_types", lambda: {**real(), "stub": Stub, "other": Other})
    local = tmp_path / "devices" / "local"
    local.mkdir(parents=True)
    monkeypatch.setattr(devices, "DEVICES_DIR", str(tmp_path / "devices"))
    elf = tmp_path / "station.sh"
    elf.write_text("#!/bin/sh\nexec sleep 60\n")
    elf.chmod(elf.stat().st_mode | stat.S_IEXEC)
    (local / "stub_local.yaml").write_text("kind: stub\nproject: Stub\nvirtual_hardware: ESP32\n"
                                           "elf: %s\n" % elf)
    (local / "stubtwo_local.yaml").write_text("kind: stub\nproject: Stubtwo\nelf: %s\n" % elf)
    (local / "other_local.yaml").write_text("kind: other\nelf: %s\n" % elf)
    (tmp_path / "geodata_dir" / "flat.yaml").write_text(
        "synthetic:\n  exponent: 3.0\n")
    (tmp_path / "nodesets_dir" / "three.yaml").write_text(
        "nodes:\n"
        "  a: { id: 1, lat: 0, lon: 0, tags: [transport] }\n"
        "  b: { id: 2, lat: 0, lon: 0.006, tags: [no-radio] }\n"
        "  c: { id: 3, lat: 0.006, lon: 0, tags: [far] }\n")
    (tmp_path / "scripts_dir" / "far.py").write_text('"""far"""\nfrom simesh import *\n')
    (tmp_path / "scripts_dir" / "globals.py").write_text(
        "FREQ_MHZ = 869.525\nSF = 8\nBW_KHZ = 125\nCR = 5\n")
    return tmp_path


def make_simd(tmp_path, *extra):
    args = simd.parse_args([
        "--bind", "127.0.0.1:%d" % free_port(), "--ether", "127.0.0.1:%d"
        % free_port(socket.SOCK_DGRAM), "--run", str(tmp_path / "runs_dir" / "t"),
        "--stagger", "0", "--net", "127.60.0.0/22", *extra])
    daemon = simd.Simd(args)
    daemon.said = []
    daemon.broadcast = lambda message: daemon.said.append(json.loads(json.dumps(message)))
    return daemon


async def until(check, seconds=5.0):
    for _ in range(int(seconds / 0.02)):
        if check():
            return True
        await asyncio.sleep(0.02)
    return False


def lines_of(run, name):
    with open(os.path.join(run.node_dir(name), "lines")) as handle:
        return handle.read().splitlines()


def test_a_run_never_lands_on_another(tmp_path):
    base = tmp_path / "lora"
    assert simd.free_run_dir(str(base)) == str(base)
    base.mkdir()
    (base / "run.yaml").write_text("{}\n")
    assert simd.free_run_dir(str(base)) == str(base) + "-2"


def test_an_override_replaces_every_firmware_of_its_kind(stores):
    refs = [STUB, "other_local_latest"]
    got = asyncio.run(kinds.ensure_builds(refs))
    assert got[STUB]["name"].startswith("Stub local ")
    assert got["other_local_latest"]["kind_type"] == "other"
    got = asyncio.run(kinds.ensure_builds(refs, override="stubtwo_local_latest"))
    assert got[STUB]["name"].startswith("Stubtwo local ")
    assert got[STUB]["asked"] == "stubtwo_local_latest"
    assert got["other_local_latest"]["kind_type"] == "other"
    with pytest.raises(kinds.CommandError, match="nothere"):
        asyncio.run(kinds.ensure_builds(["nothere_local_1"]))


def test_setup_is_the_name_then_the_first_boot_rules_in_order(stores):
    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three", **FAR})
        run = daemon.run
        assert run is not None and run.bands() == ["868"] and run.script_path
        assert sorted(daemon.ether.names.values()) == ["a", "b", "c"]
        assert await until(lambda: all(
            daemon.stations.get(n) and daemon.stations[n].status == "up" for n in "abc"))
        # The name, then the rules in order: intents in the kind's lines, a
        # transmit power of "max" and {max_dbm} the node's maximum.
        radio = "radio freq_mhz=869.525 sf=8 tx_dbm=22.0"
        assert lines_of(run, "a")[:4] == ["name a 1", "role transport", radio, "radio up"]
        assert lines_of(run, "b") == ["name b 2"]                 # no-radio, no role
        assert lines_of(run, "c")[:6] == ["name c 3", radio, "far 127.60.0.7",
                                          "role transport", "txp 22", "radio up"]
        assert run.meta["first_boot_rules"] == FAR["first_boot_rules"]
        node = [m for m in daemon.said if m["type"] == "node" and m["name"] == "c"][-1]
        assert node["firmware"] == STUB and node["device_name"].endswith("(virtual ESP32)")
        assert node["kind"] == "stub" and node["antenna"] == {"type": "whip_sma_quarter_wave"}
        assert daemon.run.meta["firmware"] == {n: STUB for n in "abc"}
        for key in ("lat", "lon", "height_m", "height_from", "antenna", "tags", "role", "stale"):
            assert key in node
        assert daemon.facts("a")["role"] in ("transport", "client")   # live, else its tag
        snap = daemon.snapshot()
        assert snap["script"]["name"] == "far" and snap["geodata"]["kind"] == "synthetic"
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())


def test_moves_offsets_ids_and_levels(stores):
    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three", **STUBS})
        run = daemon.run
        assert await until(lambda: len(daemon.stations) == 3)
        before = daemon.tables["868"].get("a", "b")
        gain = horizon_pair_gain()
        await daemon.do_nodeset_move({"name": "b", "lat": 0.0, "lon": 0.03})
        assert "b" in daemon.stale
        assert [m for m in daemon.said if m["type"] == "node" and m["name"] == "b"][-1]["stale"]
        assert await until(lambda: not daemon.stale)
        after = daemon.tables["868"].get("a", "b")
        assert after > before + 10
        assert slt.Table.read(run.table_path("868")).get("a", "b") == pytest.approx(after)
        # The medium's table has the antennas on it, the run's is the model's own.
        assert daemon.ether.tables["868"].get("b", "a") == pytest.approx(after - gain, abs=0.01)
        moves = [e for e in run.edits() if e["what"] == "move"]
        assert moves and moves[-1]["node"] == "b" and "t" in moves[-1]
        assert run.nodeset().node("b")["lon"] == 0.03

        await daemon.do_nodeset_offset({"between": ["a", "b"], "db": 30, "note": "wall"})
        assert daemon.tables["868"].get("a", "b") == pytest.approx(after)
        assert daemon.ether.tables["868"].get("a", "b") == pytest.approx(after + 30 - gain,
                                                                         abs=0.01)

        # A yagi aimed away from b takes its back's floor off the pair; aimed
        # at it (b is due east of a), its peak.
        await daemon.do_nodeset_set({"name": "a", "antenna": {"type": "yagi_directional",
                                                              "azimuth_deg": 270}})
        away = daemon.ether.tables["868"].get("a", "b")
        await daemon.do_nodeset_set({"name": "a", "antenna": {"type": "yagi_directional",
                                                              "azimuth_deg": 90}})
        toward = daemon.ether.tables["868"].get("a", "b")
        assert away - toward == pytest.approx(18.0, abs=0.1)
        assert daemon.tables["868"].get("a", "b") == pytest.approx(after)
        await daemon.do_nodeset_set({"name": "a", "antenna": {"type": "whip_sma_quarter_wave"}})
        assert run.nodeset().offsets == [{"between": ["a", "b"], "db": 30.0, "note": "wall"}]

        await daemon.do_levels({"name": "a"})
        levels = [m for m in daemon.said if m["type"] == "levels"][-1]
        assert levels["freq"] == pytest.approx(869.525e6) and "c" in levels["heard"]

        await daemon.do_nodeset_set({"name": "c", "id": 9})
        assert daemon.ether.names.get(9) == "c" and 3 not in daemon.ether.names
        assert any(m["type"] == "notice" and "id 9" in m["text"] for m in daemon.said)
        assert await until(lambda: daemon.stations["c"].node_id == 9)
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())


def test_commands_and_intents_go_to_the_stations_chosen(stores):
    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three", **STUBS})
        assert await until(lambda: len(daemon.stations) == 3 and all(
            s.status == "up" for s in daemon.stations.values()))

        await daemon.do_command({"line": "hello {name}", "tag": "far", "id": "x1"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["id"] == "x1" and got["results"] == {"c": "did hello c"}

        await daemon.do_command({"line": "hi", "names": ["a", "b"], "id": "x2"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert sorted(got["results"]) == ["a", "b"]

        await daemon.do_meta({"verb": "announce", "id": "x3"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["verb"] == "announce" and got["results"] == {
            n: "did announce" for n in "abc"}

        await daemon.do_meta({"verb": "message", "name": "a", "args": {"to": "b", "text": "yo"},
                              "id": "x4"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["results"] == {"a": "did send %032x yo" % 2}

        await daemon.do_meta({"verb": "path", "name": "a", "args": {"to": "b"}, "id": "x5"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["results"]["a"].startswith("! a stub station has no way to path")

        await daemon.do_meta({"verb": "address", "id": "x6"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["results"]["c"] == "%032x" % 3

        # A sequence: its steps one after the other at one T, answered once.
        await daemon.do_sequence({"steps": [{"type": "command", "line": "first {name}"},
                                            {"type": "meta", "verb": "message",
                                             "args": {"to": "b", "text": "then"}}],
                                  "names": ["a"], "id": "x8"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["id"] == "x8" and got["results"] == [{"a": "did first a"},
                                                         {"a": "did send %032x then" % 2}]
        assert lines_of(daemon.run, "a")[-2:] == ["first a", "send %032x then" % 2]

        # A line is one kind's language: a mixed choice is refused unless narrowed.
        await daemon.do_nodeset_add({"name": "d", "lat": 0.001, "lon": 0.001, "tags": ["odd"]})
        assert await until(lambda: daemon.stations.get("d") and
                           daemon.stations["d"].status == "up")
        with pytest.raises(kinds.CommandError, match="say which kind"):
            await daemon.do_command({"line": "x"})
        await daemon.do_command({"line": "only", "kind": "other", "id": "x7"})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["results"] == {"d": "did only"}
        assert daemon.run.meta["builds"]["other_local_latest"]["kind_type"] == "other"
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())


def test_firmware_rules_start_restart_and_leave_idle(stores):
    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three"})
        # No script, no rule: nothing runs.
        await asyncio.sleep(0.2)
        assert daemon.stations == {} and daemon.firmware == {}
        node = next(n for n in daemon.snapshot()["nodes"] if n["name"] == "a")
        assert node["firmware"] is None and node["status"] == "stopped"

        await daemon.do_firmware({"id": "f1", "rules": [
            {"which": {"where": {"tag": ["far"]}}, "firmware": STUB}]})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["id"] == "f1" and got["results"] == {"c": STUB}
        assert await until(lambda: daemon.stations.get("c") and
                           daemon.stations["c"].status == "up")
        assert set(daemon.stations) == {"c"}

        await daemon.do_firmware({"id": "f2", "rules": [
            {"which": {"all": True}, "firmware": STUB},
            {"which": {"names": ["c"]}, "firmware": "other_local_latest"}]})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["results"] == {"a": STUB, "b": STUB, "c": "other_local_latest"}
        assert await until(lambda: len(daemon.stations) == 3 and all(
            s.status == "up" for s in daemon.stations.values()))
        assert daemon.kind_of("c").name == "other"
        assert any(m["type"] == "notice" and "c now runs" in m["text"] for m in daemon.said)
        assert daemon.run.meta["firmware_rules"][0]["firmware"] == STUB

        await daemon.do_firmware({"id": "f3", "rules": [{"which": {"bogus": 1},
                                                         "firmware": STUB}]})
        got = [m for m in daemon.said if m["type"] == "command_result"][-1]
        assert got["id"] == "f3" and "selection" in got["error"]
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())


def test_a_role_the_station_forgets_is_said_again_after_a_reset(stores, monkeypatch):
    monkeypatch.setattr(Stub, "role_volatile", True)

    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three", **FAR})
        assert await until(lambda: daemon.stations.get("a") and daemon.stations["a"].status == "up")
        run = daemon.run
        assert lines_of(run, "a").count("role transport") == 1
        await daemon.do_node_reset({"name": "a"})
        # The role intent of its first-boot rules again, and nothing else of them.
        assert await until(lambda: lines_of(run, "a").count("role transport") == 2)
        assert lines_of(run, "a").count("name a 1") == 1          # not set up again
        assert lines_of(run, "a").count("radio up") == 1
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())


def test_a_snapshot_reloads_what_the_run_had(stores):
    async def go():
        daemon = make_simd(stores)
        await daemon.start_ether()
        await daemon.do_sim_load({"geodata": "flat", "nodeset": "three", **FAR})
        assert await until(lambda: len(daemon.stations) == 3)
        await daemon.do_nodeset_move({"name": "a", "lat": 0.02, "lon": 0.0})
        assert await until(lambda: not daemon.stale)
        await daemon.do_snapshot_save_as({"name": "moved"})
        first = daemon.run.dir
        await daemon.do_snapshot_load({"name": "moved"})
        assert daemon.run.dir == first + "-2"
        assert daemon.nodeset.node("a")["lat"] == 0.02
        assert daemon.first_boot == FAR["first_boot_rules"] and daemon.rules == [ALL_STUB]
        assert daemon.tables["868"].get("a", "b") == pytest.approx(
            slt.Table.read(os.path.join(first, "losses", "868.bin")).get("a", "b"))
        await daemon.stop_all(flush=False)
        daemon.ether.close()
    asyncio.run(go())
