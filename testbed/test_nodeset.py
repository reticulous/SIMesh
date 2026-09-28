"""Nodesets: the file, edits, the geometry hash, the extent, and the planner imports."""

import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import boards  # noqa: E402
import nodeset  # noqa: E402
import store  # noqa: E402

SAMPLE = """\
nodes:
  gw-alex: { id: 1, lat: 52.5219, lon: 13.4132, height_m: 38, height_from: roof, max_dbm: 27, antenna: { type: panel_directional, azimuth_deg: 120, elevation_deg: -2 }, tags: [transport, gateway] }
  n017: { id: 2, lat: 52.5301, lon: 13.4018, height_m: 15, height_from: assumed, antenna: { type: whip_sma_quarter_wave }, tags: [rooftop, no-radio] }
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
    assert alex["antenna"] == {"type": "panel_directional", "azimuth_deg": 120,
                               "elevation_deg": -2}
    # A role is a tag; a node without a radio says so with one.
    assert nodeset.tag_role(alex["tags"]) == "transport"
    assert nodeset.tag_role(ns.node("n017")["tags"]) is None
    assert nodeset.has_radio(alex) and not nodeset.has_radio(ns.node("n017"))
    # A node's own maximum; one that states none sends at most 22 dBm.
    assert boards.max_dbm(alex["max_dbm"]) == 27
    assert "max_dbm" not in ns.node("n017") and boards.max_dbm(None) == 22
    # What a station is told: flat, the node's maximum, and above 22 dBm the
    # GC1109 front end's figures.
    told = json.loads(boards.environment(alex["max_dbm"]))
    assert told["chip"] == "sx1262" and told["max_dbm"] == 27 and told["fem_part"] == "gc1109"
    assert told["fem_tx_cal"].startswith("gc1109 measured 1:7,")
    assert told["fem_rx_gain_db"] == 20 and told["fem_gain_db"] == 0
    assert json.loads(boards.environment(None)) == {"chip": "sx1262", "max_dbm": 22}
    assert json.loads(boards.environment(22)) == {"chip": "sx1262", "max_dbm": 22}
    assert json.loads(boards.environment(14)) == {"chip": "sx1262", "max_dbm": 14}
    assert json.loads(boards.environment(23))["fem_part"] == "gc1109"
    assert ns.offset("n017", "gw-alex") == 40
    assert nodeset.dump(ns.data) == SAMPLE
    assert ns.tags() == {"gateway": 1, "no-radio": 1, "rooftop": 1, "transport": 1}


def test_names_ids_and_heights_are_checked_and_nodes_declare_nothing(tmp_path):
    for text, match in (
            ("nodes:\n  Bad_Name: { id: 1, lat: 0, lon: 0 }\n", "usable node name"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0 }\n  b: { id: 1, lat: 0, lon: 0 }\n", "share id"),
            ("nodes:\n  a: { id: 0, lat: 0, lon: 0 }\n", "a node id is"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, height_from: guessed }\n", "height_from"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, role: transport }\n", "declares no role"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, radio: { sf: 8 } }\n", "declares no radio"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, device: dev }\n", "firmware\\(\\)"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, board: { type: heltec_v4 } }\n",
             "has no board.*max_dbm"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, max_dbm: 30 }\n", "max_dbm is -9 to 27"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, max_dbm: loud }\n", "max_dbm is a number"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, antenna: { type: dish } }\n", "no antenna"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0, antenna: { gain_dbi: 2 } }\n",
             "antenna has no"),
            ("nodes:\n  a: { id: 1, lat: 0, lon: 0 }\noffsets:\n"
             "  - { between: [a, z], db: 3 }\n", "not a node")):
        path = tmp_path / "f.yaml"
        path.write_text(text)
        with pytest.raises(store.StoreError, match=match):
            nodeset.read(str(path))


def test_edits_mark_it_dirty_and_ids_are_the_lowest_free(nodesets_dir):
    ns = nodeset.create("new")
    assert ns.add_node("a", 0.0, 0.0)["id"] == 1
    assert ns.add_node("b", 0.1, 0.1, height_m=10, tags=["transport"])["id"] == 2
    assert ns.add_node("c", 0.2, 0.2,
                       antenna={"type": "yagi_directional", "azimuth_deg": 370})["id"] == 3
    assert ns.node("a")["antenna"] == {"type": "whip_sma_quarter_wave"}
    assert ns.node("c")["antenna"] == {"type": "yagi_directional", "azimuth_deg": 10.0,
                                       "elevation_deg": 0.0}
    ns.remove_node("b")
    assert ns.next_id() == 2
    assert ns.dirty
    with pytest.raises(store.StoreError, match="already"):
        ns.add_node("a", 0, 0)
    with pytest.raises(store.StoreError, match="already c's"):
        ns.set_node("a", id=3)
    assert ns.set_node("a", id=7) is True
    assert ns.set_node("a", id=7) is False
    ns.set_node("c", tags=["x", "y", "x"])
    assert ns.node("c")["tags"] == ["x", "y"]
    # A maximum power stays until a null clears it.
    ns.set_node("c", max_dbm=24)
    ns.set_node("c", height_m=3)
    assert ns.node("c")["max_dbm"] == 24
    ns.set_node("c", max_dbm=None)
    assert "max_dbm" not in ns.node("c")
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
    ns.set_node("n017", id=9, antenna={"type": "rubber_duck"}, tags=["x"], height_from="measured",
                max_dbm=27)
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
    # The file's transmit power is the node's maximum.
    assert node["max_dbm"] == 20
    nodeset.parse(data, "import")


def test_nodes_csv_import(tmp_path):
    path = tmp_path / "nodes.csv"
    path.write_text(
        "id,name,kind,lat,lon,height_agl_m,tx_power_dbm,last_seen_unix\n"
        "02be91,B Fhain | rePeaterParke,repeater,52.524699,13.448100,,,1790333077\n"
        "02d4aa,B Fhain | rePeaterParke,repeater,52.534400,13.403800,22,17,1790300671\n"
        "0367ab,☀,repeater,52.456598,13.512300,,,1790532335\n"
        "0400ff,nowhere,repeater,,,,,1\n")
    data = nodeset.import_nodes_csv(str(path), height_m=15)
    names = list(data["nodes"])
    assert names == ["b-fhain-repeaterparke", "b-fhain-repeaterparke-2", "n003"]
    first, second = (data["nodes"][n] for n in names[:2])
    assert (first["height_m"], first["height_from"]) == (15, "assumed")
    assert (second["height_m"], second["height_from"]) == (22, "measured")
    assert first["tags"] == ["repeater"] and first["antenna"]["type"] == "whip_sma_quarter_wave"
    assert "max_dbm" not in first
    assert second["max_dbm"] == 17
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


def layer(*nodes, offsets=()):
    return {"nodes": {name: {"id": node_id, "lat": lat, "lon": lon, "tags": tags}
                      for name, node_id, lat, lon, tags in nodes},
            "offsets": [{"between": list(pair), "db": db} for pair, db in offsets]}


def test_one_shown_layer_saves_as_itself():
    got = nodeset.merge([("town", layer(("a", 3, 52.5, 13.4, ["x"])))])
    assert list(got["nodes"]) == ["a"] and got["nodes"]["a"]["tags"] == ["x"]
    assert got["nodes"]["a"]["id"] == 3


def test_shown_layers_merge_top_first():
    town = layer(("gw", 1, 52.5000, 13.4000, ["lxmf"]), ("hill", 2, 52.5100, 13.4100, []),
                 offsets=[(("gw", "hill"), 12)])
    # 3 m north of gw, another name for the same mast: the town's gw it is.
    meshcore = layer(("mast", 1, 52.500027, 13.4000, ["repeater"]),
                     ("gw", 2, 52.5200, 13.4200, []), ("far", 7, 52.5300, 13.4300, []),
                     offsets=[(("mast", "far"), 3), (("gw", "far"), 5)])
    got = nodeset.merge([("town", town), ("meshcore", meshcore)])
    nodes = got["nodes"]
    assert sorted(nodes) == ["far", "gw", "gw-meshcore", "hill"]
    assert nodes["gw"]["tags"] == ["lxmf", "town"]
    assert nodes["gw-meshcore"]["tags"] == ["meshcore"] and nodes["gw-meshcore"]["lat"] == 52.52
    # Ids 1 and 2 were the town's: meshcore's gw takes the lowest free, far keeps its 7.
    assert nodes["gw-meshcore"]["id"] == 3 and nodes["far"]["id"] == 7
    assert sorted(n["id"] for n in nodes.values()) == [1, 2, 3, 7]
    # The mast's offset went with the mast; the others came along, renamed.
    assert {(tuple(o["between"]), o["db"]) for o in got["offsets"]} == {
        (("gw", "hill"), 12), (("gw-meshcore", "far"), 5)}
    # Two nodes of one layer stay two, however close.
    same = layer(("a", 1, 52.5, 13.4, []), ("b", 2, 52.50001, 13.4, []))
    assert sorted(nodeset.merge([("one", same), ("two", layer())])["nodes"]) == ["a", "b"]
    # A name taken twice over gets a number too.
    got = nodeset.merge([("a", layer(("n", 1, 1, 1, []), ("n-b", 2, 2, 2, []))),
                         ("b", layer(("n", 1, 3, 3, [])))])
    assert sorted(got["nodes"]) == ["n", "n-b", "n-b-2"]


def test_a_delete_takes_the_nodesets_own_setup_with_it(nodesets_dir):
    (nodesets_dir / "ex.yaml").write_text(SAMPLE)
    (nodesets_dir / "ex.py").write_text("# its own setup\n")
    assert nodeset.load("ex").inside([13.3, 52.4, 13.5, 52.6]) == len(nodeset.load("ex").nodes)
    nodeset.delete("ex")
    assert nodeset.names() == [] and not (nodesets_dir / "ex.py").exists()
    with pytest.raises(store.StoreError, match="no nodeset"):
        nodeset.delete("ex")
