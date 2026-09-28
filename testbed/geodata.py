"""Geodata: the ground nodes stand on.

    testbed/geodata/<name>.yaml

One of two kinds:

    pack: ../../packs/berlin-city       # a pack directory, in SIMesh's packs/

    synthetic:                          # ground made up here, at 0°, 0°
      terrain: flat
      exponent: 2.7                     # log-distance path loss exponent
      extent_m: 20000                   # the square it covers, centred on 0°, 0°

A pack says nothing its pack does not already say: its CRS, extent and
layers are the pack's `manifest.json`, read where the pack is. The path is
relative to the geodata file; an imported pack goes into `packs/`.
Geodata names no node, and a nodeset names no geodata: a nodeset is offered
on every geodata whose extent holds one of its nodes. Noise figure and the
interference figures belong to the medium, not the ground, and are simd's.

**Synthetic ground lies at 0°, 0°**, and its degrees are metres by one fixed
rule on both axes, a nautical mile to the minute of arc:

    x = lon · 60 · 1852 m          y = lat · 60 · 1852 m

So a position is still latitude and longitude, the page can show it as
either, and a nodeset made on one synthetic ground stands on any other.
`terrain: flat` is the only terrain so far: a pair's loss is log-distance
with the ground's exponent. A node's height is above the ground under it.

Coordinates on a pack are its own CRS, absolute easting and northing in
metres. Packs are UTM on WGS84 (EPSG 326zz north, 327zz south) or on ETRS89
(258zz), and the transverse Mercator here is Krüger's series to sixth order
in n (Karney 2011), good to well under a millimetre inside a zone, so what
SIMesh sends the planner is the point the planner itself would compute. No
projection library is needed.

**A SIMesh geodata pack** is how geodata moves between machines: a zip
holding `geodata.yaml` at the top and, for a pack, the pack under `pack/`,
which the yaml's `pack:` names. Importing takes that, or a bare planner pack
(its `manifest.json` at the zip's root, or inside one top-level directory),
expands a pack into `packs/<name>/`, whole or not at all, and writes the
geodata file that names it.

**Nodes are never part of the ground.** A planner pack may carry a `Nodes`
layer, a deployed network baked in; it is left out of what the page is told,
of an export, and of an import. Nodes belong to nodesets.
"""

import contextlib
import copy
import hashlib
import json
import math
import os
import shutil
import zipfile

import yaml

import store

PACK, SYNTHETIC = "pack", "synthetic"
MANIFEST = "manifest.json"
TERRAINS = ("flat",)
DEFAULT_EXPONENT = 2.7
DEFAULT_EXTENT_M = 20000.0
M_PER_DEGREE = 60 * 1852.0          # a nautical mile to the minute, on both axes
SIMESH_ROOT = os.path.dirname(store.SIM_DIR)
PLANNER_DIR = os.path.join(SIMESH_ROOT, "planner")
PACKS_DIR = os.path.join(SIMESH_ROOT, "packs")
PART_PREFIX = ".part-"
CHUNK = 1 << 16
GEODATA_MEMBER = "geodata.yaml"     # at the top of a SIMesh geodata pack
PACK_MEMBER = "pack"                # where an exported pack goes inside it
EXPORT_LEVEL = 1                    # deflate's fastest: berlin-city, 450 MB, is 124 MB in 2 s
NODES_LAYER = "Nodes"
NODES_NOTICE = "Deployed mesh nodes"   # how the Nodes layer's notice names its source


def planner_repo():
    """SIMesh's planner: the Rust workspace in `planner/`, whose planner-web
    serves a pack."""
    return PLANNER_DIR


def packs_dir():
    """Where packs live, and an imported pack goes: `packs/`."""
    return PACKS_DIR


# ---- transverse Mercator -------------------------------------------------

# Semi-major axis and flattening of the two ellipsoids UTM packs are on.
WGS84 = (6378137.0, 1 / 298.257223563)
GRS80 = (6378137.0, 1 / 298.257222101)
UTM_K0 = 0.9996
UTM_FALSE_EASTING = 500000.0
UTM_FALSE_NORTHING_SOUTH = 10000000.0


