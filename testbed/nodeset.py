"""A nodeset: which nodes stand where, with what antenna, and what they are.

    testbed/nodesets/<name>.yaml
    testbed/nodesets/<name>.py        its own setup, when it has one (script.py)

    nodes:
      gw-alex: { id: 1, lat: 52.5219, lon: 13.4132, height_m: 38, height_from: roof,
                 max_dbm: 27,
                 antenna: { type: panel_directional, azimuth_deg: 120, elevation_deg: -2 },
                 tags: [transport, gateway] }
      n017: { id: 2, lat: 52.5301, lon: 13.4018, height_m: 15, height_from: assumed,
              antenna: { type: whip_sma_quarter_wave }, tags: [rooftop] }
    offsets:
      - { between: [gw-alex, n017], db: 40, note: "wall, measured 2026-09-20" }

A node is its position, its antenna's height above the ground under it and
where that figure came from (`measured`, `roof`, `raster`, `assumed`), its
maximum power (`max_dbm`, dBm at the antenna connector, 22 when absent and
at most 27; above 22 the node has a front end, boards.py), its antenna
(antennas.py: a type of the catalogue, and a directional one's aim), and its
tags. A node has no board: every node is the one board boards.py
describes. Nothing in it is said to a station: scripts say everything, in
the node's terms, keyed off its tags.
Two tags mean something to the page and the analysis as well:

- a role's name (`transport`, `router`, `repeater`) is the node's role
  (`tag_role`), which the startup script tells it and the map rings;
- `no-radio` is a node with no radio: the startup script sets up every
  other node's, to globals.py's settings, and the page draws coverage and
  links for those alone.

What a node runs is not the nodeset's either: a script says it
(`firmware()`), so one nodeset is run on any firmware.

A nodeset names no geodata. It is offered on every geodata whose extent holds
one of its nodes (`inside`); synthetic ground lies at 0°, 0°.

A node's name is how everything refers to it: scripts, offsets, snapshots and
the page. Its id is its network identity, stored and editable: the station's
loopback address and MAC follow from it (`stations.bind_addr`), so changing
it moves the station, and anything already set up with its old `{id}` or
`{addr}` is stale. A new node takes the lowest id not in use.

**Offsets** are dB added to the computed loss of one pair, both ways, on any
geodata: where a measurement says the model is wrong, and by how much. They
are a layer over the loss table, applied when the medium is given it, so the
table itself (`geometry_hash`: which nodes, where, how high) is cached
without them and an offset never forces a recompute.
"""

import copy
import csv
import hashlib
import json
import math
import os

import yaml

import antennas as antennas_module
import boards as boards_module
import stations as stations_module
import store

HEIGHT_FROM = ("measured", "roof", "raster", "assumed")
# The tags that are a role: a node carrying one forwards for the others.
ROLE_TAGS = ("transport", "router", "repeater")
NO_RADIO = "no-radio"
DEFAULT_HEIGHT_M = 2.0
MERGE_WITHIN_M = 5.0                # two layers' nodes this close are one node
EARTH_RADIUS_M = 6371008.8
KEEP = object()                     # set_node: a fact not given, left as it is


def nodeset_path(name):
    return os.path.join(store.NODESETS_DIR, store.check_name(name, "nodeset") + ".yaml")


def names():
    """Every nodeset on disk, by name."""
    return store.listing(store.NODESETS_DIR)


def blank():
    return {"nodes": {}, "offsets": []}


def node_record(node_id, lat, lon, height_m=DEFAULT_HEIGHT_M, height_from="assumed",
                antenna=None, tags=(), max_dbm=None):
    """One node, in the shape the file and every caller use: `max_dbm` is
    there only when the node states one."""
    out = {"id": int(node_id), "lat": float(lat), "lon": float(lon),
           "height_m": float(height_m), "height_from": str(height_from),
           "antenna": antennas_module.check(antenna, "a node"),
           "tags": list(tags)}
    max_dbm = boards_module.check(max_dbm, "a node")
    if max_dbm is not None:
        out["max_dbm"] = max_dbm
    return out


def tag_role(tags):
    """The role a node's tags give it, or None: the first role tag it carries."""
    return next((tag for tag in tags if tag in ROLE_TAGS), None)


