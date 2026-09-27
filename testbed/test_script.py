"""Scripts, and what a station is told: macros, declared settings, intents,
and a script's setup as it sees a station."""

import asyncio
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import kinds  # noqa: E402
import nodeset  # noqa: E402
import script  # noqa: E402
import stations  # noqa: E402
import store  # noqa: E402
from simesh import setup as station_setup  # noqa: E402 - not `setup_module`, a pytest hook

RETICULOUS = {"ref": "stable", "kind_type": "reticulous", "elf": "/x", "name": "Reticulous",
              "stands_for": "ESP32"}
BERLINMESH = {"ref": "bm", "kind_type": "berlinmesh", "elf": "/y", "tools": {"rncfg": "/r"}}
RADIO = {"freq_mhz": 869.525, "sf": 8, "bw_khz": 125.0, "cr": 5, "tx_dbm": 14.0,
         "sync": 0x12, "preamble": 18}


@pytest.fixture
def scripts_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "SCRIPTS_DIR", str(tmp_path))
    return tmp_path


def test_a_script_is_described_without_being_run(scripts_dir):
    (scripts_dir / "both.py").write_text(
        '"""Sets up, then drives.\n\nMore."""\nraise SystemExit("never run")\n'
        "async def setup(node):\n    pass\nasync def main(sim):\n    pass\n")
    (scripts_dir / "broken.py").write_text("async def setup(node:\n")
    (scripts_dir / "plain.py").write_text("def setup(node):\n    pass\n")
    assert script.names() == ["both", "broken", "plain"]
    got = script.describe(script.script_path("both"), "both")
    assert got == {"name": "both", "doc": "Sets up, then drives.", "setup": True, "main": True,
                   "report": False}
    (scripts_dir / "told.py").write_text("async def main(sim):\n    pass\n"
                                         "def report(run_dir):\n    return '# done'\n")
    assert script.describe(script.script_path("told"))["report"] is True
    assert "line 1" in script.describe(script.script_path("broken"))["error"]
    assert "async def setup" in script.describe(script.script_path("plain"))["error"]


def test_a_script_is_written_only_when_it_parses(scripts_dir):
    script.write("new", script.DEFAULT_TEXT, new=True)
    with pytest.raises(store.StoreError, match="already"):
        script.write("new", script.DEFAULT_TEXT, new=True)
    with pytest.raises(store.StoreError, match="line"):
        script.write("new", "def (:\n")
    assert script.read("new") == script.DEFAULT_TEXT
    with pytest.raises(store.StoreError, match="no script"):
        script.write("absent", "x = 1\n")


def test_a_script_loads_as_a_module_of_its_own(scripts_dir):
    (scripts_dir / "one.py").write_text("import simesh\nWHO = 'one'\nasync def setup(node):\n"
                                        "    return WHO\n")
    module = script.load(script.script_path("one"), "one")
    assert asyncio.run(module.setup(None)) == "one"
    (scripts_dir / "bad.py").write_text("raise ValueError('no')\n")
    with pytest.raises(store.StoreError, match="would not load"):
        script.load(script.script_path("bad"))


def test_the_repositorys_scripts_all_parse_and_load():
    for name in script.names():
        info = script.describe(script.script_path(name), name)
        assert "error" not in info, info
        assert info["setup"] or info["main"], name
        script.load(script.script_path(name), name)


def test_macros_fill_name_id_and_addresses_and_leave_the_rest(monkeypatch):
    monkeypatch.setattr(stations, "NET", "127.16.0.0/22")
    ids = {"gw": 1, "leaf": 2}
    assert kinds.expand("hostname {name}", "gw", 1) == "hostname gw"
    assert kinds.expand("addr {addr} id {id} {unknown}", "gw", 1, ids) == \
        "addr 127.16.0.5 id 1 {unknown}"
    assert kinds.expand("tcp peer add {addr:leaf}:4965", "gw", 1, ids) == \
        "tcp peer add 127.16.0.6:4965"
    assert kinds.expand("x {addr:ghost} {name:leaf}", "gw", 1, ids) == \
        "x {addr:ghost} {name:leaf}"
    assert kinds.expand_all(["a {name}", "# c", "  ", "b"], "n", 3) == ["a n", "b"]


def test_declared_settings_in_each_kinds_own_lines():
    ret = kinds.make_kinds({"stable": RETICULOUS})["stable"]
    node = nodeset.node_record(1, 0, 0, role="transport", radio=RADIO)
    assert ret.declared(node) == [
        "hostname {name}", "set s.rnsd.transport_enabled 1", "lora 0 freq 869.525",
        "lora 0 sf 8", "lora 0 bw 125", "lora 0 cr 5", "lora 0 txp 14", "lora 0 sync 0x12",
        "lora 0 preamble 18"]
    assert ret.radio_start() == ["lora up"]
    assert ret.lines("radio", sf=9) == ["lora 0 sf 9", "lora up"]
    assert ret.declared(nodeset.node_record(2, 0, 0)) == ["hostname {name}"]
    assert ret.label == "Reticulous (virtual ESP32)"

    bm = kinds.make_kinds({"bm": BERLINMESH})["bm"]
    assert bm.rncfg == "/r"
    assert bm.declared(dict(node, role="client")) == [
        "name set {name}", "transport off",
        "set --freq-hz 869525000 --sf 8 --bw-hz 125000 --cr 5 --txpower-dbm 14"]
    assert bm.radio_start() == []


def test_intents_are_said_in_each_kinds_lines_or_refused():
    ret = kinds.make_kinds({"stable": RETICULOUS})["stable"]
    bm = kinds.make_kinds({"bm": BERLINMESH})["bm"]
    assert ret.lines("announce") == ["lora 0 a"]
    assert bm.lines("announce") == ["announce now"]
    assert ret.lines("message", dest="ab" * 16, text="hi there") == ["lxmf send %s hi there" % ("ab" * 16)]
    assert bm.lines("message", dest="cd" * 16, text="hi") == ["send %s hi" % ("cd" * 16)]
    assert ret.lines("peer_tcp", addr="127.0.0.5", port=4965) == ["tcp peer add 127.0.0.5:4965"]
    with pytest.raises(kinds.CommandError, match="berlinmesh station has no way to path"):
        bm.lines("path", dest="x")
    with pytest.raises(kinds.CommandError, match="no such intent"):
        ret.lines("dance")


def test_an_unknown_device_kind_is_named():
    with pytest.raises(kinds.CommandError, match="kind 'meshcore'"):
        kinds.make_kinds({"mc": {"kind_type": "meshcore"}})


def test_a_setup_node_runs_lines_and_intents_through_its_station():
    said = []

    async def run(line):
        said.append(line)
        return "ok %s\n" % line

    def lines(verb, **args):
        if verb == "role":
            return ["role %s" % args["role"]]
        raise station_setup.Refused("no way to %s" % verb)

    record = nodeset.node_record(4, 0, 0, tags=["a"], role="client", radio={"sf": 7})
    node = station_setup.Node("n4", record, "reticulous", run, lines, lambda other: "addr-" + other)
    assert (node.name, node.id, node.tags, node.kind, node.radio) == \
        ("n4", 4, ["a"], "reticulous", {"sf": 7})
    assert node.addr() == "addr-n4" and node.addr("gw") == "addr-gw"

    async def go():
        assert await node.run("x") == "ok x\n"
        assert await node.set_role("transport") == "ok role transport"
        with pytest.raises(station_setup.Refused):
            await node.announce()
    asyncio.run(go())
    assert said == ["x", "role transport"]