def utm_zone_of(epsg):
    """(zone, north, ellipsoid) for an EPSG code this module can project to."""
    epsg = int(epsg)
    for base, north, ellipsoid in ((32600, True, WGS84), (32700, False, WGS84),
                                   (25800, True, GRS80)):
        zone = epsg - base
        if 1 <= zone <= 60:
            return zone, north, ellipsoid
    raise store.StoreError("EPSG %d is not a UTM zone (326zz, 327zz or 258zz)" % epsg)


class TransverseMercator:
    """One UTM zone, forward and back, in Krüger's series.

    The series coefficients depend only on the ellipsoid's third flattening
    n, so they are worked out once per zone object. The inverse's last step,
    from the conformal latitude back to the geographic one, is Newton on
    tan(latitude), which converges to machine precision in three or four
    steps from the conformal value; five are taken unconditionally.
    """

    def __init__(self, zone, north=True, ellipsoid=WGS84):
        a, f = ellipsoid
        self.zone, self.north = zone, north
        self.lon0 = math.radians((zone - 1) * 6 - 180 + 3)
        self.fn = 0.0 if north else UTM_FALSE_NORTHING_SOUTH
        self.e = math.sqrt(f * (2 - f))
        n = f / (2 - f)
        n2, n3, n4, n5, n6 = n ** 2, n ** 3, n ** 4, n ** 5, n ** 6
        self.k0a = UTM_K0 * a / (1 + n) * (1 + n2 / 4 + n4 / 64 + n6 / 256)
        self.alpha = (
            n / 2 - 2 * n2 / 3 + 5 * n3 / 16 + 41 * n4 / 180 - 127 * n5 / 288 + 7891 * n6 / 37800,
            13 * n2 / 48 - 3 * n3 / 5 + 557 * n4 / 1440 + 281 * n5 / 630 - 1983433 * n6 / 1935360,
            61 * n3 / 240 - 103 * n4 / 140 + 15061 * n5 / 26880 + 167603 * n6 / 181440,
            49561 * n4 / 161280 - 179 * n5 / 168 + 6601661 * n6 / 7257600,
            34729 * n5 / 80640 - 3418889 * n6 / 1995840,
            212378941 * n6 / 319334400)
        self.beta = (
            n / 2 - 2 * n2 / 3 + 37 * n3 / 96 - n4 / 360 - 81 * n5 / 512 + 96199 * n6 / 604800,
            n2 / 48 + n3 / 15 - 437 * n4 / 1440 + 46 * n5 / 105 - 1118711 * n6 / 3870720,
            17 * n3 / 480 - 37 * n4 / 840 - 209 * n5 / 4480 + 5569 * n6 / 90720,
            4397 * n4 / 161280 - 11 * n5 / 504 - 830251 * n6 / 7257600,
            4583 * n5 / 161280 - 108847 * n6 / 3991680,
            20648693 * n6 / 638668800)

    def forward(self, lat, lon):
        """Degrees to (easting, northing) in metres."""
        phi, lam = math.radians(lat), math.radians(lon) - self.lon0
        e = self.e
        s = math.sin(phi)
        t = math.sinh(math.atanh(s) - e * math.atanh(e * s))
        xi_p = math.atan2(t, math.cos(lam))
        eta_p = math.atanh(math.sin(lam) / math.sqrt(1 + t * t))
        xi, eta = xi_p, eta_p
        for j, a_j in enumerate(self.alpha, 1):
            xi += a_j * math.sin(2 * j * xi_p) * math.cosh(2 * j * eta_p)
            eta += a_j * math.cos(2 * j * xi_p) * math.sinh(2 * j * eta_p)
        return UTM_FALSE_EASTING + self.k0a * eta, self.fn + self.k0a * xi

    def inverse(self, x, y):
        """(easting, northing) in metres to degrees (lat, lon)."""
        xi = (y - self.fn) / self.k0a
        eta = (x - UTM_FALSE_EASTING) / self.k0a
        xi_p, eta_p = xi, eta
        for j, b_j in enumerate(self.beta, 1):
            xi_p -= b_j * math.sin(2 * j * xi) * math.cosh(2 * j * eta)
            eta_p -= b_j * math.cos(2 * j * xi) * math.sinh(2 * j * eta)
        tau_p = math.sin(xi_p) / math.sqrt(math.sinh(eta_p) ** 2 + math.cos(xi_p) ** 2)
        lam = math.atan2(math.sinh(eta_p), math.cos(xi_p))
        e, e2 = self.e, self.e ** 2
        tau = tau_p
        for _ in range(5):
            sigma = math.sinh(e * math.atanh(e * tau / math.sqrt(1 + tau * tau)))
            tau_i = tau * math.sqrt(1 + sigma * sigma) - sigma * math.sqrt(1 + tau * tau)
            tau += ((tau_p - tau_i) / math.sqrt(1 + tau_i * tau_i)
                    * (1 + (1 - e2) * tau * tau) / ((1 - e2) * math.sqrt(1 + tau * tau)))
        return math.degrees(math.atan(tau)), math.degrees(lam + self.lon0)