def has_radio(node):
    return NO_RADIO not in node["tags"]


# ---- the file ------------------------------------------------------------

def check_tags(tags):
    tags = [str(tag) for tag in (tags or [])]
    for tag in tags:
        store.check_name(tag, "tag")
    return list(dict.fromkeys(tags))


def parse(data, where):
    """A nodeset file's mapping, checked and filled out.

    Two nodes under one id would be two processes answering the ether as
    one station and two sockets on one address, so that is refused here,
    as is an offset naming a node the nodeset does not have.
    """
    if not isinstance(data, dict):
        raise store.StoreError("%s: not a nodeset" % where)
    out = blank()
    ids = {}
    for name, node in (data.get("nodes") or {}).items():
        name = str(name)
        store.check_name(name, "node")
        if not isinstance(node, dict):
            raise store.StoreError("%s: node %s is not a mapping" % (where, name))
        try:
            node_id = int(node["id"])
            lat, lon = float(node["lat"]), float(node["lon"])
        except (KeyError, TypeError, ValueError) as err:
            raise store.StoreError("%s: node %s needs id, lat and lon" % (where, name)) from err
        if node_id in ids:
            raise store.StoreError("%s: nodes %s and %s share id %d"
                                   % (where, ids[node_id], name, node_id))
        check_id(node_id)
        ids[node_id] = name
        height_from = str(node.get("height_from") or "assumed")
        if height_from not in HEIGHT_FROM:
            raise store.StoreError("%s: node %s: height_from is one of %s, not %r"
                                   % (where, name, ", ".join(HEIGHT_FROM), height_from))
        here = "%s: node %s" % (where, name)
        if "device" in node:
            raise store.StoreError("%s: a node names no device: a script's firmware() says "
                                   "what each node runs" % here)
        for key in ("radio", "role"):
            if key in node:
                raise store.StoreError(
                    "%s: a node declares no %s: scripts/globals.py sets every radio, a "
                    "`no-radio` tag marks a node without one, and a role is a tag "
                    "(transport, router, repeater)" % (here, key))
        if "board" in node:
            raise store.StoreError("%s: a node has no board: every node is an SX1262, and "
                                   "`max_dbm` is its maximum power" % here)
        out["nodes"][name] = node_record(
            node_id, lat, lon, node.get("height_m", DEFAULT_HEIGHT_M), height_from,
            antennas_module.check(node.get("antenna"), here), check_tags(node.get("tags")),
            boards_module.check(node.get("max_dbm"), here))
    for offset in data.get("offsets") or []:
        try:
            a, b = (str(n) for n in list(offset["between"])[:2])
            db = float(offset.get("db", 0))
        except (KeyError, TypeError, ValueError) as err:
            raise store.StoreError("%s: an offset is { between: [a, b], db, note? }" % where) from err
        for end in (a, b):
            if end not in out["nodes"]:
                raise store.StoreError("%s: offset names %s, which is not a node" % (where, end))
        entry = {"between": [a, b], "db": db}
        if offset.get("note"):
            entry["note"] = str(offset["note"])
        out["offsets"].append(entry)
    return out


def check_id(node_id):
    limit = stations_module.max_node_id()
    if not 1 <= node_id <= limit:
        raise store.StoreError("a node id is 1 to %d on the network %s, not %d"
                               % (limit, stations_module.NET, node_id))


def read(path):
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle) or {}
    except (OSError, yaml.YAMLError) as err:
        raise store.StoreError("%s: %s" % (path, err)) from err
    return parse(data, path)


def dump_antenna(antenna):
    parts = ["type: %s" % antenna["type"]]
    for key in ("azimuth_deg", "elevation_deg"):
        if key in antenna:
            parts.append("%s: %s" % (key, store.scalar(antenna[key])))
    return "{ %s }" % ", ".join(parts)


