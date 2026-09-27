"""Nodesets: the file, edits, the geometry hash, the extent, and the planner imports."""

import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import nodeset  # noqa: E402
import store  # noqa: E402

SAMPLE = """\
nodes:
  gw-alex: { id: 1, lat: 52.5219, lon: 13.4132, height_m: 38, height_from: roof, antenna: { gain_dbi: 5 }, device: stable, role: transport, radio: { freq_mhz: 869.525, sf: 8, bw_khz: 125, cr: 5, tx_dbm: 14, sync: 0x12, preamble: 18 }, tags: [gateway] }
  n017: { id: 2, lat: 52.5301, lon: 13.4018, height_m: 15, height_from: assumed, antenna: { gain_dbi: 2 }, device: dev, tags: [rooftop] }
offsets:
  - { between: [gw-alex, n017], db: 40, note: "wall, measured 2026-09-20" }
"""


@pytest.fixture
def nodesets_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "NODESETS_DIR", str(tmp_path))
    return tmp_path


def test_the_documented_example_reads_and_writes_back_unchanged(nodesets_dir):
    (nodesets_dir / "ex.yaml").write_text(SAMPLE)
    ns = nodeset.load("ex")
    alex = ns.node("gw-alex")
    assert alex["antenna"]["gain_dbi"] == 5 and alex["role"] == "transport"
    assert alex["radio"]["sync"] == 0x12 and alex["radio"]["sf"] == 8
    assert ns.node("n017")["role"] is None and ns.node("n017")["radio"] is None
    assert ns.offset("n017", "gw-alex") == 40
    assert nodeset.dump(ns.data) == SAMPLE
    assert ns.tags() == {"gateway": 1, "rooftop": 1}


def test_names_ids_roles_radios_and_heights_are_checked(tmp_path):
    for text, match in (
            ("nodes:\n  Bad_Name: { id: 1, lat: 0, lon: 0 }\n", "usable node name"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0 }\n  b: { id: 1, lat: 0, lon: 0 }\n", "share id"),
            ("nodes:\n  a: { id: 0, lat: 0, lon: 0 }\n", "a node id is"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, height_from: guessed }\n", "height_from"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, role: king }\n", "role is one of"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, radio: { power: 3 } }\n", "radio has no"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, radio: { sf: fast } }\n", "is a number"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0 }\noffsets:\n"
             "  - { between: [a, z], db: 3 }\n", "not a node")):
        path = tmp_path / "f.yaml"
        path.write_text(text)
        with pytest.raises(store.StoreError, match=match):
            nodeset.read(str(path))


def test_edits_mark_it_dirty_and_ids_are_the_lowest_free(nodesets_dir):
    ns = nodeset.create("new")
    assert ns.add_node("a", 0.0, 0.0)["id"] == 1
    assert ns.add_node("b", 0.1, 0.1, height_m=10, role="transport")["id"] == 2
    assert ns.add_node("c", 0.2, 0.2, device="dev", radio={"sf": 9})["id"] == 3
    assert ns.node("a")["device"] == nodeset.DEFAULT_DEVICE
    ns.remove_node("b")
    assert ns.next_id() == 2
    assert ns.dirty
    with pytest.raises(store.StoreError, match="already"):
        ns.add_node("a", 0, 0)
    with pytest.raises(store.StoreError, match="already c's"):
        ns.set_node("a", id=3)
    assert ns.set_node("a", id=7) is True
    assert ns.set_node("a", id=7) is False
    ns.set_node("c", role="client", tags=["x", "y", "x"])
    assert ns.node("c")["tags"] == ["x", "y"]
    ns.set_node("c", role="", radio={})
    assert ns.node("c")["role"] is None and ns.node("c")["radio"] is None
    ns.set_offset("a", "c", 12, "a wall")
    ns.rename_node("c", "charlie")
    assert ns.offset("a", "charlie") == 12
    ns.save()
    assert not ns.dirty
    again = nodeset.load("new")
    assert again.node("a")["id"] == 7 and "charlie" in again.nodes
    assert again.offsets[0]["note"] == "a wall"
    with pytest.raises(store.StoreError):
        ns.save_as("new")
    ns.save_as("other")
    assert nodeset.names() == ["new", "other"]


