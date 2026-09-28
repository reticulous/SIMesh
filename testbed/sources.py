"""Where a pack's ground comes from: the public sources, what a rectangle
needs of each, and the cache they are fetched into.

```
front ── GET index-v1.json ────────────► download.geofabrik.de   every extract's outline, weekly
front ── GET <feed>/atom/0.atom ───────► gdi.berlin.de           Berlin's tiles, weekly
front ── GET /api/v1/nodes ────────────► map.meshcore.io         its node list, weekly
front ── HEAD <file> ──────────────────► the source's host       its size, once
front ── GET <file>, Range: bytes=<n>- ► the source's host       into the cache, resumed
```

    packs/.cache/glo30/<tile>.tif            Copernicus GLO-30 surface model, 1° tiles
    packs/.cache/worldcover/<tile>.tif       ESA WorldCover 2021 land cover, 3° tiles
    packs/.cache/itu/DN50.TXT, N050.TXT      ITU-R P.1812-8's ΔN and N0 maps, never packed
    packs/.cache/geofabrik/<id>.osm.pbf      one OpenStreetMap extract
    packs/.cache/berlin-dgm1/<tile>.zip      Berlin's 1 m terrain, 2 km tiles; x/ extracted
    packs/.cache/berlin-bdom/<tile>.zip      Berlin's 1 m surface, 2 km tiles; x/ extracted
    packs/.cache/berlin-lod2/<tile>.zip      Berlin's LoD2 building models, 1 km tiles; x/ extracted
    packs/.cache/zensus/<zip>                Zensus 2022's population grid; x/ extracted
    packs/.cache/meta/                       what the sources have: Geofabrik's index, Berlin's
                                             feeds, the MeshCore map's node list; a week old at most
    packs/.cache/sizes.json                  each file's size as its host said, by URL

Every build shares the cache, so a second region beside the first fetches
only what is new. A file is fetched once: into `<file>.part`, resumed from
where it stopped by a range request, and renamed into place when whole. A
file its host does not have (GLO-30 has no tile over open sea) leaves a
`<file>.absent` beside it and is not asked for again.

**OpenStreetMap is one Geofabrik extract**, the smallest whose outline holds
the rectangle: roads, places, sites and buildings all come from it. **Berlin's
tiles are chosen, not mirrored**: a tile's name is its south-west corner in
kilometres of EPSG:25833 (UTM zone 33 on ETRS89), and only the tiles meeting
the rectangle are fetched.

**A build takes the best source for each part of its rectangle**, and
offers no choice: Berlin's own data where the rectangle touches Berlin,
GLO-30 and OpenStreetMap's buildings on the rest, the Zensus grid where it
touches Germany (`plan` says which, and what each is used for).

Every request names SIMesh in its User-Agent, a 429 or 5xx is retried after
the Retry-After the host gives (or a growing pause), and nothing is fetched
that a build does not need.
"""

import asyncio
import contextlib
import json
import math
import os
import re
import shutil
import time
import zipfile

import aiohttp

import geodata
import store

CACHE_DIR = os.path.join(geodata.PACKS_DIR, ".cache")
USER_AGENT = "SIMesh (+https://github.com/reticulous/SIMesh)"
CHUNK = 1 << 16
TRIES = 4
BACKOFF_S = 5.0
META_MAX_AGE_S = 7 * 86400          # the Geofabrik index and Berlin's feeds, weekly
HEAD_PARALLEL = 6
MAX_CELLS = 25_000_000              # a grid past this is refused: rasters are held whole
RESOLUTIONS = (30.0, 10.0)

GLO30_URL = "https://copernicus-dem-30m.s3.amazonaws.com/{tile}/{tile}.tif"
WORLDCOVER_URL = ("https://esa-worldcover.s3.eu-central-1.amazonaws.com/v200/2021/map/"
                  "ESA_WorldCover_10m_2021_v200_{tile}_Map.tif")
