"""Scripts, and what a station is told: the listing, the library's
declarations, selections, macros, declared settings and intents."""

import os
import re
import shutil
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import kinds  # noqa: E402
import nodeset  # noqa: E402
import script  # noqa: E402
import stations  # noqa: E402
import store  # noqa: E402
from simesh import library  # noqa: E402

RETICULOUS = {"ref": "stable", "kind_type": "reticulous", "elf": "/x", "name": "Reticulous",
              "virtual_hardware": "ESP32-S3"}
SERGEYCULUM = {"ref": "bm", "kind_type": "sergeyculum", "elf": "/y", "tools": {"rncfg": "/r"}}
RADIO = {"freq_mhz": 869.525, "sf": 8, "bw_khz": 125.0, "cr": 5, "tx_dbm": 14.0,
         "sync": 0x12, "preamble": 18}


@pytest.fixture
def scripts_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "SCRIPTS_DIR", str(tmp_path))
    return tmp_path


@pytest.fixture
def runtime(monkeypatch):
    """A fresh library runtime, with no simulation to reach."""
    fresh = library.Runtime()
    monkeypatch.setattr(library, "runtime", fresh)
    monkeypatch.delenv("SIMESH_SIM", raising=False)
    return fresh


def test_a_script_is_described_without_being_run(scripts_dir):
    (scripts_dir / "told.py").write_text(
        '"""Sets up, then reports.\n\nMore."""\nfrom simesh import *\nfrom simesh import traffic\n'
        'raise SystemExit("never run")\n'
        "def report(run_dir):\n    return '# done'\n")
    (scripts_dir / "broken.py").write_text("firmware('all',\n")
    assert script.names() == ["broken", "told"]
    got = script.describe(script.script_path("told"), "told")
    assert got["doc"] == "Sets up, then reports." and got["report"] is True
    assert [r["path"] for r in got["references"]] == ["simesh/library.py", "simesh/traffic.py"]
    assert all(r["library"] for r in got["references"])
    assert "line 1" in script.describe(script.script_path("broken"))["error"]
    assert script.read_reference("simesh/traffic.py").startswith('"""The LXMF traffic driver')
    with pytest.raises(store.StoreError):
        script.read_reference("../front.py")


def test_a_script_is_written_only_when_it_parses(scripts_dir):
    script.write("new", script.DEFAULT_TEXT, new=True)
    with pytest.raises(store.StoreError, match="already"):
        script.write("new", script.DEFAULT_TEXT, new=True)
    with pytest.raises(store.StoreError, match="line"):
        script.write("new", "def (:\n")
    assert script.read("new") == script.DEFAULT_TEXT
    with pytest.raises(store.StoreError, match="no script"):
        script.write("absent", "x = 1\n")


def test_a_scripts_definitions_are_read_without_running_it(scripts_dir, runtime):
    (scripts_dir / "one.py").write_text(
        "from simesh import *\nWHO = 'one'\nfirmware('all', 'x_y_latest')\nup('all')\n"
        "def report(run_dir):\n    return WHO + run_dir\n")
    module = script.module_of(script.script_path("one"), "one")
    assert module.report("/r") == "one/r"
    assert runtime.firmware_rules == [] and runtime.sim is None


def test_declarations_are_collected_until_the_script_does_something(scripts_dir, runtime):
    (scripts_dir / "fw.py").write_text(
        "from simesh import *\n"
        "time(10)\n"
        "firmware('all', 'reticulous_dev_latest')\n"
        "firmware(nodes(tag='sergeyculum'), 'sergeyculum_local_latest')\n"
        "on_first_boot(nodes(kind='reticulous'), '''\n"
        "    lora 0 txp {max_dbm}\n"
        "    # a comment\n"
        "    lxmf create {name}\n"
        "''')\n")
    script.run_file(script.script_path("fw"), "fw")
    assert runtime.time == "10x"
    assert runtime.firmware_rules == [
        {"which": {"all": True}, "firmware": "reticulous_dev_latest"},
        {"which": {"where": {"tag": "sergeyculum"}}, "firmware": "sergeyculum_local_latest"}]
    assert runtime.first_boot_rules == [
        {"which": {"where": {"kind": "reticulous"}},
         "lines": ["lora 0 txp {max_dbm}", "lxmf create {name}"]}]
    # Doing something needs a simulation, which this runtime has no way to.
    with pytest.raises(library.ScriptError, match="no simulation"):
        library.up("all")
    assert library.time_mode("max") == "max" and library.time_mode("0.5") == "0.5x"
    with pytest.raises(library.ScriptError):
        library.time_mode("fast")