def test_geometry_hash_follows_positions_and_heights_only(nodesets_dir):
    (nodesets_dir / "ex.yaml").write_text(SAMPLE)
    ns = nodeset.load("ex")
    base = ns.geometry_hash()
    ns.set_node("n017", id=9, gain_dbi=7, device="stable", tags=["x"], height_from="measured",
                role="transport", radio={"sf": 12})
    ns.set_offset("gw-alex", "n017", 3)
    assert ns.geometry_hash() == base
    for change in (lambda n: n.move_node("n017", 52.5302, 13.4018),
                   lambda n: n.set_node("n017", height_m=16),
                   lambda n: n.add_node("n018", 52.53, 13.40),
                   lambda n: n.rename_node("n017", "n017b")):
        other = nodeset.load("ex")
        change(other)
        assert other.geometry_hash() != base


def test_a_nodeset_is_inside_an_extent_when_one_node_is(nodesets_dir):
    (nodesets_dir / "ex.yaml").write_text(SAMPLE)
    ns = nodeset.load("ex")
    assert ns.inside([13.3, 52.4, 13.5, 52.6])
    assert not ns.inside([-0.1, -0.1, 0.1, 0.1])
    summary = nodeset.summary("ex")
    assert summary["nodes"] == 2 and summary["bbox"] == [13.4018, 52.5219, 13.4132, 52.5301]


def test_sites_csv_import(tmp_path):
    path = tmp_path / "sites.csv"
    path.write_text("# lat,lon,tx_power_dbm,surface_masl,cs_neighbours\n"
                    "52.520000,13.405000,14,40,3\n52.530000,13.415000,20,45,2\n")
    data = nodeset.import_sites_csv(str(path), height_m=12)
    assert list(data["nodes"]) == ["site-001", "site-002"]
    node = data["nodes"]["site-002"]
    assert (node["id"], node["lat"], node["height_m"], node["height_from"]) == (2, 52.53, 12, "assumed")
    assert node["radio"]["tx_dbm"] == 20 and node["radio"]["freq_mhz"] == 869.525
    nodeset.parse(data, "import")


def test_nodes_csv_import(tmp_path):
    path = tmp_path / "nodes.csv"
    path.write_text(
        "id,name,kind,lat,lon,height_agl_m,tx_power_dbm,last_seen_unix\n"
        "02be91,B Fhain | rePeaterParke,repeater,52.524699,13.448100,,,1790333077\n"
        "02d4aa,B Fhain | rePeaterParke,repeater,52.534400,13.403800,22,17,1790300671\n"
        "0367ab,☀,repeater,52.456598,13.512300,,,1790532335\n"
        "0400ff,nowhere,repeater,,,,,1\n")
    data = nodeset.import_nodes_csv(str(path), height_m=15, device="dev")
    names = list(data["nodes"])
    assert names == ["b-fhain-repeaterparke", "b-fhain-repeaterparke-2", "n003"]
    first, second = (data["nodes"][n] for n in names[:2])
    assert (first["height_m"], first["height_from"]) == (15, "assumed")
    assert (second["height_m"], second["height_from"]) == (22, "measured")
    assert first["tags"] == ["repeater"] and first["device"] == "dev"
    assert first["radio"]["tx_dbm"] == 14 and second["radio"]["tx_dbm"] == 17
    nodeset.parse(data, "import")


def test_the_planners_own_deployed_network_csv_imports():
    path = os.path.join(HERE, "..", "..", "sergey", "planner", ".cache", "nodes", "berlin.csv")
    if not os.path.isfile(path):
        pytest.skip("no planner nodes CSV beside SIMesh")
    data = nodeset.import_nodes_csv(path)
    assert len(data["nodes"]) > 100
    nodeset.parse(data, "import")


def test_the_repositorys_nodesets_all_read():
    for name in nodeset.names():
        nodeset.load(name)
