"""Geodata: the file, the projections between degrees and metres, the
extent, and importing a pack."""

import json
import os
import sys
import zipfile

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import geodata  # noqa: E402
import store  # noqa: E402

# Corners of the berlin-city pack as planner-web's /api/pack states them:
# its extent in EPSG:32633 and the same points in WGS84, by the planner's
# own proj4rs.
BERLIN_CORNERS = [
    ((377598.949424815, 5808681.7827512985), (52.414648212657944, 13.20029149508144)),
    ((405178.949424815, 5829291.7827512985), (52.60535686371495, 13.599784967616484)),
]


def test_utm_matches_the_planner_both_ways():
    tm = geodata.TransverseMercator(*geodata.utm_zone_of(32633))
    for (x, y), (lat, lon) in BERLIN_CORNERS:
        got_lat, got_lon = tm.inverse(x, y)
        assert got_lat == pytest.approx(lat, abs=1e-9)
        assert got_lon == pytest.approx(lon, abs=1e-9)
        got_x, got_y = tm.forward(lat, lon)
        assert got_x == pytest.approx(x, abs=1e-3)
        assert got_y == pytest.approx(y, abs=1e-3)


def test_utm_south_and_etrs89_zones_round_trip():
    for epsg, point in ((32733, (-33.9249, 18.4241)), (25833, (52.52, 13.405)),
                        (32601, (10.0, -177.5))):
        tm = geodata.TransverseMercator(*geodata.utm_zone_of(epsg))
        lat, lon = tm.inverse(*tm.forward(*point))
        assert (lat, lon) == pytest.approx(point, abs=1e-10)
    with pytest.raises(store.StoreError):
        geodata.utm_zone_of(4326)


def manifest(name="tiny", epsg=32633):
    return {"name": name, "region": {"bbox": [13.3, 52.4, 13.5, 52.6], "crs_epsg": epsg},
            "layers": ["TerrainDtm", {"kind": "Roads"}]}


def write_pack(tmp_path, epsg=32633):
    pack = tmp_path / "packs" / "tiny"
    pack.mkdir(parents=True)
    (pack / "manifest.json").write_text(json.dumps(manifest(epsg=epsg)))
    return pack


def test_a_pack_reads_its_manifest_relative_to_the_geodata_file(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    pack = write_pack(tmp_path)
    geodata.write(geodata.geodata_path("tiny"), {"pack": "../packs/tiny"})
    gd = geodata.load("tiny")
    assert gd.is_pack and gd.crs_epsg == 32633
    assert gd.pack_dir == str(pack)
    assert len(gd.pack_manifest_hash) == 64
    x, y = gd.to_xy(52.52, 13.405)
    assert gd.to_latlon(x, y) == pytest.approx((52.52, 13.405), abs=1e-10)
    assert gd.as_dict()["layers"] == ["TerrainDtm", "Roads"]
    assert gd.bbox == [13.3, 52.4, 13.5, 52.6]
    assert gd.holds(52.5, 13.4) and not gd.holds(0, 0)
    before = gd.content_hash
    (pack / "manifest.json").write_text((pack / "manifest.json").read_text() + " ")
    assert geodata.load("tiny").content_hash != before


def test_a_pack_without_its_pack_is_refused(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path))
    geodata.write(geodata.geodata_path("gone"), {"pack": "../nowhere"})
    with pytest.raises(store.StoreError, match="manifest.json"):
        geodata.load("gone")


def test_synthetic_ground_is_a_nautical_mile_to_the_minute_at_zero(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path))
    geodata.write(geodata.geodata_path("flat"),
                  {"synthetic": {"exponent": 3.5, "extent_m": 10000}})
    gd = geodata.load("flat")
    assert not gd.is_pack and gd.exponent == 3.5 and gd.terrain == "flat"
    assert gd.to_xy(1 / 60, 2 / 60) == pytest.approx((2 * 1852.0, 1852.0))
    assert gd.to_latlon(*gd.to_xy(0.01, -0.02)) == pytest.approx((0.01, -0.02))
    half = 5000 / 1852.0 / 60
    assert gd.bbox == pytest.approx([-half, -half, half, half])
    assert gd.origin == pytest.approx((0, 0))
    assert gd.holds(0.04, -0.04) and not gd.holds(0.05, 0)
    assert geodata.read(geodata.geodata_path("flat")).data == gd.data


def test_a_geodata_file_must_say_what_it_is(tmp_path):
    path = tmp_path / "odd.yaml"
    path.write_text("colour: blue\n")
    with pytest.raises(store.StoreError):
        geodata.read(str(path))
    path.write_text("synthetic: { terrain: mountains }\n")
    with pytest.raises(store.StoreError, match="terrain"):
        geodata.read(str(path))
    with pytest.raises(store.StoreError):
        geodata.geodata_path("Not A Name")


def test_a_copied_pack_still_names_its_pack(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    pack = write_pack(tmp_path)
    geodata.write(geodata.geodata_path("tiny"), {"pack": "../packs/tiny"})
    target = tmp_path / "runs" / "a" / "geodata.yaml"
    geodata.write_copy(geodata.load("tiny"), str(target))
    assert geodata.read(str(target), "tiny").pack_dir == str(pack)
    text = geodata.rebase_text(target.read_text(), str(target.parent), str(tmp_path / "elsewhere"))
    assert "packs/tiny" in text


def make_zip(path, members):
    with zipfile.ZipFile(path, "w") as zf:
        for name, text in members.items():
            zf.writestr(name, text)


def test_a_pack_zip_is_imported_into_the_packs_and_named(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    packs = tmp_path / "planner" / "packs"
    zipped = tmp_path / "up.zip"
    make_zip(zipped, {"berlin/manifest.json": json.dumps(manifest("berlin")),
                      "berlin/terrain/a.bin": "x"})
    gd = geodata.import_pack(str(zipped), "berlin", str(packs))
    assert gd.is_pack and gd.pack_dir == str(packs / "berlin")
    assert (packs / "berlin" / "terrain" / "a.bin").read_text() == "x"
    with pytest.raises(store.StoreError, match="already"):
        geodata.import_pack(str(zipped), "berlin", str(packs))


def test_a_pack_zip_without_a_usable_manifest_leaves_nothing(tmp_path, monkeypatch):
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    packs = tmp_path / "packs"
    zipped = tmp_path / "up.zip"
    make_zip(zipped, {"a/b.bin": "x"})
    with pytest.raises(store.StoreError, match="manifest"):
        geodata.import_pack(str(zipped), "nothing", str(packs))
    make_zip(zipped, {"manifest.json": json.dumps({"region": {"crs_epsg": 4326, "bbox": [0, 0, 1, 1]}})})
    with pytest.raises(store.StoreError, match="not usable"):
        geodata.import_pack(str(zipped), "nothing", str(packs))
    make_zip(zipped, {"manifest.json": "{}", "../escape": "x"})
    with pytest.raises(store.StoreError):
        geodata.import_pack(str(zipped), "nothing", str(packs))
    assert os.listdir(packs) == []
    assert not os.path.exists(geodata.geodata_path("nothing"))