def test_the_repositorys_scripts_all_parse_and_declare_what_they_run(runtime, monkeypatch,
                                                                    tmp_path):
    monkeypatch.syspath_prepend(store.SCRIPTS_DIR)
    # include() reads under the testbed: the repository's scripts, beside a
    # nodeset `gw` whose own setup adds a TCP peer.
    shutil.copytree(store.SCRIPTS_DIR, str(tmp_path / "scripts"))
    (tmp_path / "nodesets").mkdir()
    (tmp_path / "nodesets" / "gw.py").write_text(
        "from simesh import *\n"
        "on_first_boot(nodes(tag=\"tcp-peer\"), \"tcp peer add {addr:internet}:4965\")\n")
    monkeypatch.setattr(library, "TESTBED_DIR", str(tmp_path))
    names = script.names()
    helpers = {os.path.splitext(os.path.basename(ref["path"]))[0]
               for name in names for ref in script.describe(script.script_path(name))["references"]
               if not ref["library"]}
    assert {"startup", "globals"} <= helpers
    for name in names:
        info = script.describe(script.script_path(name), name)
        assert "error" not in info, info
        if name in helpers:
            continue                    # included or imported by the others
        fresh = library.Runtime()
        fresh.configure(geodata="berlin-city", nodesets=["gw"])
        library.runtime = fresh
        path = script.script_path(name)
        # Only the declarations: everything before the first thing done.
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        head = re.split(r"\n(?:traffic\.run|up)\(", text)[0]
        exec(compile(head, path, "exec"), {"__name__": "check_" + name})   # noqa: S102
        assert fresh.firmware_rules, "%s says no firmware" % name
        assert fresh.time in ("real", "max"), name
        # The startup script's rules came with it, in order: roles, radios,
        # gw's own, the radios up, and the script's own after.
        said = [line["intent"] if isinstance(line, dict) else line
                for rule in fresh.first_boot_rules for line in rule["lines"]]
        order = ["role", "radio", "tcp peer add {addr:internet}:4965", "radio_up"]
        assert [s for s in said if s in order] == order, name


def test_the_startup_script_sets_every_radio_but_the_no_radio_ones(runtime, monkeypatch):
    monkeypatch.syspath_prepend(store.SCRIPTS_DIR)
    runtime.configure(geodata="berlin-city", nodesets=["fachhochschule"])
    library.include("scripts/startup.py")          # fachhochschule has no setup of its own
    shared = script.shared()
    radio = [rule for rule in runtime.first_boot_rules
             if any(isinstance(l, dict) and l["intent"] == "radio" for l in rule["lines"])]
    assert radio == [{"which": {"not": {"where": {"tag": "no-radio"}}}, "lines": [
        {"intent": "radio", "args": {"freq_mhz": shared["FREQ_MHZ"], "sf": shared["SF"],
                                     "bw_khz": shared["BW_KHZ"], "cr": shared["CR"],
                                     "sync": 0x12, "tx_dbm": "max"}}]}]
    with pytest.raises(library.ScriptError, match="no file"):
        library.include("nodesets/nowhere.py")
    library.include("nodesets/nowhere.py", missing_ok=True)


def test_globals_shared_names_are_read_without_running_it(scripts_dir):
    (scripts_dir / "globals.py").write_text("FREQ_MHZ = 433.92\nSF = 9\nBW_KHZ = 250\nCR = 5\n"
                                            "import os\nOTHER = os.getcwd()\n")
    assert script.shared_radio() == {"freq_mhz": 433.92, "sf": 9, "bw_khz": 250.0, "cr": 5}
    (scripts_dir / "globals.py").write_text("FREQ_MHZ = 433.92\nSF = 9\n")
    with pytest.raises(store.StoreError, match="BW_KHZ"):
        script.shared()


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
    assert kinds.expand("lora 0 txp {max_dbm}", "gw", 1, ids, {"max_dbm": "27"}) == \
        "lora 0 txp 27"
    assert kinds.expand_all(["a {name}", "# c", "  ", "b"], "n", 3) == ["a n", "b"]