# ---- synthetic ground ----------------------------------------------------

def synthetic_xy(lat, lon):
    """Degrees to metres on synthetic ground: a nautical mile to the minute."""
    return lon * M_PER_DEGREE, lat * M_PER_DEGREE


def synthetic_latlon(x, y):
    return y / M_PER_DEGREE, x / M_PER_DEGREE


# ---- the geodata ---------------------------------------------------------

def geodata_path(name):
    return os.path.join(store.GEODATA_DIR, store.check_name(name, "geodata") + ".yaml")


def names():
    """Every geodata on disk, by name."""
    return store.listing(store.GEODATA_DIR)


class Geodata:
    """One geodata, loaded: its kind, its extent, and the projection between
    a nodeset's degrees and the metres the loss model works in."""

    def __init__(self, name, data, path=None):
        self.name = name
        self.data = data
        self.path = path
        self.manifest = None
        self.pack_manifest_hash = None
        self.tm = None
        self.pack_dir = None
        if PACK in data:
            base = os.path.dirname(os.path.abspath(path)) if path else store.GEODATA_DIR
            self.pack_dir = os.path.normpath(os.path.join(base, os.path.expanduser(str(data[PACK]))))
            manifest_path = os.path.join(self.pack_dir, MANIFEST)
            try:
                with open(manifest_path, "rb") as handle:
                    raw = handle.read()
                self.manifest = json.loads(raw.decode("utf-8"))
            except (OSError, ValueError) as err:
                raise store.StoreError("geodata %s: pack %s has no readable %s (%s)"
                                       % (name, self.pack_dir, MANIFEST, err)) from err
            self.pack_manifest_hash = hashlib.sha256(raw).hexdigest()
            epsg = (self.manifest.get("region") or {}).get("crs_epsg")
            if epsg is None:
                raise store.StoreError("geodata %s: the pack's manifest names no crs_epsg" % name)
            self.tm = TransverseMercator(*utm_zone_of(epsg))
        elif SYNTHETIC not in data:
            raise store.StoreError("geodata %s: neither `pack:` nor `synthetic:`" % name)

    @property
    def kind(self):
        return PACK if self.manifest is not None else SYNTHETIC

    @property
    def is_pack(self):
        return self.kind == PACK

    @property
    def content_hash(self):
        """What a loss table on this ground depends on, hashed: the geodata
        file's content, and for a pack its manifest. A cached table or
        coverage raster is good while this is unchanged."""
        text = json.dumps([self.data, self.pack_manifest_hash], sort_keys=True)
        return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]

    @property
    def crs_epsg(self):
        return self.manifest["region"]["crs_epsg"] if self.is_pack else None

    @property
    def exponent(self):
        return self.data[SYNTHETIC]["exponent"] if not self.is_pack else None

    @property
    def terrain(self):
        return self.data[SYNTHETIC]["terrain"] if not self.is_pack else None

    @property
    def extent_m(self):
        return self.data[SYNTHETIC]["extent_m"] if not self.is_pack else None

    @property
    def bbox(self):
        """The extent in degrees, [lon0, lat0, lon1, lat1]: a pack's region,
        synthetic ground's square around 0°, 0°."""
        if self.is_pack:
            return [float(v) for v in self.manifest["region"]["bbox"]]
        half = self.extent_m / 2.0 / M_PER_DEGREE
        return [-half, -half, half, half]

    @property
    def origin(self):
        """The centre (lat, lon): a pack's region's, synthetic ground's 0°, 0°."""
        lon0, lat0, lon1, lat1 = self.bbox
        return ((lat0 + lat1) / 2, (lon0 + lon1) / 2)

    def holds(self, lat, lon):
        lon0, lat0, lon1, lat1 = self.bbox
        return lat0 <= lat <= lat1 and lon0 <= lon <= lon1

    def to_xy(self, lat, lon):
        """Degrees to the ground's metres: a pack's easting and northing, or
        synthetic ground's nautical-mile metres."""
        if self.is_pack:
            return self.tm.forward(lat, lon)
        return synthetic_xy(lat, lon)

    def to_latlon(self, x, y):
        if self.is_pack:
            return self.tm.inverse(x, y)
        return synthetic_latlon(x, y)

    def distance_m(self, a, b):
        """Metres between two (lat, lon), measured in the ground's own plane."""
        ax, ay = self.to_xy(*a)
        bx, by = self.to_xy(*b)
        return math.hypot(bx - ax, by - ay)

    def as_dict(self):
        """What the page is told about the geodata."""
        out = {"name": self.name, "kind": self.kind, "origin": list(self.origin),
               "bbox": self.bbox}
        if self.is_pack:
            manifest, _ = without_nodes(self.manifest)
            out.update(pack=self.pack_dir, crs_epsg=self.crs_epsg,
                       pack_manifest_hash=self.pack_manifest_hash,
                       layers=[layer_kind(layer) for layer in manifest["layers"]],
                       licences=[{"source": str(n.get("source", "")),
                                  "notice": str(n.get("notice", ""))}
                                 for n in manifest.get("licenses") or () if isinstance(n, dict)])
        else:
            out.update(exponent=self.exponent, terrain=self.terrain, extent_m=self.extent_m)
        return out


