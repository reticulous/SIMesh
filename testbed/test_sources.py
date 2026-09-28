"""Building a pack from its sources: the rectangle's grid and what it is
refused for, which tiles and which extract a rectangle needs, the cache's
fetches (resumed, once, a missing file remembered), and a build end to end
against a host and a compiler of the test's own."""

import asyncio
import json
import os
import sys
import zipfile

import aiohttp
import pytest
from aiohttp import web

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import geodata  # noqa: E402
import packbuild  # noqa: E402
import sources  # noqa: E402
import store  # noqa: E402

MITTE = [13.38, 52.51, 13.42, 52.53]


def square(lon0, lat0, lon1, lat1):
    return [[lon0, lat0], [lon1, lat0], [lon1, lat1], [lon0, lat1], [lon0, lat0]]


def index():
    """A Geofabrik index with Germany, Brandenburg round Berlin, and Berlin."""
    def feature(id_, ring):
        return {"type": "Feature", "properties": {
            "id": id_, "name": id_.title(),
            "urls": {"pbf": "/geofabrik/%s-latest.osm.pbf" % id_}},
            "geometry": {"type": "MultiPolygon", "coordinates": [[ring]]}}
    return {"features": [feature("germany", square(5.8, 47.2, 15.1, 55.1)),
                         feature("brandenburg", square(11.2, 51.3, 14.8, 53.6)),
                         feature("berlin", square(13.08, 52.33, 13.77, 52.68))]}


FEED = "\n".join('<link href="https://gdi.berlin.de/data/a_lod2/atom/LoD2_%d_%d.zip"/>' % (e, n)
                 for e in range(388, 394) for n in range(5818, 5823))


def test_a_rectangle_is_a_grid_in_the_zone_of_its_centre():
    got = sources.check(MITTE, 30)
    assert got["zone"] == 33 and got["epsg"] == 32633
    nx, ny = got["cells"]
    assert 85 < nx < 95 and 70 < ny < 80
    with pytest.raises(store.StoreError, match="wider than its UTM zone"):
        sources.check([10, 50, 17, 51], 30)
    with pytest.raises(store.StoreError, match="million"):
        sources.check([12, 50, 17, 54], 10)
    with pytest.raises(store.StoreError, match="30 m or 10 m"):
        sources.check(MITTE, 20)
    with pytest.raises(store.StoreError, match="west to east"):
        sources.check([13.42, 52.51, 13.38, 52.53], 30)


def test_tiles_are_named_by_their_south_west_corner_over_the_grids_reach():
    tm = geodata.TransverseMercator(33, True, geodata.WGS84)
    hull = sources.degree_hull([12.96, 52.25, 13.85, 52.79], tm)
    assert [f.name for f in sources.glo30_files(hull)] == [
        "Copernicus_DSM_COG_10_N52_00_E012_00_DEM.tif",
        "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"]
    assert [f.name for f in sources.glo30_files([-58.5, -34.7, -58.3, -34.5])] == [
        "Copernicus_DSM_COG_10_S35_00_W059_00_DEM.tif"]
    assert [f.name for f in sources.worldcover_files(hull)] == [
        "ESA_WorldCover_10m_2021_v200_N51E012_Map.tif"]
    assert sources.glo30_files(hull)[0].url.endswith(
        "/Copernicus_DSM_COG_10_N52_00_E012_00_DEM/Copernicus_DSM_COG_10_N52_00_E012_00_DEM.tif")


def test_the_smallest_extract_holding_the_rectangle_is_the_one():
    assert sources.smallest_extract(index(), MITTE)["properties"]["id"] == "berlin"
    assert sources.smallest_extract(index(), [13.0, 52.5, 13.2, 52.6])["properties"]["id"] \
        == "brandenburg"
    assert sources.smallest_extract(index(), [4.0, 50, 6.0, 51]) is None
    germany = sources.outline(index(), "germany")
    assert sources.meets(germany, [4.0, 50, 6.0, 51]) and not sources.meets(germany, [0, 0, 1, 1])
    # A rectangle astride a concave outline's notch is not held by it.
    notched = {"type": "Polygon", "coordinates": [[[0, 0], [4, 0], [4, 4], [3, 4], [3, 1],
                                                   [1, 1], [1, 4], [0, 4], [0, 0]]]}
    assert not sources.holds(notched, [0.5, 0.5, 3.5, 3.5])
    assert sources.holds(notched, [0.2, 0.2, 3.8, 0.8])