ITU_URL = "https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1812-8-202509-I!!ZIP-E.zip"
ITU_MEMBERS = ("DN50.TXT", "N050.TXT")
GEOFABRIK_INDEX = "https://download.geofabrik.de/index-v1.json"
BERLIN_FEEDS = {"berlin-dgm1": "https://gdi.berlin.de/data/dgm1/atom/0.atom",
                "berlin-bdom": "https://gdi.berlin.de/data/bdom/atom/0.atom",
                "berlin-lod2": "https://gdi.berlin.de/data/a_lod2/atom/0.atom"}
BERLIN_TILE_KM = {"berlin-dgm1": 2, "berlin-bdom": 2, "berlin-lod2": 1}
BERLIN_ZONE = geodata.TransverseMercator(33, True, geodata.GRS80)
ZENSUS_URL = "https://www.destatis.de/static/DE/zensus/gitterdaten/Zensus2022_Bevoelkerungszahl.zip"
ZENSUS_MEMBER = "Zensus2022_Bevoelkerungszahl_100m-Gitter.csv"

# What the side panel says of each source: what it is, and its licence.
SOURCES = {
    "glo30": ("Copernicus GLO-30 surface model", "Copernicus DEM licence, attribution required"),
    "worldcover": ("ESA WorldCover 2021 land cover", "CC BY 4.0"),
    "itu": ("ITU-R P.1812-8 ΔN and N0 maps", "ITU; kept on this machine, only two numbers enter a pack"),
    "geofabrik": ("OpenStreetMap extract (Geofabrik)", "ODbL 1.0, © OpenStreetMap contributors"),
    "berlin-dgm1": ("Berlin DGM1, 1 m terrain", "Datenlizenz Deutschland – Zero – 2.0"),
    "berlin-bdom": ("Berlin bDOM, 1 m surface", "Datenlizenz Deutschland – Zero – 2.0"),
    "berlin-lod2": ("Berlin LoD2 building models", "Datenlizenz Deutschland – Zero – 2.0"),
    "zensus": ("Zensus 2022 100 m population grid", "Datenlizenz Deutschland – Namensnennung – 2.0"),
}


class SourceError(store.StoreError):
    """A source could not be read or fetched; the message is for the page."""


class File:
    """One file of one source: where it comes from, where it goes in the
    cache, and for a zip, which of its members a build reads (extracted into
    `x/` beside it)."""

    def __init__(self, source, url, name, members=None):
        self.source, self.url, self.name, self.members = source, url, name, members

    def path(self, root):
        return os.path.join(root, self.source, self.name)

    def __repr__(self):
        return "File(%s/%s)" % (self.source, self.name)


# ---- the rectangle ---------------------------------------------------------

def utm_zone(lon):
    return min(60, max(1, int(math.floor((lon + 180) / 6)) + 1))


def utm_hull(bbox, tm):
    """The rectangle's grid in a zone, as the compiler lays it: the extent of
    its four corners projected, (x0, y0, x1, y1) in metres."""
    lon0, lat0, lon1, lat1 = bbox
    xy = [tm.forward(lat, lon) for lon, lat in ((lon0, lat0), (lon0, lat1),
                                                  (lon1, lat0), (lon1, lat1))]
    return (min(p[0] for p in xy), min(p[1] for p in xy),
            max(p[0] for p in xy), max(p[1] for p in xy))


def degree_hull(bbox, tm):
    """The degrees the rectangle's grid reaches: its UTM hull's corners back
    in degrees. The grid bows past the rectangle with meridian convergence,
    so a tile source is asked for this, not for the rectangle."""
    x0, y0, x1, y1 = utm_hull(bbox, tm)
    ll = [tm.inverse(x, y) for x, y in ((x0, y0), (x0, y1), (x1, y0), (x1, y1))]
    return [min(p[1] for p in ll), min(p[0] for p in ll),
            max(p[1] for p in ll), max(p[0] for p in ll)]