def parse(data, where):
    """A geodata file's mapping, checked and filled out."""
    if not isinstance(data, dict):
        raise store.StoreError("%s: not geodata" % where)
    if PACK in data:
        if not data[PACK]:
            raise store.StoreError("%s: `pack:` names no directory" % where)
        return {PACK: str(data[PACK])}
    ground = data.get(SYNTHETIC)
    if not isinstance(ground, dict):
        raise store.StoreError("%s: geodata is `pack: <dir>` or "
                               "`synthetic: {terrain, exponent, extent_m}`" % where)
    terrain = str(ground.get("terrain") or "flat")
    if terrain not in TERRAINS:
        raise store.StoreError("%s: terrain is one of %s, not %r"
                               % (where, ", ".join(TERRAINS), terrain))
    extent = float(ground.get("extent_m", DEFAULT_EXTENT_M))
    if extent <= 0:
        raise store.StoreError("%s: extent_m must be above 0" % where)
    return {SYNTHETIC: {"terrain": terrain,
                        "exponent": float(ground.get("exponent", DEFAULT_EXPONENT)),
                        "extent_m": extent}}


def read(path, name=None, refuse_packs=None):
    """A geodata file, loaded.

    With `refuse_packs`, a sentence with one `%s` for the geodata's name, a
    pack is refused with that sentence before its manifest is looked for:
    the front passes it when there is no planner, because the reason a pack
    is unusable then is the missing planner, whatever state the pack
    directory is in.
    """
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle) or {}
    except (OSError, yaml.YAMLError) as err:
        raise store.StoreError("%s: %s" % (path, err)) from err
    name = name or os.path.splitext(os.path.basename(path))[0]
    data = parse(data, path)
    if refuse_packs and PACK in data:
        raise store.StoreError(refuse_packs % name)
    return Geodata(name, data, path)


def load(name, refuse_packs=None):
    path = geodata_path(name)
    if not os.path.isfile(path):
        raise store.StoreError("no geodata called %r" % name)
    return read(path, name, refuse_packs)