def test_only_the_berlin_tiles_meeting_the_rectangle_are_chosen():
    chosen = sources.berlin_files("berlin-lod2", FEED, MITTE)
    names = sorted(f.name for f in chosen)
    assert names and len(names) < 30
    x0, y0, x1, y1 = sources.berlin_km_box(MITTE)
    for f in chosen:
        e, n = (int(v) for v in f.name[5:-4].split("_"))
        assert e < x1 and e + 1 > x0 and n < y1 and n + 1 > y0
    assert sources.berlin_files("berlin-lod2", FEED, [12.0, 52.5, 12.1, 52.6]) == []
    assert sources.berlin_extent(FEED)[0] < MITTE[0]


# ---- a host of the test's own --------------------------------------------------

def host_app(root, calls):
    async def any_file(request):
        calls.append((request.method, request.path, request.headers.get("Range"),
                      request.headers.get("User-Agent")))
        path = os.path.join(root, request.path.lstrip("/"))
        if not os.path.isfile(path):
            raise web.HTTPNotFound()
        return web.FileResponse(path)
    app = web.Application()
    app.router.add_route("*", "/{tail:.*}", any_file)
    return app


def running_host(root, check):
    calls = []

    async def go():
        runner = web.AppRunner(host_app(str(root), calls))
        await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0)
        await site.start()
        base = "http://127.0.0.1:%d" % runner.addresses[0][1]
        try:
            async with aiohttp.ClientSession() as session:
                await check(base, session, calls)
        finally:
            await runner.cleanup()
    asyncio.run(go())


def test_a_file_is_fetched_once_resumed_and_a_missing_one_remembered(tmp_path):
    served = tmp_path / "served"
    served.mkdir()
    (served / "big.bin").write_bytes(bytes(range(256)) * 1000)

    async def check(base, session, calls):
        cache = sources.Cache(session, str(tmp_path / "cache"))
        f = sources.File("glo30", base + "/big.bin", "big.bin")
        path = f.path(cache.root)
        os.makedirs(os.path.dirname(path))
        with open(path + ".part", "wb") as out:
            out.write((bytes(range(256)) * 1000)[:1000])
        assert await cache.size(f) == 256000
        assert (await cache.sizes_of([f]))[f] == 255000
        heard = []
        assert await cache.fetch(f, heard.append) == path
        assert open(path, "rb").read() == bytes(range(256)) * 1000
        assert calls[-1][2] == "bytes=1000-" and heard[-1] == 256000
        assert calls[-1][3] == sources.USER_AGENT
        before = len(calls)
        assert await cache.fetch(f) == path and len(calls) == before
        gone = sources.File("glo30", base + "/sea.tif", "sea.tif")
        assert await cache.fetch(gone) is None
        assert await cache.fetch(gone) is None and len(calls) == before + 1
        assert cache.have(gone) is False
    running_host(tmp_path / "served", check)


def test_a_zips_wanted_members_are_extracted_once(tmp_path):
    cache = sources.Cache(None, str(tmp_path))
    f = sources.File("itu", "http://x/itu.zip", "itu.zip", sources.ITU_MEMBERS)
    os.makedirs(tmp_path / "itu")
    with zipfile.ZipFile(tmp_path / "itu" / "itu.zip", "w") as zf:
        for name in ("ReadMe.doc", "DN50.TXT", "N050.TXT", "LAT.TXT"):
            zf.writestr(name, name)
    got = cache.extracted(f)
    assert sorted(os.path.basename(p) for p in got) == ["DN50.TXT", "N050.TXT"]
    assert sorted(os.listdir(tmp_path / "itu" / "x")) == ["DN50.TXT", "N050.TXT"]
    os.remove(tmp_path / "itu" / "itu.zip")
    assert cache.extracted(f) == got