def dump_node(name, node):
    """One node as one line, its keys in the order the format documents."""
    parts = ["id: %d" % node["id"], "lat: %s" % store.scalar(node["lat"]),
             "lon: %s" % store.scalar(node["lon"]),
             "height_m: %s" % store.scalar(node["height_m"]),
             "height_from: %s" % node["height_from"]]
    if node.get("max_dbm") is not None:
        parts.append("max_dbm: %s" % store.scalar(node["max_dbm"]))
    parts.append("antenna: %s" % dump_antenna(node["antenna"]))
    parts.append("tags: %s" % store.flow(node["tags"]))
    return "  %s: { %s }" % (name, ", ".join(parts))


def dump(data, comment=None):
    """The nodeset as the file, in the shape the format documents.

    Hand-composed rather than emitted by the YAML writer so that a node
    stays one readable line, in id order: the file is meant to be opened and
    edited, not only round-tripped. `comment` lines go first, each behind a `#`.
    """
    out = ["# %s" % line for line in (comment or "").splitlines()]
    nodes = data.get("nodes") or {}
    out.append("nodes:" if nodes else "nodes: {}")
    out += [dump_node(name, node)
            for name, node in sorted(nodes.items(), key=lambda kv: kv[1]["id"])]
    offsets = data.get("offsets") or []
    out.append("offsets:" if offsets else "offsets: []")
    for offset in offsets:
        note = ", note: %s" % store.scalar(offset["note"]) if offset.get("note") else ""
        out.append("  - { between: [%s, %s], db: %s%s }"
                   % (offset["between"][0], offset["between"][1], store.scalar(offset["db"]), note))
    return "\n".join(out) + "\n"


def write(path, data, comment=None):
    store.write_text(path, dump(data, comment))


def comment_of(path):
    """The comment lines a nodeset file starts with, without their `#`: what
    a save over it keeps, since the page edits the nodes and not the prose."""
    lines = []
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if not line.startswith("#"):
                    break
                text = line.rstrip("\n")[1:]
                lines.append(text[1:] if text.startswith(" ") else text)
    except OSError:
        return None
    return "\n".join(lines) or None


def geometry_hash(data):
    """What the loss table depends on, hashed: which nodes, where and how
    high. Sixteen hex digits of SHA-256 over a canonical spelling of those
    alone, so the same geometry always has the same key."""
    nodes = sorted(
        (name, round(node["lat"], 7), round(node["lon"], 7), round(node["height_m"], 3))
        for name, node in (data.get("nodes") or {}).items())
    text = json.dumps({"nodes": nodes}, separators=(",", ":"))
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


# ---- the loaded nodeset --------------------------------------------------

