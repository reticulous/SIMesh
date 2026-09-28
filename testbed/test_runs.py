"""Runs and snapshots: laying a run out, keeping a moment, and starting from it."""

import asyncio
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "ether"))

import geodata  # noqa: E402
import losses  # noqa: E402
import nodeset  # noqa: E402
import runs  # noqa: E402
import script  # noqa: E402
import slt  # noqa: E402
import store  # noqa: E402

SETUP = "async def setup(node):\n    await node.run('hello {name}')\n"
GLOBALS = "FREQ_MHZ = 433.92\nSF = 9\nBW_KHZ = 125\nCR = 5\n"


@pytest.fixture
def setup(tmp_path, monkeypatch):
    for key, sub in (("GEODATA_DIR", "geodata"), ("NODESETS_DIR", "nodesets"),
                     ("LOSSES_DIR", "losses"), ("SCRIPTS_DIR", "scripts"),
                     ("RUNS_DIR", "runs"), ("SNAPSHOTS_DIR", "snapshots")):
        monkeypatch.setattr(store, key, str(tmp_path / sub))
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"exponent": 2.7}})
    ns = nodeset.create("pair")
    ns.add_node("a", 0.0, 0.0, tags=["transport"])
    ns.add_node("b", 0.006, 0.0)
    ns.save()
    os.makedirs(store.SCRIPTS_DIR)
    (tmp_path / "scripts" / "hello.py").write_text(SETUP)
    (tmp_path / "scripts" / "globals.py").write_text(GLOBALS)
    gd, ns = geodata.load("flat"), nodeset.load("pair")
    path, _ = asyncio.run(losses.compute(gd, ns, "868"))
    run = runs.create_run(runs.run_path("r1"), gd, ns, "hello", "max", {"868": path},
                          builds={"stable": {"stamp": "20260925"}})
    return run


def test_a_run_holds_its_own_copies(setup):
    run = setup
    assert runs.runs() == ["r1"]
    assert run.meta["geodata"] == "flat" and run.meta["nodeset"] == "pair"
    assert run.meta["script"] == "hello"
    assert run.meta["time"] == "max" and run.meta["builds"] == {"stable": {"stamp": "20260925"}}
    assert run.bands() == ["868"]
    assert slt.Table.read(run.table_path("868")).names == ["a", "b"]
    assert run.geodata().exponent == 2.7
    with open(run.script_path) as handle:
        assert handle.read() == SETUP
    # globals.py as it was when the run began, whatever the store's says later.
    assert run.radio() == {"freq_mhz": 433.92, "sf": 9, "bw_khz": 125.0, "cr": 5}
    with open(os.path.join(store.SCRIPTS_DIR, "globals.py"), "w") as handle:
        handle.write(GLOBALS.replace("SF = 9", "SF = 12"))
    assert run.radio()["sf"] == 9
    ns = run.nodeset()
    assert nodeset.tag_role(ns.node("a")["tags"]) == "transport"
    # The run's nodeset is its own: editing it leaves nodesets/ alone.
    ns.move_node("b", 0.01, 0.0)
    ns.save()
    assert nodeset.load("pair").node("b")["lat"] == 0.006
    run.log_move(12.5, "b", 0.01, 0.0)
    assert run.edits() == [{"t": 12.5, "what": "move", "node": "b", "lat": 0.01, "lon": 0.0}]
    with pytest.raises(store.StoreError):
        runs.create_run(run.dir, run.geodata(), ns, None, "real", {})