def check(bbox, res_m):
    """The grid a rectangle makes at a resolution: {zone, epsg, cells: [nx,
    ny]}, or a StoreError with the sentence saying why it is refused."""
    try:
        lon0, lat0, lon1, lat1 = (float(v) for v in bbox)
    except (TypeError, ValueError) as err:
        raise store.StoreError("a rectangle is four numbers, west, south, east, north") from err
    if not (-180 <= lon0 < lon1 <= 180 and -80 <= lat0 < lat1 <= 84):
        raise store.StoreError("the rectangle is not west to east and south to north, "
                               "inside 80° S to 84° N")
    if float(res_m) not in RESOLUTIONS:
        raise store.StoreError("the resolution is 30 m or 10 m")
    zone = utm_zone((lon0 + lon1) / 2)
    if lon1 - lon0 > 6:
        raise store.StoreError("the rectangle is %.1f° wide, wider than its UTM zone (%d, 6°)"
                               % (lon1 - lon0, zone))
    tm = geodata.TransverseMercator(zone, True, geodata.WGS84)
    x0, y0, x1, y1 = utm_hull([lon0, lat0, lon1, lat1], tm)
    nx, ny = max(2, round((x1 - x0) / res_m)), max(2, round((y1 - y0) / res_m))
    if nx * ny > MAX_CELLS:
        raise store.StoreError("the grid would be %d × %d cells at %g m, more than %d million: "
                               "a smaller rectangle, or 30 m"
                               % (nx, ny, res_m, MAX_CELLS // 1_000_000))
    return {"zone": zone, "epsg": 32600 + zone, "cells": [nx, ny],
            "size_km": [round((x1 - x0) / 1000, 2), round((y1 - y0) / 1000, 2)]}


def hemi_lat(lat):
    return "%s%02d" % ("N" if lat >= 0 else "S", abs(lat))


def hemi_lon(lon):
    return "%s%03d" % ("E" if lon >= 0 else "W", abs(lon))


def glo30_files(hull):
    """The 1° tiles, named by their south-west corner."""
    lon0, lat0, lon1, lat1 = hull
    out = []
    for lat in range(math.floor(lat0), math.floor(lat1 - 1e-9) + 1):
        for lon in range(math.floor(lon0), math.floor(lon1 - 1e-9) + 1):
            tile = "Copernicus_DSM_COG_10_%s_00_%s_00_DEM" % (hemi_lat(lat), hemi_lon(lon))
            out.append(File("glo30", GLO30_URL.format(tile=tile), tile + ".tif"))
    return out


def worldcover_files(hull):
    """The 3° tiles, named by their south-west corner."""
    lon0, lat0, lon1, lat1 = hull
    out = []
    for lat in range(math.floor(lat0 / 3) * 3, math.floor((lat1 - 1e-9) / 3) * 3 + 1, 3):
        for lon in range(math.floor(lon0 / 3) * 3, math.floor((lon1 - 1e-9) / 3) * 3 + 1, 3):
            tile = hemi_lat(lat) + hemi_lon(lon)
            out.append(File("worldcover", WORLDCOVER_URL.format(tile=tile),
                            "ESA_WorldCover_10m_2021_v200_%s_Map.tif" % tile))
    return out


def itu_files():
    return [File("itu", ITU_URL, "itu.zip", ITU_MEMBERS)]


def zensus_files():
    return [File("zensus", ZENSUS_URL, "Zensus2022_Bevoelkerungszahl.zip", (ZENSUS_MEMBER,))]


# ---- outlines --------------------------------------------------------------

def _rings(geometry):
    """A GeoJSON (Multi)Polygon as a list of polygons, each [outer, holes…]."""
    if not geometry:
        return []
    if geometry.get("type") == "Polygon":
        return [geometry["coordinates"]]
    if geometry.get("type") == "MultiPolygon":
        return geometry["coordinates"]
    return []


def _in_ring(lon, lat, ring):
    inside = False
    j = len(ring) - 1
    for i in range(len(ring)):
        xi, yi = ring[i][0], ring[i][1]
        xj, yj = ring[j][0], ring[j][1]
        if (yi > lat) != (yj > lat) and lon < (xj - xi) * (lat - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def _in_polygon(lon, lat, polygon):
    return _in_ring(lon, lat, polygon[0]) and not any(_in_ring(lon, lat, h) for h in polygon[1:])


def _crosses(a, b, c, d):
    """Whether segments ab and cd cross."""
    def side(p, q, r):
        return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    return (side(a, b, c) * side(a, b, d) < 0) and (side(c, d, a) * side(c, d, b) < 0)


def _box_edges(bbox):
    lon0, lat0, lon1, lat1 = bbox
    corners = [(lon0, lat0), (lon1, lat0), (lon1, lat1), (lon0, lat1)]
    return corners, [(corners[i], corners[(i + 1) % 4]) for i in range(4)]


def _edge_crosses_box(polygon, edges):
    for ring in polygon:
        for i in range(len(ring) - 1):
            p, q = ring[i], ring[i + 1]
            if any(_crosses(p, q, a, b) for a, b in edges):
                return True
    return False


def holds(geometry, bbox):
    """Whether an outline holds the whole rectangle."""
    corners, edges = _box_edges(bbox)
    for polygon in _rings(geometry):
        if all(_in_polygon(lon, lat, polygon) for lon, lat in corners) \
                and not _edge_crosses_box(polygon, edges):
            return True
    return False


def meets(geometry, bbox):
    """Whether an outline and the rectangle share any ground."""
    corners, edges = _box_edges(bbox)
    lon0, lat0, lon1, lat1 = bbox
    for polygon in _rings(geometry):
        if any(_in_polygon(lon, lat, polygon) for lon, lat in corners):
            return True
        if any(lon0 <= p[0] <= lon1 and lat0 <= p[1] <= lat1 for p in polygon[0]):
            return True
        if _edge_crosses_box(polygon, edges):
            return True
    return False


def area(geometry):
    """An outline's area in square degrees, outer rings only: enough to say
    which of two outlines is the smaller."""
    total = 0.0
    for polygon in _rings(geometry):
        ring = polygon[0]
        total += abs(sum(ring[i][0] * ring[i + 1][1] - ring[i + 1][0] * ring[i][1]
                         for i in range(len(ring) - 1))) / 2
    return total


def smallest_extract(index, bbox):
    """The Geofabrik extract whose outline holds the rectangle and is the
    smallest: its index feature, or None."""
    best = None
    for feature in index.get("features") or ():
        props = feature.get("properties") or {}
        if not (props.get("urls") or {}).get("pbf"):
            continue
        geometry = feature.get("geometry")
        if holds(geometry, bbox):
            size = area(geometry)
            if best is None or size < best[0]:
                best = (size, feature)
    return best[1] if best else None


def extract_file(feature):
    props = feature["properties"]
    return File("geofabrik", props["urls"]["pbf"], "%s.osm.pbf" % props["id"])


def outline(index, extract_id):
    for feature in index.get("features") or ():
        if (feature.get("properties") or {}).get("id") == extract_id:
            return feature.get("geometry")
    return None


# ---- Berlin ------------------------------------------------------------------

TILE_RE = re.compile(r'href="([^"]+?(\d{3})_(\d{4})\.zip)"')


def berlin_tiles(feed_text):
    """A Berlin ATOM feed's tiles: [(url, easting km, northing km)]."""
    return [(m.group(1), int(m.group(2)), int(m.group(3))) for m in TILE_RE.finditer(feed_text)]


def berlin_km_box(bbox):
    """The rectangle's extent in kilometres of EPSG:25833."""
    x0, y0, x1, y1 = utm_hull(bbox, BERLIN_ZONE)
    return x0 / 1000, y0 / 1000, x1 / 1000, y1 / 1000


def berlin_files(source, feed_text, bbox):
    """The tiles of one Berlin feed that meet the rectangle."""
    x0, y0, x1, y1 = berlin_km_box(bbox)
    size = BERLIN_TILE_KM[source]
    out = []
    for url, e, n in berlin_tiles(feed_text):
        if e < x1 and e + size > x0 and n < y1 and n + size > y0:
            out.append(File(source, url, url.rsplit("/", 1)[1], "*.xyz" if source != "berlin-lod2"
                            else "*.xml"))
    return out


def berlin_extent(feed_text, source="berlin-lod2"):
    """[lon0, lat0, lon1, lat1] of every tile in a feed: where Berlin's data is."""
    tiles = berlin_tiles(feed_text)
    if not tiles:
        return None
    size = BERLIN_TILE_KM[source]
    x0 = min(t[1] for t in tiles) * 1000
    y0 = min(t[2] for t in tiles) * 1000
    x1 = (max(t[1] for t in tiles) + size) * 1000
    y1 = (max(t[2] for t in tiles) + size) * 1000
    ll = [BERLIN_ZONE.inverse(x, y) for x, y in ((x0, y0), (x0, y1), (x1, y0), (x1, y1))]
    return [min(p[1] for p in ll), min(p[0] for p in ll),
            max(p[1] for p in ll), max(p[0] for p in ll)]


# ---- the cache -------------------------------------------------------------

class Cache:
    """The download cache, and the one session every fetch goes through."""

    def __init__(self, session, root=None):
        self.session = session
        self.root = root or CACHE_DIR
        self.sizes_path = os.path.join(self.root, "sizes.json")
        self.sizes = {}
        with contextlib.suppress(OSError, ValueError):
            with open(self.sizes_path, encoding="utf-8") as handle:
                self.sizes = json.load(handle)
        self.meta_locks = {}

    def have(self, f):
        """True when fetched, False when its host has none, None when not yet."""
        path = f.path(self.root)
        if os.path.isfile(path):
            return True
        if os.path.isfile(path + ".absent"):
            return False
        return None

    def fetched_bytes(self, f):
        """How much of a file is here: whole, or its `.part` so far."""
        path = f.path(self.root)
        for each in (path, path + ".part"):
            with contextlib.suppress(OSError):
                return os.path.getsize(each)
        return 0

    async def request(self, method, url, headers=None):
        """A response from the host, retried politely on 429 and 5xx."""
        headers = dict(headers or {}, **{"User-Agent": USER_AGENT})
        pause = BACKOFF_S
        for attempt in range(TRIES):
            try:
                resp = await self.session.request(method, url, headers=headers,
                                                  timeout=aiohttp.ClientTimeout(
                                                      total=None, sock_connect=30, sock_read=120))
            except (aiohttp.ClientError, asyncio.TimeoutError) as err:
                if attempt == TRIES - 1:
                    raise SourceError("%s: %s" % (url, err or type(err).__name__)) from err
                await asyncio.sleep(pause)
                pause *= 2
                continue
            if resp.status == 429 or resp.status >= 500:
                wait = resp.headers.get("Retry-After")
                resp.release()
                if attempt == TRIES - 1:
                    raise SourceError("%s answers %d" % (url, resp.status))
                await asyncio.sleep(float(wait) if wait and wait.isdigit() else pause)
                pause *= 2
                continue
            return resp
        raise SourceError("%s: no answer" % url)

    async def size(self, f):
        """A file's size in bytes as its host says, asked once; None when
        the host does not say."""
        if f.url in self.sizes:
            return self.sizes[f.url]
        resp = await self.request("HEAD", f.url)
        try:
            got = None if resp.status == 404 else resp.content_length
        finally:
            resp.release()
        self.sizes[f.url] = got
        with contextlib.suppress(OSError):
            store.write_text(self.sizes_path, json.dumps(self.sizes, indent=0, sort_keys=True))
        return got

    async def sizes_of(self, files):
        """{file: bytes still to fetch, or None when unknown} for files not here."""
        todo = [f for f in files if self.have(f) is None]
        gate = asyncio.Semaphore(HEAD_PARALLEL)

        async def one(f):
            async with gate:
                try:
                    whole = await self.size(f)
                except SourceError:
                    whole = None
                return f, None if whole is None else max(0, whole - self.fetched_bytes(f))
        return dict(await asyncio.gather(*(one(f) for f in todo)))

    async def fetch(self, f, progress=None):
        """A file into the cache, resumed where a `.part` of it stopped:
        its path, or None when its host has none. `progress(bytes)` hears
        the bytes it holds as they arrive."""
        have = self.have(f)
        path = f.path(self.root)
        if have is not None:
            return path if have else None
        os.makedirs(os.path.dirname(path), exist_ok=True)
        part = path + ".part"
        start = self.fetched_bytes(f)
        resp = await self.request("GET", f.url, {"Range": "bytes=%d-" % start} if start else None)
        try:
            if resp.status == 404:
                store.write_text(path + ".absent", "%s\n" % f.url)
                return None
            if resp.status == 416 and start:
                os.replace(part, path)
                return path
            if resp.status not in (200, 206):
                raise SourceError("%s answers %d" % (f.url, resp.status))
            mode = "ab" if resp.status == 206 else "wb"
            got = start if resp.status == 206 else 0
            with open(part, mode) as out:
                async for chunk in resp.content.iter_chunked(CHUNK):
                    out.write(chunk)
                    got += len(chunk)
                    if progress:
                        progress(got)
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            raise SourceError("%s: %s" % (f.url, err or type(err).__name__)) from err
        finally:
            resp.release()
        os.replace(part, path)
        return path

    def extracted(self, f):
        """The members a build reads of a fetched zip, extracted into `x/`
        beside it (once): their paths. `members` is names, or one `*.ext`."""
        path = f.path(self.root)
        into = os.path.join(os.path.dirname(path), "x")
        done = path + ".x"
        if os.path.isfile(done):
            with open(done, encoding="utf-8") as handle:
                return [os.path.join(into, n) for n in handle.read().split("\n") if n]
        names = []
        try:
            with zipfile.ZipFile(path) as zf:
                for info in zf.infolist():
                    base = os.path.basename(info.filename)
                    if info.is_dir() or not base or not self.wanted(f, base):
                        continue
                    os.makedirs(into, exist_ok=True)
                    with zf.open(info) as src, open(os.path.join(into, base) + ".part", "wb") as dst:
                        shutil.copyfileobj(src, dst, CHUNK)
                    os.replace(os.path.join(into, base) + ".part", os.path.join(into, base))
                    names.append(base)
        except zipfile.BadZipFile as err:
            with contextlib.suppress(OSError):
                os.remove(path)
            raise SourceError("%s is not a zip, and is fetched again next time: %s"
                              % (f.url, err)) from err
        store.write_text(done, "".join(n + "\n" for n in names))
        return [os.path.join(into, n) for n in names]

    @staticmethod
    def wanted(f, base):
        members = f.members if isinstance(f.members, (list, tuple)) else (f.members,)
        for m in members:
            if m.startswith("*") and base.lower().endswith(m[1:].lower()):
                return True
            if base == m:
                return True
        return False

    async def meta(self, name, url):
        """A file saying what a source has (the Geofabrik index, a Berlin
        feed), fetched when missing or a week old: its text."""
        path = await self.meta_file(name, url)
        with open(path, encoding="utf-8-sig") as handle:
            return handle.read()

    async def meta_file(self, name, url, max_age_s=META_MAX_AGE_S):
        """As `meta`, the file's path: kept `max_age_s`, and the copy there
        is served when a fetch fails."""
        path = os.path.join(self.root, "meta", name)
        lock = self.meta_locks.setdefault(name, asyncio.Lock())
        async with lock:
            fresh = os.path.isfile(path) and time.time() - os.path.getmtime(path) < max_age_s
            if not fresh:
                try:
                    resp = await self.request("GET", url)
                    try:
                        if resp.status != 200:
                            raise SourceError("%s answers %d" % (url, resp.status))
                        body = await resp.read()
                    finally:
                        resp.release()
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with open(path + ".part", "wb") as out:
                        out.write(body)
                    os.replace(path + ".part", path)
                except SourceError:
                    if not os.path.isfile(path):
                        raise
            return path

    def meta_age(self, name):
        """When a meta file was fetched, in Unix seconds, or None."""
        with contextlib.suppress(OSError):
            return os.path.getmtime(os.path.join(self.root, "meta", name))
        return None

    async def index(self):
        text = await self.meta("geofabrik-index-v1.json", GEOFABRIK_INDEX)
        try:
            return json.loads(text)
        except ValueError as err:
            raise SourceError("Geofabrik's index is not JSON: %s" % err) from err

    async def feed(self, source):
        return await self.meta(source + ".atom", BERLIN_FEEDS[source])


# ---- what a build needs ------------------------------------------------------

async def areas(cache):
    """The outlines of the sources that do not cover the world, for the
    map: {berlin: GeoJSON geometry, germany: GeoJSON geometry}."""
    index = await cache.index()
    return {"berlin": outline(index, "berlin"), "germany": outline(index, "germany")}


def used_for(berlin, inside_berlin, germany, inside_germany):
    """What each source is used for in a rectangle that touches Berlin
    (`berlin`) or lies inside it (`inside_berlin`), and the same of Germany:
    {source: one line}. A source with no line is not used."""
    rest = "" if not berlin else " where Berlin has no tile" if inside_berlin else " outside Berlin"
    out = {"glo30": "terrain and clutter" + rest,
           "worldcover": "land cover classes",
           "itu": "the radio climate (ΔN and N0; kept here)",
           "geofabrik": "roads and places; buildings" + rest}
    if berlin:
        where = "" if inside_berlin else " inside Berlin"
        out["berlin-dgm1"] = "terrain" + where
        out["berlin-bdom"] = "clutter" + where
        out["berlin-lod2"] = "buildings" + where
    if germany:
        out["zensus"] = "population" + ("" if inside_germany else " inside Germany")
    return out


async def plan(cache, spec, sizes=True):
    """What a build of `spec` ({bbox, res_m}) takes: the grid, the sources
    chosen for the rectangle with what each is used for, and each source's
    files with what is still to fetch.

    The best source is chosen for every part of the rectangle: Berlin's 1 m
    terrain and surface and its LoD2 buildings where the rectangle touches
    Berlin, GLO-30 and OpenStreetMap's buildings on the rest; the Zensus
    grid where it touches Germany, and no population elsewhere; WorldCover,
    OpenStreetMap's roads and places, and the ITU maps everywhere.

    {grid, extract, sources: [{source, title, licence, used_for, files,
    cached, to_fetch}], files: {source: [File]}}; to_fetch is None where a
    host does not say a size.
    """
    bbox = [float(v) for v in spec["bbox"]]
    grid = check(bbox, float(spec.get("res_m", 30)))
    tm = geodata.TransverseMercator(grid["zone"], True, geodata.WGS84)
    hull = degree_hull(bbox, tm)
    index = await cache.index()
    lod2_feed = await cache.feed("berlin-lod2")
    berlin = bool(berlin_files("berlin-lod2", lod2_feed, bbox))
    germany_outline = outline(index, "germany")
    germany = meets(germany_outline, bbox)
    extract = smallest_extract(index, bbox)
    if extract is None:
        raise store.StoreError("no Geofabrik extract holds the whole rectangle")
    files = {"glo30": glo30_files(hull), "worldcover": worldcover_files(hull),
             "itu": itu_files(), "geofabrik": [extract_file(extract)]}
    if berlin:
        files["berlin-dgm1"] = berlin_files("berlin-dgm1", await cache.feed("berlin-dgm1"), bbox)
        files["berlin-bdom"] = berlin_files("berlin-bdom", await cache.feed("berlin-bdom"), bbox)
        files["berlin-lod2"] = berlin_files("berlin-lod2", lod2_feed, bbox)
    if germany:
        files["zensus"] = zensus_files()
    uses = used_for(berlin, berlin and holds(outline(index, "berlin"), bbox),
                    germany, germany and holds(germany_outline, bbox))
    todo = await cache.sizes_of([f for fs in files.values() for f in fs]) if sizes else {}
    rows = []
    for source, fs in files.items():
        title, licence = SOURCES[source]
        pending = [todo.get(f) for f in fs if cache.have(f) is None]
        rows.append({"source": source, "title": title, "licence": licence,
                     "used_for": uses[source], "files": len(fs),
                     "cached": sum(1 for f in fs if cache.have(f) is not None),
                     "to_fetch": None if None in pending else sum(pending)})
    return {"grid": grid,
            "extract": {"id": extract["properties"]["id"], "name": extract["properties"].get("name")},
            "sources": rows, "files": files}