class Nodeset:
    """One nodeset being looked at or edited, and whether it is dirty.

    Everything that changes the nodeset goes through a method here, and
    every one of them sets `dirty`, which is the whole of the unsaved-changes
    rule. `path` is where Save writes; a nodeset copied into a run has its
    run's path and no name of its own until saved as one.
    """

    def __init__(self, name, data, path=None):
        self.name = name
        self.data = data
        self.path = path or (nodeset_path(name) if name else None)
        self.dirty = False

    @property
    def nodes(self):
        """Node name -> its record."""
        return self.data["nodes"]

    @property
    def offsets(self):
        return self.data["offsets"]

    def node(self, name):
        node = self.nodes.get(name)
        if node is None:
            raise store.StoreError("no node called %r" % name)
        return node

    def by_id(self, node_id):
        """The name of the node with this id, or None."""
        return next((name for name, node in self.nodes.items() if node["id"] == node_id), None)

    def geometry_hash(self):
        return geometry_hash(self.data)

    def offset(self, a, b):
        """The dB added between two nodes, 0 when there is none."""
        pair = {a, b}
        return sum(o["db"] for o in self.offsets if set(o["between"]) == pair)

    def inside(self, bbox):
        """How many nodes stand inside [lon0, lat0, lon1, lat1]."""
        lon0, lat0, lon1, lat1 = bbox
        return sum(1 for n in self.nodes.values()
                   if lat0 <= n["lat"] <= lat1 and lon0 <= n["lon"] <= lon1)

    def tags(self):
        """Every tag a node carries, with how many carry it."""
        count = {}
        for node in self.nodes.values():
            for tag in node["tags"]:
                count[tag] = count.get(tag, 0) + 1
        return dict(sorted(count.items()))

    def next_id(self):
        """The lowest station id this nodeset is not already using.

        A removed node's id comes back, because nothing is left that answers
        to it; a node keeps its id for as long as it exists, so its address
        and directory stay put.
        """
        taken = {node["id"] for node in self.nodes.values()}
        limit = stations_module.max_node_id()
        for candidate in range(1, limit + 1):
            if candidate not in taken:
                return candidate
        raise store.StoreError("a nodeset holds at most %d nodes on the network %s"
                               % (limit, stations_module.NET))

    # ---- edits -----------------------------------------------------------

    def add_node(self, name, lat, lon, **fields):
        """Place a new node; `fields` are any of `set_node`'s, and a node
        given no id takes the lowest free one."""
        store.check_name(name, "node")
        if name in self.nodes:
            raise store.StoreError("there is already a node called %r" % name)
        self.nodes[name] = node_record(self.next_id(), lat, lon)
        try:
            self.set_node(name, **fields)
        except store.StoreError:
            del self.nodes[name]
            raise
        self.dirty = True
        return self.nodes[name]

    def remove_node(self, name):
        self.node(name)
        del self.nodes[name]
        self.data["offsets"] = [o for o in self.offsets if name not in o["between"]]
        self.dirty = True

    def rename_node(self, name, new):
        """Give a node another name, and carry its offsets over."""
        node = self.node(name)
        store.check_name(new, "node")
        if new in self.nodes:
            raise store.StoreError("there is already a node called %r" % new)
        self.data["nodes"] = {(new if key == name else key): value
                              for key, value in self.nodes.items()}
        for offset in self.offsets:
            offset["between"] = [new if end == name else end for end in offset["between"]]
        self.dirty = True
        return node

    def move_node(self, name, lat, lon):
        node = self.node(name)
        node["lat"], node["lon"] = float(lat), float(lon)
        self.dirty = True

    def set_node(self, name, id=None, lat=None, lon=None, height_m=None, height_from=None,
                 antenna=None, tags=None, max_dbm=KEEP):
        """Change any of a node's facts; None leaves one as it is, except
        `max_dbm`, which None clears (the node then sends at most 22 dBm)
        and which is left as it is when not given.

        Returns True when the id changed: the station then has a new address,
        and one with state must be restarted for it to take.
        """
        node = self.node(name)
        changed_id = False
        if id is not None and int(id) != node["id"]:
            node_id = int(id)
            check_id(node_id)
            other = self.by_id(node_id)
            if other is not None:
                raise store.StoreError("id %d is already %s's" % (node_id, other))
            node["id"] = node_id
            changed_id = True
        if lat is not None:
            node["lat"] = float(lat)
        if lon is not None:
            node["lon"] = float(lon)
        if height_m is not None:
            node["height_m"] = float(height_m)
        if height_from is not None:
            if height_from not in HEIGHT_FROM:
                raise store.StoreError("height_from is one of %s" % ", ".join(HEIGHT_FROM))
            node["height_from"] = height_from
        if antenna is not None:
            node["antenna"] = antennas_module.check(antenna, "node %s" % name)
        if max_dbm is not KEEP:
            max_dbm = boards_module.check(max_dbm, "node %s" % name)
            if max_dbm is None:
                node.pop("max_dbm", None)
            else:
                node["max_dbm"] = max_dbm
        if tags is not None:
            node["tags"] = check_tags(tags)
        self.dirty = True
        return changed_id

    def set_offset(self, a, b, db, note=None):
        """Set the dB added between two nodes; 0 removes it."""
        self.node(a), self.node(b)
        pair = {a, b}
        self.data["offsets"] = [o for o in self.offsets if set(o["between"]) != pair]
        if db:
            entry = {"between": [a, b], "db": float(db)}
            if note:
                entry["note"] = str(note)
            self.offsets.append(entry)
        self.dirty = True

    # ---- saving ----------------------------------------------------------

    def save(self):
        if not self.path:
            raise store.StoreError("this nodeset has no file yet: save it as a name")
        write(self.path, self.data)
        self.dirty = False

    def save_as(self, name):
        """Write the nodeset under a new name, which becomes this one's."""
        path = nodeset_path(name)
        if os.path.exists(path):
            raise store.StoreError("there is already a nodeset called %r" % name)
        self.name, self.path = name, path
        self.save()

    def copy(self, path=None):
        """An independent copy, for a run to edit without touching this one."""
        return Nodeset(self.name, copy.deepcopy(self.data), path or self.path)

    def as_dict(self):
        """What the page is told about the nodeset."""
        return {"name": self.name, "dirty": self.dirty,
                "geometry_hash": self.geometry_hash(),
                "nodes": copy.deepcopy(self.nodes),
                "offsets": copy.deepcopy(self.offsets)}