# ---- a build, end to end ---------------------------------------------------------

FAKE_JOB = r'''#!/usr/bin/env python3
import json, os, sys
params = json.load(sys.stdin)
with open(os.environ["FAKE_JOB_SAW"], "w") as out:
    json.dump(params, out)
if params["name"] == "broken":
    print("pack build: something went wrong", file=sys.stderr)
    print(json.dumps({"error": "the DSM tiles leave holes"}))
    sys.exit(1)
os.makedirs(params["out_dir"], exist_ok=True)
for i, step in enumerate(["terrain", "clutter", "roads"]):
    print(json.dumps({"step": step, "done": i, "total": 3}), flush=True)
manifest = {"name": params["name"], "region": {"bbox": params["bbox"],
            "crs_epsg": 32600 + params["utm_zone"]}, "layers": [], "licenses": []}
path = os.path.join(params["out_dir"], "manifest.json")
with open(path, "w") as out:
    json.dump(manifest, out)
print(json.dumps({"manifest": path}))
'''


def serve_sources(root):
    """The public hosts as a directory: the index, Berlin's feeds, and a file
    for every URL a Mitte build asks for, except the GLO-30 tile east of it
    and Berlin's 1 m tiles."""
    (root / "geofabrik").mkdir(parents=True)
    (root / "index.json").write_text(json.dumps(index()))
    (root / "geofabrik" / "berlin-latest.osm.pbf").write_bytes(b"pbf" * 100)
    (root / "lod2").mkdir()
    with zipfile.ZipFile(root / "lod2" / "LoD2_390_5820.zip", "w") as zf:
        zf.writestr("LoD2_33_390_5820_1_BE.xml", "<CityModel/>")
    with zipfile.ZipFile(root / "zensus.zip", "w") as zf:
        zf.writestr(sources.ZENSUS_MEMBER, "GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner\n")


def test_each_source_says_what_it_is_used_for():
    outside = sources.used_for(False, False, True, True)
    assert "berlin-lod2" not in outside and outside["zensus"] == "population"
    assert outside["geofabrik"] == "roads and places; buildings"
    across = sources.used_for(True, False, True, False)
    assert across["berlin-lod2"] == "buildings inside Berlin"
    assert across["berlin-dgm1"] == "terrain inside Berlin"
    assert across["glo30"] == "terrain and clutter outside Berlin"
    assert across["geofabrik"] == "roads and places; buildings outside Berlin"
    assert across["zensus"] == "population inside Germany"
    assert "zensus" not in sources.used_for(False, False, False, False)