def dump(data):
    if PACK in data:
        return "pack: %s\n" % store.scalar(data[PACK])
    ground = data[SYNTHETIC]
    return ("synthetic:\n  terrain: %s\n  exponent: %s\n  extent_m: %s\n"
            % (ground["terrain"], store.scalar(ground["exponent"]),
               store.scalar(ground["extent_m"])))


def write(path, data, comment=None):
    """Write a geodata file; `comment` lines go first, each behind a `#`."""
    head = "".join("# %s\n" % line for line in (comment or "").splitlines())
    store.write_text(path, head + dump(parse(data, path)))


# ---- renaming and deleting ------------------------------------------------

def pack_of(path):
    """The pack directory a geodata file names, read without loading the
    pack; None for synthetic ground or a file that cannot be read."""
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle) or {}
    except (OSError, yaml.YAMLError):
        return None
    if not isinstance(data, dict) or not data.get(PACK):
        return None
    base = os.path.dirname(os.path.abspath(path))
    return os.path.normpath(os.path.join(base, os.path.expanduser(str(data[PACK]))))


def pack_users(pack_dir, but=None):
    """Everything else naming a pack directory: other geodata by name, runs
    and snapshots as `run <name>` and `snapshot <name>`. `but` is the
    geodata left out."""
    out = [other for other in names()
           if other != but and pack_of(geodata_path(other)) == pack_dir]
    for what, base in (("run", store.RUNS_DIR), ("snapshot", store.SNAPSHOTS_DIR)):
        if os.path.isdir(base):
            out += ["%s %s" % (what, entry) for entry in sorted(os.listdir(base))
                    if pack_of(os.path.join(base, entry, "geodata.yaml")) == pack_dir]
    return out


def _own_pack(name, pack):
    """Whether a pack is this geodata's alone to move or remove: a directory
    of `packs/` nothing else names."""
    return (pack is not None and os.path.isdir(pack)
            and os.path.dirname(pack) == os.path.normpath(packs_dir())
            and not pack_users(pack, but=name))


def rename(name, to):
    """Geodata by another name, its comment kept. Its pack directory is
    renamed with it when it is `packs/<name>` and nothing else names it."""
    src = geodata_path(name)
    if not os.path.isfile(src):
        raise store.StoreError("no geodata called %r" % name)
    dst = geodata_path(to)
    if os.path.exists(dst):
        raise store.StoreError("there is already a geodata called %r" % to)
    with open(src, encoding="utf-8") as handle:
        text = handle.read()
    comment = "\n".join(line[1:].strip() for line in text.splitlines() if line.startswith("#"))
    data = parse(yaml.safe_load(text) or {}, src)
    pack = pack_of(src)
    target = os.path.join(os.path.normpath(packs_dir()), to)
    if os.path.basename(pack or "") == name and _own_pack(name, pack) \
            and not os.path.exists(target):
        os.rename(pack, target)
        data = {PACK: os.path.relpath(target, os.path.dirname(os.path.abspath(dst)))}
    write(dst, data, comment)
    os.remove(src)


def delete(name):
    """Geodata gone, and its pack directory with it when that is in `packs/`
    and nothing else names it. What still names the pack, which keeps it;
    [] when it went or there was none."""
    src = geodata_path(name)
    if not os.path.isfile(src):
        raise store.StoreError("no geodata called %r" % name)
    pack = pack_of(src)
    users = pack_users(pack, but=name) if pack else []
    own = _own_pack(name, pack)
    os.remove(src)
    if own:
        shutil.rmtree(pack)
    return users


def write_copy(gd, path):
    """Write loaded geodata to another place (a run's or a snapshot's
    `geodata.yaml`), its pack path re-based so it still names the same pack."""
    data = copy.deepcopy(gd.data)
    if gd.is_pack:
        data[PACK] = os.path.relpath(gd.pack_dir, os.path.dirname(os.path.abspath(path)))
    write(path, data, "geodata %s" % gd.name)


def rebase_text(text, src_dir, dst_dir):
    """A geodata file's text as it reads from another directory: a pack path
    re-based, synthetic ground unchanged. Works on the file without loading
    the pack."""
    data = yaml.safe_load(text) or {}
    if PACK in data:
        pack = os.path.normpath(os.path.join(src_dir, str(data[PACK])))
        data[PACK] = os.path.relpath(pack, dst_dir)
    return dump(parse(data, "geodata"))