def load(name):
    path = nodeset_path(name)
    if not os.path.isfile(path):
        raise store.StoreError("no nodeset called %r" % name)
    return Nodeset(name, read(path), path)


def open_path(path, name=None):
    """A nodeset file anywhere (a run's or a snapshot's copy)."""
    return Nodeset(name, read(path), path)


def create(name):
    """An empty nodeset, written at once so it has a file."""
    path = nodeset_path(name)
    if os.path.exists(path):
        raise store.StoreError("there is already a nodeset called %r" % name)
    ns = Nodeset(name, blank(), path)
    ns.save()
    return ns


def delete(name):
    """A nodeset gone, with its own setup script (`nodesets/<name>.py`)."""
    path = nodeset_path(name)
    if not os.path.isfile(path):
        raise store.StoreError("no nodeset called %r" % name)
    os.remove(path)
    setup = os.path.splitext(path)[0] + ".py"
    if os.path.isfile(setup):
        os.remove(setup)


def summary(name):
    """What a listing says of one nodeset: its node count, extent and tags."""
    ns = load(name)
    lats = [n["lat"] for n in ns.nodes.values()]
    lons = [n["lon"] for n in ns.nodes.values()]
    return {"name": name, "nodes": len(ns.nodes),
            "bbox": [min(lons), min(lats), max(lons), max(lats)] if lats else None,
            "tags": ns.tags()}


def unique_name(base, taken):
    """`base`, or `base-2`, `base-3`… cut to a name's length, whichever
    `taken` does not hold."""
    name = base[:32].rstrip("-")
    suffix = 2
    while name in taken:
        tail = "-%d" % suffix
        name = base[:32 - len(tail)].rstrip("-") + tail
        suffix += 1
    return name


def lowest_free_id(taken):
    node_id = 1
    while node_id in taken:
        node_id += 1
    return node_id


def distance_m(a, b):
    """Metres between two nodes, on a sphere of the Earth's mean radius:
    at the few metres merging asks about, the ellipsoid changes nothing."""
    lat1, lon1, lat2, lon2 = (math.radians(v) for v in (a["lat"], a["lon"], b["lat"], b["lon"]))
    return EARTH_RADIUS_M * math.hypot((lon2 - lon1) * math.cos((lat1 + lat2) / 2), lat2 - lat1)


def merge(layers):
    """Nodesets shown together as one, as Save visible as writes it.

    `layers` is [(layer name, file mapping)], the top of the Layers panel
    first. One layer is its own mapping unchanged. Of several, every node
    keeps its tags and gains its layer's name as one; a node within
    MERGE_WITHIN_M of a node of an earlier layer is that node, the earlier
    layer's, and is left out; a name an earlier layer took gets the layer's
    name appended (and a number, should that be taken too); an id taken gets
    the lowest free one. An offset comes along where both its ends do.
    """
    layers = [(layer, parse(data, "layer %s" % layer)) for layer, data in layers]
    if len(layers) == 1:
        return layers[0][1]
    out = blank()
    ids = set()
    for layer, data in layers:
        earlier = list(out["nodes"].values())
        renamed = {}
        for name, node in sorted(data["nodes"].items(), key=lambda item: item[1]["id"]):
            if any(distance_m(node, other) <= MERGE_WITHIN_M for other in earlier):
                continue
            new = name if name not in out["nodes"] else unique_name("%s-%s" % (name, layer),
                                                                    out["nodes"])
            node = copy.deepcopy(node)
            if node["id"] in ids:
                node["id"] = lowest_free_id(ids)
            node["tags"] = check_tags(node["tags"] + [layer])
            out["nodes"][new] = node
            ids.add(node["id"])
            renamed[name] = new
        for offset in data["offsets"]:
            a, b = offset["between"]
            if a in renamed and b in renamed:
                out["offsets"].append(dict(copy.deepcopy(offset), between=[renamed[a], renamed[b]]))
    return out


