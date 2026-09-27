"""A nodeset: which nodes stand where, what they run, and how they are set.

    testbed/nodesets/<name>.yaml

    nodes:
      gw-alex: { id: 1, lat: 52.5219, lon: 13.4132, height_m: 38, height_from: roof,
                 antenna: { gain_dbi: 5 }, device: stable, role: transport,
                 radio: { freq_mhz: 869.525, sf: 8, bw_khz: 125, cr: 5, tx_dbm: 14 },
                 tags: [gateway] }
      n017: { id: 2, lat: 52.5301, lon: 13.4018, height_m: 15, height_from: assumed,
              antenna: { gain_dbi: 2 }, device: dev, tags: [rooftop] }
    offsets:
      - { between: [gw-alex, n017], db: 40, note: "wall, measured 2026-09-20" }

A node is its position, its antenna's height above the ground under it and
where that figure came from (`measured`, `roof`, `raster`, `assumed`), its
antenna gain, its tags, and three **declared settings** the page and the
medium read without running anything:

- `device`: what it runs, a device reference (devices.py): a catalogue
  (`stable`, `dev`), a package, a stamp, a local device or a path;
- `role`: `transport` or `client`, or absent for the firmware's own default;
- `radio`: slot 0's carrier, spreading factor, bandwidth, coding rate,
  transmit power, and optionally sync word and preamble; absent, the radio
  is left as the firmware starts it.

The declared settings are applied at setup, in the device's own language, by
its kind (kinds.Kind.declared), before any script's `setup`.

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
import os

import yaml

import stations as stations_module
import store

HEIGHT_FROM = ("measured", "roof", "raster", "assumed")
ROLES = ("transport", "client")
DEFAULT_HEIGHT_M = 2.0
DEFAULT_DEVICE = "stable"
# The radio's keys, in the order the file spells them, and what each one is.
RADIO_KEYS = (("freq_mhz", float), ("sf", int), ("bw_khz", float), ("cr", int),
              ("tx_dbm", float), ("sync", int), ("preamble", int))
# The calling channel of the EU 868 plan at SF8, 125 kHz, 14 dBm.
DEFAULT_RADIO = {"freq_mhz": 869.525, "sf": 8, "bw_khz": 125.0, "cr": 5, "tx_dbm": 14.0}


def nodeset_path(name):
    return os.path.join(store.NODESETS_DIR, store.check_name(name, "nodeset") + ".yaml")


def names():
    """Every nodeset on disk, by name."""
    return store.listing(store.NODESETS_DIR)


def blank():
    return {"nodes": {}, "offsets": []}


def node_record(node_id, lat, lon, height_m=DEFAULT_HEIGHT_M, height_from="assumed",
                gain_dbi=0.0, device=DEFAULT_DEVICE, role=None, radio=None, tags=()):
    """One node, in the shape the file and every caller use."""
    return {"id": int(node_id), "lat": float(lat), "lon": float(lon),
            "height_m": float(height_m), "height_from": str(height_from),
            "antenna": {"gain_dbi": float(gain_dbi)}, "device": str(device),
            "role": role, "radio": copy.deepcopy(radio) if radio else None,
            "tags": list(tags)}


# ---- the file ------------------------------------------------------------

def parse_radio(radio, where):
    """A radio mapping checked: its keys known, each of its kind. None or
    an empty mapping is no radio."""
    if radio in (None, {}):
        return None
    if not isinstance(radio, dict):
        raise store.StoreError("%s: radio is a mapping of %s"
                               % (where, ", ".join(k for k, _ in RADIO_KEYS)))
    known = dict(RADIO_KEYS)
    out = {}
    for key, value in radio.items():
        if key not in known:
            raise store.StoreError("%s: radio has no %r (it takes %s)"
                                   % (where, key, ", ".join(k for k, _ in RADIO_KEYS)))
        if value is None:
            continue
        try:
            out[key] = int(value, 0) if known[key] is int and isinstance(value, str) \
                else known[key](value)
        except (TypeError, ValueError) as err:
            raise store.StoreError("%s: radio %s is a number, not %r" % (where, key, value)) from err
    return {k: out[k] for k, _ in RADIO_KEYS if k in out} or None


def parse_role(role, where):
    if role in (None, ""):
        return None
    if role not in ROLES:
        raise store.StoreError("%s: role is one of %s, not %r" % (where, ", ".join(ROLES), role))
    return str(role)


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
        out["nodes"][name] = node_record(
            node_id, lat, lon, node.get("height_m", DEFAULT_HEIGHT_M), height_from,
            (node.get("antenna") or {}).get("gain_dbi", 0.0),
            str(node.get("device") or DEFAULT_DEVICE), parse_role(node.get("role"), here),
            parse_radio(node.get("radio"), here), check_tags(node.get("tags")))
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


def dump_radio(radio):
    parts = []
    for key, kind in RADIO_KEYS:
        if key not in radio:
            continue
        value = radio[key]
        parts.append("%s: %s" % (key, "0x%02x" % value if key == "sync" else store.scalar(value)))
    return "{ %s }" % ", ".join(parts)


def dump_node(name, node):
    """One node as one line, its keys in the order the format documents."""
    parts = ["id: %d" % node["id"], "lat: %s" % store.scalar(node["lat"]),
             "lon: %s" % store.scalar(node["lon"]),
             "height_m: %s" % store.scalar(node["height_m"]),
             "height_from: %s" % node["height_from"],
             "antenna: { gain_dbi: %s }" % store.scalar(node["antenna"]["gain_dbi"]),
             "device: %s" % store.scalar(node["device"])]
    if node.get("role"):
        parts.append("role: %s" % node["role"])
    if node.get("radio"):
        parts.append("radio: %s" % dump_radio(node["radio"]))
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


def carrier_mhz(node):
    """A node's declared carrier in MHz, or None when it declares no radio."""
    radio = node.get("radio") or {}
    return radio.get("freq_mhz")


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
        """Whether any node stands inside [lon0, lat0, lon1, lat1]."""
        lon0, lat0, lon1, lat1 = bbox
        return any(lat0 <= n["lat"] <= lat1 and lon0 <= n["lon"] <= lon1
                   for n in self.nodes.values())

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
                 gain_dbi=None, device=None, role=None, radio=None, tags=None):
        """Change any of a node's facts; None leaves one as it is. An empty
        `role` or `radio` ("" or {}) takes it away.

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
        if gain_dbi is not None:
            node["antenna"]["gain_dbi"] = float(gain_dbi)
        if device is not None:
            if not str(device).strip():
                raise store.StoreError("a node's device is a device reference, not empty")
            node["device"] = str(device).strip()
        if role is not None:
            node["role"] = parse_role(role, "node %s" % name)
        if radio is not None:
            node["radio"] = parse_radio(radio, "node %s" % name)
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


def summary(name):
    """What a listing says of one nodeset: its node count, extent and tags."""
    ns = load(name)
    lats = [n["lat"] for n in ns.nodes.values()]
    lons = [n["lon"] for n in ns.nodes.values()]
    return {"name": name, "nodes": len(ns.nodes),
            "bbox": [min(lons), min(lats), max(lons), max(lats)] if lats else None,
            "tags": ns.tags()}


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


def _radio_with(power):
    """The default radio, at a transmit power a CSV stated."""
    if power is None:
        return copy.deepcopy(DEFAULT_RADIO)
    return dict(DEFAULT_RADIO, tx_dbm=float(power))


def import_sites_csv(path, height_m=DEFAULT_HEIGHT_M, device=DEFAULT_DEVICE, prefix="site"):
    """The planner optimiser's `sites.csv` as a nodeset.

    Columns `lat, lon, tx_power_dbm, surface_masl, cs_neighbours`. The file
    has no antenna height, so every site gets `height_m`, marked assumed.
    Every node gets the default radio, at the file's transmit power where it
    states one.
    """
    data = blank()
    for index, row in enumerate(_rows(path), 1):
        lat, lon = _number(row.get("lat")), _number(row.get("lon"))
        if lat is None or lon is None:
            continue
        name = "%s-%03d" % (prefix, index)
        data["nodes"][name] = node_record(
            len(data["nodes"]) + 1, lat, lon, height_m, "assumed", 0.0, device,
            radio=_radio_with(_number(row.get("tx_power_dbm"))))
    return data


def import_nodes_csv(path, height_m=15.0, device=DEFAULT_DEVICE):
    """The deployed-network CSV `planner nodes import` writes, as a nodeset.

    Header `id, name, kind, lat, lon, height_agl_m, tx_power_dbm,
    last_seen_unix`. A node is named after its own label, made usable (lower
    case, hyphens) and made unique; its kind becomes a tag. `height_agl_m`
    where the operator gave one is taken as measured; else `height_m`,
    marked assumed. Every node gets the default radio, at the file's
    transmit power where it states one.
    """
    data = blank()
    for row in _rows(path):
        lat, lon = _number(row.get("lat")), _number(row.get("lon"))
        if lat is None or lon is None:
            continue
        node_id = len(data["nodes"]) + 1
        base = store.slug(row.get("name"), "n%03d" % node_id)
        name, suffix = base, 2
        while name in data["nodes"]:
            tail = "-%d" % suffix
            name = base[:32 - len(tail)].rstrip("-") + tail
            suffix += 1
        height = _number(row.get("height_agl_m"))
        kind = store.slug(row.get("kind"), "")
        data["nodes"][name] = node_record(
            node_id, lat, lon, height if height is not None else height_m,
            "measured" if height is not None else "assumed", 0.0, device,
            radio=_radio_with(_number(row.get("tx_power_dbm"))), tags=[kind] if kind else [])
    return data