# ---- nodes are never ground ----------------------------------------------

def layer_kind(layer):
    return layer.get("kind") if isinstance(layer, dict) else str(layer)


def without_nodes(manifest):
    """A manifest with no `Nodes` layer and no notice for one, and the pack
    paths of the files those layers were. Nodes belong to nodesets, so a
    pack SIMesh keeps, draws or exports never carries a planner's baked-in
    deployed network."""
    out = copy.deepcopy(manifest)
    layers = out.get("layers") or []
    dropped = [layer.get("path") for layer in layers
               if layer_kind(layer) == NODES_LAYER and isinstance(layer, dict)]
    out["layers"] = [layer for layer in layers if layer_kind(layer) != NODES_LAYER]
    if isinstance(out.get("licenses"), list):
        out["licenses"] = [notice for notice in out["licenses"]
                           if not str((notice or {}).get("source", "")).startswith(NODES_NOTICE)]
    return out, [os.path.normpath(p) for p in dropped if p]


# ---- a SIMesh geodata pack -----------------------------------------------

def export_zip(gd, out):
    """Geodata as a SIMesh geodata pack, written to `out`, a binary stream
    that need not seek (a response being sent).

    The zip holds `geodata.yaml` at the top, its first line `# geodata
    <name>`; for a pack, the pack itself under `pack/`, which the yaml's
    `pack:` names. The pack goes without its `Nodes` layer, whose file and
    manifest entry and notice are left out, and without dot files.
    """
    head = "# geodata %s\n" % gd.name
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=EXPORT_LEVEL) as zf:
        if not gd.is_pack:
            zf.writestr(GEODATA_MEMBER, head + dump(gd.data))
            return
        zf.writestr(GEODATA_MEMBER, head + dump({PACK: PACK_MEMBER}))
        manifest, dropped = without_nodes(gd.manifest)
        zf.writestr("%s/%s" % (PACK_MEMBER, MANIFEST), json.dumps(manifest, indent=1))
        skip = set(dropped) | {MANIFEST}
        for top, dirs, files in os.walk(gd.pack_dir):
            dirs[:] = sorted(d for d in dirs if not d.startswith("."))
            for each in sorted(files):
                full = os.path.join(top, each)
                inner = os.path.relpath(full, gd.pack_dir)
                if each.startswith(".") or inner in skip:
                    continue
                zf.write(full, "%s/%s" % (PACK_MEMBER, inner.replace(os.sep, "/")))


def _manifest_root(members):
    """The directory inside a zip that holds `manifest.json`: '' for the
    root, or the one top-level directory that has it."""
    if MANIFEST in members:
        return ""
    tops = {m.split("/", 1)[0] for m in members if "/" in m}
    found = [t for t in tops if "%s/%s" % (t, MANIFEST) in members]
    if len(found) == 1:
        return found[0] + "/"
    raise store.StoreError("the zip holds neither %s nor a %s at its root or inside one "
                           "top-level directory" % (GEODATA_MEMBER, MANIFEST))


def _inside(inner, what):
    """A path inside a zip, normalised, refused when it would leave it."""
    inner = os.path.normpath(inner)
    if os.path.isabs(inner) or inner == ".." or inner.startswith("../"):
        raise store.StoreError("%s leaves the zip" % what)
    return inner


def _named(text):
    """The name in a `# geodata <name>` first line, or None."""
    first = text.split("\n", 1)[0].strip()
    if first.startswith("#"):
        words = first[1:].split()
        if len(words) == 2 and words[0] == "geodata" and store.NAME_RE.match(words[1]):
            return words[1]
    return None


def zip_name(zf):
    """The name a zip gives its geodata: a SIMesh geodata pack's `# geodata`
    line, or a bare planner pack's manifest `name` made usable; None when it
    gives none."""
    members = [info.filename for info in zf.infolist() if not info.is_dir()]
    if GEODATA_MEMBER in members:
        return _named(zf.read(GEODATA_MEMBER).decode("utf-8", "replace"))
    try:
        manifest = json.loads(zf.read(_manifest_root(members) + MANIFEST).decode("utf-8"))
    except (store.StoreError, ValueError, KeyError):
        return None
    got = store.slug(manifest.get("name") if isinstance(manifest, dict) else None, "")
    return got or None