# ---- imports -------------------------------------------------------------

def _rows(path):
    """A planner CSV's rows as dicts. A header written as a `#` comment
    (sites.csv's) is a header all the same."""
    with open(path, encoding="utf-8", newline="") as handle:
        lines = [line for line in handle if line.strip()]
    if lines and lines[0].startswith("#"):
        lines[0] = lines[0].lstrip("#").strip() + "\n"
    reader = csv.DictReader(lines)
    reader.fieldnames = [field.strip() for field in reader.fieldnames or ()]
    return [{key: (value or "").strip() for key, value in row.items() if key} for row in reader]


def _number(text):
    try:
        return float(text) if text not in (None, "") else None
    except ValueError:
        return None


def _board_at(power):
    """The maximum power of a node whose CSV row states a transmit power:
    that power, held to the range the board has."""
    return None if power is None else \
        max(float(boards_module.CHIP_DBM[0]), min(float(power), float(boards_module.FEM_MAX_DBM)))


def import_sites_csv(path, height_m=DEFAULT_HEIGHT_M, prefix="site"):
    """The planner optimiser's `sites.csv` as a nodeset.

    Columns `lat, lon, tx_power_dbm, surface_masl, cs_neighbours`. The file
    has no antenna height, so every site gets `height_m`, marked assumed.
    A transmit power the file states is the node's maximum power.
    """
    data = blank()
    for index, row in enumerate(_rows(path), 1):
        lat, lon = _number(row.get("lat")), _number(row.get("lon"))
        if lat is None or lon is None:
            continue
        name = "%s-%03d" % (prefix, index)
        data["nodes"][name] = node_record(
            len(data["nodes"]) + 1, lat, lon, height_m, "assumed",
            max_dbm=_board_at(_number(row.get("tx_power_dbm"))))
    return data


def from_imported(rows, source, height_m=15.0):
    """Nodes `planner-job nodes-import` read from a public node map, as a
    nodeset.

    Each row is {label, lat, lon, kind, position, height_m?}. A node is named
    after its label, made usable and unique, else `<source>-<n>`; it
    stands at the row's height where the source gives one
    (measured) and else at `height_m` (assumed), and is tagged with the source, its kind and its position's quality
    (`position-gps`, `position-fixed`, …).
    """
    data = blank()
    for row in rows:
        node_id = len(data["nodes"]) + 1
        name = unique_name(store.slug(row.get("label"), "%s-%d" % (source, node_id)), data["nodes"])
        height = row.get("height_m")
        tags = [source, store.slug(row.get("kind"), ""),
                store.slug("position-%s" % row.get("position"), "") if row.get("position") else ""]
        data["nodes"][name] = node_record(
            node_id, row["lat"], row["lon"], height if height is not None else height_m,
            "measured" if height is not None else "assumed",
            tags=check_tags(t for t in tags if t))
    return data


def import_nodes_csv(path, height_m=15.0):
    """The deployed-network CSV `planner nodes import` writes, as a nodeset.

    Header `id, name, kind, lat, lon, height_agl_m, tx_power_dbm,
    last_seen_unix`. A node is named after its own label, made usable (lower
    case, hyphens) and made unique; its kind becomes a tag. `height_agl_m`
    where the operator gave one is taken as measured; else `height_m`,
    marked assumed. A transmit power the file states is the node's maximum
    power.
    """
    data = blank()
    for row in _rows(path):
        lat, lon = _number(row.get("lat")), _number(row.get("lon"))
        if lat is None or lon is None:
            continue
        node_id = len(data["nodes"]) + 1
        name = unique_name(store.slug(row.get("name"), "n%03d" % node_id), data["nodes"])
        height = _number(row.get("height_agl_m"))
        kind = store.slug(row.get("kind"), "")
        data["nodes"][name] = node_record(
            node_id, lat, lon, height if height is not None else height_m,
            "measured" if height is not None else "assumed",
            max_dbm=_board_at(_number(row.get("tx_power_dbm"))), tags=[kind] if kind else [])
    return data