def test_a_build_fetches_what_it_needs_and_hands_the_compiler_its_inputs(tmp_path, monkeypatch):
    served = tmp_path / "served"
    serve_sources(served)
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    packs = tmp_path / "packs"
    job = tmp_path / "planner-job"
    job.write_text(FAKE_JOB)
    job.chmod(0o755)
    saw = tmp_path / "saw.json"
    monkeypatch.setenv("FAKE_JOB_SAW", str(saw))

    async def check(base, session, calls):
        monkeypatch.setattr(sources, "GEOFABRIK_INDEX", base + "/index.json")
        monkeypatch.setattr(sources, "BERLIN_FEEDS", {
            s: "%s/%s.atom" % (base, s) for s in sources.BERLIN_FEEDS})
        monkeypatch.setattr(sources, "ZENSUS_URL", base + "/zensus.zip")
        monkeypatch.setattr(sources, "GLO30_URL", base + "/glo30/{tile}.tif")
        monkeypatch.setattr(sources, "WORLDCOVER_URL", base + "/wc/{tile}.tif")
        monkeypatch.setattr(sources, "ITU_URL", base + "/itu.zip")
        (served / "glo30").mkdir()
        (served / "glo30" / "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif").write_bytes(b"dem")
        (served / "wc").mkdir()
        (served / "wc" / "N51E012.tif").write_bytes(b"wc")
        with zipfile.ZipFile(served / "itu.zip", "w") as zf:
            zf.writestr("DN50.TXT", "dn")
            zf.writestr("N050.TXT", "n0")
        idx = index()
        for feature in idx["features"]:
            feature["properties"]["urls"]["pbf"] = base + feature["properties"]["urls"]["pbf"]
        (served / "index.json").write_text(json.dumps(idx))
        for source in sources.BERLIN_FEEDS:
            (served / (source + ".atom")).write_text(
                FEED.replace("https://gdi.berlin.de/data/a_lod2/atom", base + "/" + source[7:]))
        cache = sources.Cache(session, str(tmp_path / "cache"))

        planned = await sources.plan(cache, {"bbox": MITTE, "res_m": 30})
        rows = {r["source"]: r for r in planned["sources"]}
        assert rows["geofabrik"]["to_fetch"] == 300 and rows["glo30"]["to_fetch"] == 3
        assert planned["extract"]["id"] == "berlin"
        assert rows["berlin-lod2"]["used_for"] == "buildings"
        assert rows["geofabrik"]["used_for"] == "roads and places; buildings where Berlin has no tile"
        assert rows["zensus"]["used_for"] == "population"
        potsdam = await sources.plan(cache, {"bbox": [12.9, 52.35, 13.0, 52.42], "res_m": 30})
        rows = {r["source"]: r for r in potsdam["sources"]}
        assert potsdam["extract"]["id"] == "brandenburg"
        assert sorted(rows) == ["geofabrik", "glo30", "itu", "worldcover", "zensus"]
        assert rows["geofabrik"]["used_for"] == "roads and places; buildings"
        paris = {"bbox": [2.0, 48.8, 2.1, 48.9], "res_m": 30}
        with pytest.raises(store.StoreError, match="no Geofabrik extract"):
            await sources.plan(cache, paris)

        said = []
        spec = {"name": "mitte", "bbox": MITTE, "res_m": 30}
        packbuild.refuse(spec, str(packs))
        build = packbuild.Build(cache, spec, str(job), said.append, packs=str(packs))
        await build.start()
        assert build.row["state"] == "done", build.row
        params = json.loads(saw.read_text())
        assert params["osm_buildings"] and params["utm_zone"] == 33
        assert os.path.basename(params["lod2_dir"]) == "lod2"
        assert params["berlin_1m_dir"] and "zensus_csv" in params
        assert params["osm_pbf"].endswith("geofabrik/berlin.osm.pbf")
        assert os.path.basename(params["itu_maps_dir"]) == "x"
        assert [os.path.basename(p) for p in params["dsm_tiles"]] == [
            "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"]
        states = [r["state"] for r in said]
        assert states[0] == "fetching" and "compiling" in states and states[-1] == "done"
        assert [r["step"] for r in said if r["state"] == "compiling"][1:4] == \
            ["terrain", "clutter", "roads"]
        gd = geodata.load("mitte")
        assert gd.is_pack and gd.pack_dir == str(packs / "mitte")
        assert not [n for n in os.listdir(packs) if n.startswith(".part-")]
        with pytest.raises(store.StoreError, match="already"):
            packbuild.refuse(spec, str(packs))

        fetched = len(calls)
        build = packbuild.Build(cache, dict(spec, name="broken"), str(job), said.append,
                                packs=str(packs))
        await build.start()
        assert build.row["state"] == "failed"
        assert build.row["error"] == "the DSM tiles leave holes"
        assert "something went wrong" in open(build.log_path).read()
        assert not os.path.exists(geodata.geodata_path("broken"))
        # The second build over the same ground fetched nothing new.
        assert not [c for c in calls[fetched:] if c[0] == "GET" and "glo30" in c[1]]

        build = packbuild.Build(cache, dict(spec, name="gone"), str(job), said.append,
                                packs=str(packs))
        task = build.start()
        await asyncio.sleep(0)
        build.cancel()
        await task
        assert build.row["state"] == "cancelled"
        assert not os.path.exists(packs / "gone")
    running_host(served, check)