def test_a_paused_run_resumes_as_it_ended(setup):
    run = setup
    state = os.path.join(run.node_dir("a"), "state")
    os.makedirs(state)
    with open(os.path.join(state, "identity"), "w") as handle:
        handle.write("secret")
    assert run.paused is None and runs.paused_runs() == []
    runs.pause_run(run, "town", 7_200_000_000)
    assert run.paused["simulation"] == "town" and run.paused["t"] == 7_200_000_000
    assert [r.name for r in runs.paused_runs()] == ["r1"]
    assert runs.snapshots() == []           # kept in the run, not with the snapshots

    back = runs.resume_run(run, runs.run_path("r1-2"), "max")
    assert back.meta["resumed_from"] == "r1" and back.meta["snapshot"] is None
    assert back.meta["script"] == "hello" and back.paused is None
    with open(os.path.join(back.node_dir("a"), "state", "identity")) as handle:
        assert handle.read() == "secret"
    assert run.meta["resumed"] == "r1-2" and runs.paused_runs() == []
    assert runs.open_run(run.dir).paused is None

    runs.pause_run(back, "town", 1)
    assert [r.name for r in runs.paused_runs()] == ["r1-2"]
    runs.delete_run(back)
    assert runs.paused_runs() == [] and runs.runs() == ["r1"]


def test_a_run_without_a_script_has_none(setup):
    gd, ns = geodata.load("flat"), nodeset.load("pair")
    run = runs.create_run(runs.run_path("bare"), gd, ns, None, "real", {})
    assert run.script_path is None and run.meta["script"] is None


def test_a_snapshot_comes_back_without_recompute(setup, monkeypatch):
    run = setup
    state = os.path.join(run.node_dir("a"), "state")
    os.makedirs(state)
    with open(os.path.join(state, "identity"), "w") as handle:
        handle.write("secret")
    os.makedirs(os.path.join(run.node_dir("b")))
    with open(os.path.join(run.node_dir("b"), "log"), "w") as handle:
        handle.write("a log is not state")
    target = runs.save_snapshot(run, "moment", 3600.0)
    assert runs.snapshots() == ["moment"]
    for name in ("geodata.yaml", "nodeset.yaml", "script.py", "snapshot.yaml",
                 os.path.join("losses", "868.bin"), os.path.join("nodes", "a", "state", "identity")):
        assert os.path.isfile(os.path.join(target, name)), name
    assert not os.path.exists(os.path.join(target, "nodes", "b"))
    info = runs.snapshot_info("moment")
    assert info["t"] == 3600.0 and info["script"] == "hello"
    with pytest.raises(store.StoreError):
        runs.save_snapshot(run, "moment", 1.0)

    # Loading it asks nothing of the loss code, and the script is its own copy.
    def refuse(*args, **kwargs):
        raise AssertionError("a snapshot load recomputed a table")
    monkeypatch.setattr(losses, "compute", refuse)
    monkeypatch.setattr(losses, "build", refuse)
    os.remove(script.script_path("hello"))
    back = runs.load_snapshot("moment", runs.run_path("r2"), "real")
    assert back.meta["snapshot"] == "moment" and back.meta["snapshot_t"] == 3600.0
    assert back.meta["builds"] == {"stable": {"stamp": "20260925"}}
    assert back.meta["script"] == "hello"
    with open(back.script_path) as handle:
        assert handle.read() == SETUP
    with open(os.path.join(back.node_dir("a"), "state", "identity")) as handle:
        assert handle.read() == "secret"
    assert slt.Table.read(back.table_path("868")).get("a", "b") == \
        slt.Table.read(run.table_path("868")).get("a", "b")


def test_copy_state_takes_state_only_and_clears_the_rest(tmp_path):
    src, dst = tmp_path / "src", tmp_path / "dst"
    (src / "nodes" / "a" / "state").mkdir(parents=True)
    (src / "nodes" / "a" / "state" / "k").write_text("1")
    (src / "nodes" / "a" / "log").write_text("log")
    (dst / "nodes" / "old" / "state").mkdir(parents=True)
    runs.copy_state(str(src), str(dst), wanted=["a"])
    assert (dst / "nodes" / "a" / "state" / "k").read_text() == "1"
    assert not (dst / "nodes" / "a" / "log").exists()
    assert not (dst / "nodes" / "old" / "state").exists()
    runs.wipe_state(str(dst))
    assert not (dst / "nodes" / "a" / "state").exists()