def import_zip(zip_path, name=None, packs=None):
    """A zip as new geodata: a SIMesh geodata pack (`geodata.yaml` at the
    top, and for a pack the pack directory its `pack:` names inside the zip),
    or a bare planner pack (a `manifest.json` at the top or inside one
    directory), which becomes pack geodata. `name` is the new geodata's; by
    default the one the zip gives. Returns the loaded geodata.

    A pack is expanded into `packs/<name>/`. Its members go under a `.part-`
    sibling first, the manifest is read and checked there, a `Nodes` layer's
    files and manifest entry are dropped, and only then is it renamed into
    place, so a pack directory that exists is one that was checked. A member
    that would land outside the pack is refused.
    """
    try:
        with zipfile.ZipFile(zip_path) as zf:
            name = store.check_name(name or zip_name(zf) or "", "geodata")
            path = geodata_path(name)
            if os.path.exists(path):
                raise store.StoreError("there is already geodata called %r" % name)
            members = [info.filename for info in zf.infolist() if not info.is_dir()]
            if GEODATA_MEMBER not in members:
                _expand_pack(zf, _manifest_root(members), name, packs)
                return load(name)
            data = parse(yaml.safe_load(zf.read(GEODATA_MEMBER).decode("utf-8")) or {},
                         GEODATA_MEMBER)
            if SYNTHETIC in data:
                write(path, data, "imported geodata %s" % name)
                return load(name)
            root = _inside(data[PACK], "the geodata's pack")
            root = "" if root == "." else root + "/"
            if root + MANIFEST not in members:
                raise store.StoreError("the geodata names pack %s, and the zip has no %s there"
                                       % (data[PACK], MANIFEST))
            _expand_pack(zf, root, name, packs)
            return load(name)
    except zipfile.BadZipFile as err:
        raise store.StoreError("not a zip: %s" % err) from err
    except yaml.YAMLError as err:
        raise store.StoreError("%s: %s" % (GEODATA_MEMBER, err)) from err


def _expand_pack(zf, root, name, packs=None):
    """The pack under `root` in an open zip, into `packs/<name>/`, and the
    geodata file that names it."""
    packs = packs or packs_dir()
    dest = os.path.join(packs, name)
    if os.path.exists(dest):
        raise store.StoreError("there is already a pack called %r in %s" % (name, packs))
    os.makedirs(packs, exist_ok=True)
    part = os.path.join(packs, PART_PREFIX + name)
    shutil.rmtree(part, ignore_errors=True)
    try:
        for info in zf.infolist():
            if info.is_dir() or not info.filename.startswith(root):
                continue
            inner = _inside(info.filename[len(root):], "member %s" % info.filename)
            target = os.path.join(part, inner)
            os.makedirs(os.path.dirname(target), exist_ok=True)
            with zf.open(info) as src, open(target, "wb") as dst:
                shutil.copyfileobj(src, dst, CHUNK)
        manifest_path = os.path.join(part, MANIFEST)
        try:
            with open(manifest_path, encoding="utf-8") as handle:
                manifest = json.load(handle)
            region = manifest["region"]
            utm_zone_of(region["crs_epsg"])
            if len(region["bbox"]) != 4:
                raise ValueError("bbox is not four numbers")
        except (OSError, ValueError, KeyError, TypeError, store.StoreError) as err:
            raise store.StoreError("the pack's %s is not usable: %s" % (MANIFEST, err)) from err
        kept, dropped = without_nodes(manifest)
        if kept != manifest:
            for inner in dropped:
                with contextlib.suppress(OSError):
                    os.remove(os.path.join(part, _inside(inner, "the Nodes layer")))
            store.write_text(manifest_path, json.dumps(kept, indent=1))
        os.rename(part, dest)
    finally:
        shutil.rmtree(part, ignore_errors=True)
    write(geodata_path(name), {PACK: os.path.relpath(dest, store.GEODATA_DIR)},
          "imported pack %s" % manifest.get("name", name))