def test_the_name_role_and_radio_intents_in_each_kinds_own_lines():
    ret = kinds.make_kinds({"stable": RETICULOUS})["stable"]
    assert ret.lines("name") == ["hostname {name}"]
    assert ret.lines("role", role="transport") == ["set s.rnsd.transport_enabled 1"]
    assert ret.lines("radio", **RADIO) == [
        "lora 0 freq 869.525", "lora 0 sf 8", "lora 0 bw 125", "lora 0 cr 5", "lora 0 txp 14",
        "lora 0 sync 0x12", "lora 0 preamble 18"]
    assert ret.lines("radio_up") == ["lora up"]
    assert ret.label == "Reticulous (virtual ESP32-S3)"

    bm = kinds.make_kinds({"bm": SERGEYCULUM})["bm"]
    assert bm.rncfg == "/r"
    assert bm.lines("name") == ["name set {name}"]
    assert bm.lines("role", role="client") == ["transport off"]
    assert bm.lines("radio", **RADIO) == [
        "set --freq-hz 869525000 --sf 8 --bw-hz 125000 --cr 5 --txpower-dbm 14"]
    assert bm.lines("radio_up") == []

    # What a script writes for them: data simd turns into those lines.
    assert library.lines_of([library.role("transport"), "log rnsd debug",
                             library.radio(sf=9, tx_dbm="max")]) == [
        {"intent": "role", "args": {"role": "transport"}}, "log rnsd debug",
        {"intent": "radio", "args": {"sf": 9, "tx_dbm": "max"}}]


def test_intents_are_said_in_each_kinds_lines_or_refused():
    ret = kinds.make_kinds({"stable": RETICULOUS})["stable"]
    bm = kinds.make_kinds({"bm": SERGEYCULUM})["bm"]
    assert ret.lines("announce") == ["lora 0 a"]
    assert bm.lines("announce") == ["announce now"]
    assert ret.lines("message", dest="ab" * 16, text="hi there") == ["lxmf send %s hi there" % ("ab" * 16)]
    assert bm.lines("message", dest="cd" * 16, text="hi") == ["send %s hi" % ("cd" * 16)]
    assert ret.lines("peer_tcp", addr="127.0.0.5", port=4965) == ["tcp peer add 127.0.0.5:4965"]
    assert ret.lines("tx_power", dbm=10) == ["lora 0 txp 10"]
    assert bm.lines("tx_power", dbm=10.4) == ["set --txpower-dbm 10"]
    with pytest.raises(kinds.CommandError, match="sergeyculum station has no way to path"):
        bm.lines("path", dest="x")
    with pytest.raises(kinds.CommandError, match="no such intent"):
        ret.lines("dance")


def test_an_unknown_device_kind_is_named():
    with pytest.raises(kinds.CommandError, match="kind 'meshcore'"):
        kinds.make_kinds({"mc": {"kind_type": "meshcore"}})


# ---- selections ------------------------------------------------------------------

FACTS = {
    "gw": {"id": 1, "tags": ["lora", "tcp-peer"], "role": "transport", "height_m": 30,
           "firmware": "reticulous_dev_latest", "kind": "reticulous"},
    "n2": {"id": 2, "tags": ["lora"], "role": "client", "height_m": 2,
           "firmware": "reticulous_dev_latest", "kind": "reticulous"},
    "sg": {"id": 3, "tags": ["sergeyculum"], "role": "client", "height_m": 2,
           "firmware": "sergeyculum_local_latest", "kind": "sergeyculum"},
}


def test_a_selection_is_set_algebra_over_the_nodes_facts():
    from simesh import nodes
    from simesh.select import Nodes, which

    assert nodes().pick(FACTS) == ["gw", "n2", "sg"]
    assert nodes(tag="lora").pick(FACTS) == ["gw", "n2"]
    assert nodes(tag="lora", role="transport").pick(FACTS) == ["gw"]
    assert nodes(tag=("tcp-peer", "sergeyculum")).pick(FACTS) == ["gw", "sg"]
    assert (nodes(tag="lora") | nodes(kind="sergeyculum")).pick(FACTS) == ["gw", "n2", "sg"]
    assert (nodes(tag="lora") - nodes(role="client")).pick(FACTS) == ["gw"]
    assert (~nodes(kind="reticulous")).pick(FACTS) == ["sg"]
    assert (nodes(tag="lora") ^ nodes(role="client")).pick(FACTS) == ["gw", "sg"]
    assert nodes(height_m=lambda h: h > 20).pick(FACTS) == ["gw"]
    assert which("all").pick(FACTS) == ["gw", "n2", "sg"]
    assert which("sg").pick(FACTS) == ["sg"] and which(["n2", "gw"]).pick(FACTS) == ["gw", "n2"]
    assert (nodes(tag="lora") & "n2").pick(FACTS) == ["n2"]
    tree = (nodes(tag="lora") - nodes(name="n2")).to_json()
    assert Nodes.from_json(tree).pick(FACTS) == ["gw"]
    with pytest.raises(ValueError, match="cannot travel"):
        nodes(height_m=lambda h: h > 20).to_json()
    with pytest.raises(ValueError):
        Nodes.from_json({"where": {"colour": "red"}})
    with pytest.raises(TypeError, match="no field"):
        nodes(colour="red")
    assert library.lines_of(["a", "b\nc"]) == ["a", "b", "c"]
