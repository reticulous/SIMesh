#!/usr/bin/env python3
"""Devices: the station builds a node can run.

```
simesh ── GET <catalogue>/index.html ─────────────────────────► site or directory
simesh: newest KEEP <slug>_hw-simesh-<arch>_<stamp>.zip per entry, this machine's arch only
simesh ── GET <catalogue>/<slug>_hw-simesh-<arch>_<stamp>.zip ─► site   once per stamp
simesh: unzip to devices/<catalogue>/.part-…, check node.yaml, rename to
        devices/<catalogue>/<slug>_hw-simesh-<arch>_<stamp>/
page ── POST /api/devices/import?name=<zip name> (the zip) ──► front
front: the same checks, into devices/imported/<slug>_hw-simesh-<arch>_<stamp>/,
       that name made from its node.yaml
node  device: stable ──► resolve() ──► {elf, fixed, tools, env, kind_type, stamp, name, …}
```

A **device file** is one station build ready to run: a zip holding an
executable, whatever it needs beside it, and a `node.yaml` saying what those
are (NODE.md is the spec). It is published in a catalogue exactly as a board
image is, named `<slug>_<entry>_<stamp>.zip` with `hw-simesh-<arch>` as the
entry, and listed in that catalogue's `index.html`; or imported on the page
under any name. The executable is native code dynamically linked against the
builder's C library, so a device runs only on a machine of the architecture
its `node.yaml` names.

**Where devices live.** `devices/<catalogue>/<slug>_<entry>_<stamp>/`, one
directory per package, holding what the zip held plus `origin.yaml` (where it
came from and when). Imported zips are the catalogue `imported`. The
catalogue is a level of its own because one `make-builds` run stamps every
catalogue it builds with the same datetime, so `stable` and `dev` packages of
one run share a filename and differ only in where they came from. A
directory is whole or absent: a package is unzipped under a `.part-` name,
checked, and renamed into place. Each entry keeps its newest `KEEP`
packages per catalogue, and a refresh fetches that many; older ones are
removed after a newer one lands. The listing shows the newest `KEEP` of each
catalogue.

**Local devices** are `devices/local/<name>.yaml`: a `node.yaml` that is not
in a package, its `elf`, `fixed` and `tools` paths relative to the file and
free to point anywhere. They are the developer loop: a build of one's own
tree, run in place, named once.

**What a node's `device:` names**, which `resolve` turns into paths:

- a catalogue name (`stable`, `dev`, `imported`, or any other fetched one):
  the newest package from it for this machine's architecture;
- a package, `<catalogue>/<slug>_hw-simesh-<arch>_<stamp>`: that package;
  its bare name will do while only one catalogue has it;
- a stamp: the package with that stamp, whichever catalogue it came from, as
  long as only one did;
- a local device's name;
- a path: a package directory (it holds `node.yaml`), or a workspace's
  `build.linux` (it holds `reticulous.elf` and `data_merged/`).

**What a device is called** on the page: `node.yaml`'s `name`, else its
project, catalogue and build time (`Reticulous dev 2026-09-25 03:50`); with
`stands_for`, it is shown as a virtual one of that (`virtual ESP32`).

**Where catalogues are**, for `refresh`: a URL ending in the catalogue's
directory (its `index.html` is read, and every link is relative to it), a
local catalogue directory holding an `index.html` (a workspace's
`builds/<name>`), or a bare name, which is `SIMESH_CATALOGUES` + name (by
default `https://reticulous.net/builds/<name>/`). A catalogue's name is the
last component of its location, less a `catalogue-` prefix, so a GitHub
release `…/releases/download/catalogue-stable/` is `stable`; a directory
called `local` or `imported` is the catalogue `builds-local` or
`builds-imported`. With no sources named, a refresh reads `stable` and `dev`
from the web and then every catalogue directory in `builds/` beside SIMesh
(`BUILDS_DIR`), so a workspace's own `make-builds` output joins the
catalogue of its name. The front refreshes them all when it starts, and the
`builds/` directories again whenever the Devices tab lists.

Fetching is aiohttp in the caller's loop, and unzipping runs in a worker
thread, so nothing here blocks an event loop. Run as a script it is the CLI
behind `simesh devices`.
"""

import argparse
import asyncio
import datetime
import html.parser
import json
import os
import platform
import re
import shutil
import stat
import sys
import urllib.parse
import zipfile

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEVICES_DIR = os.path.join(ROOT, "devices")
BUILDS_DIR = os.path.join(os.path.dirname(ROOT), "builds")
LOCAL = "local"
IMPORTED = "imported"
DEFAULT_BASE = os.environ.get("SIMESH_CATALOGUES", "https://reticulous.net/builds/")
WEB_SOURCES = ("stable", "dev")
ENTRY_PREFIX = "hw-simesh-"
NODE_YAML = "node.yaml"
ORIGIN_YAML = "origin.yaml"
PART_PREFIX = ".part-"
KEEP = 3                        # packages kept, fetched and listed per entry per catalogue
CHUNK = 1 << 16
FETCH_TIMEOUT_S = 600

# A workspace build.linux: what spangap leaves for a `target: linux` build.
WORKSPACE_ELF = "reticulous.elf"
WORKSPACE_FIXED = "data_merged"
WORKSPACE_KIND = "reticulous"

ARCH_ALIASES = {"arm64": "aarch64", "amd64": "x86_64", "x64": "x86_64"}


class DeviceError(Exception):
    """A device that cannot be used, or a `device:` that names none."""


def machine_arch():
    """This machine's architecture, spelled as `uname -m` spells it on Linux."""
    arch = platform.machine().lower()
    return ARCH_ALIASES.get(arch, arch)


def split_image_name(name):
    """A catalogue image filename as (slug, entry, stamp), or None.

    Images are `<slug>_<entry>_<stamp>.zip`. The slug never holds an
    underscore and the stamp is all digits, so the entry, which may hold
    underscores, is what lies between the first and the last one.
    """
    if not name.endswith(".zip"):
        return None
    head, _, stamp = name[:-4].rpartition("_")
    if not head or not stamp.isdigit():
        return None
    slug, _, entry = head.partition("_")
    if not slug or not entry:
        return None
    return slug, entry, stamp


def slug_of(project):
    """A project's name as a catalogue filename's slug: lower case, every
    run of other than a–z and 0–9 one `-`, no `-` at either end."""
    slug = re.sub(r"[^a-z0-9]+", "-", str(project).lower()).strip("-")
    return slug or "builds"


def entry_arch(entry):
    """The architecture a `hw-simesh-<arch>` entry is for, or None for any
    other entry."""
    if entry.startswith(ENTRY_PREFIX) and len(entry) > len(ENTRY_PREFIX):
        return entry[len(ENTRY_PREFIX):]
    return None


def when_of(stamp):
    """A build stamp as `YYYY-MM-DD hh:mm`, or the stamp as it is."""
    if len(stamp) >= 12 and stamp.isdigit():
        return "%s-%s-%s %s:%s" % (stamp[:4], stamp[4:6], stamp[6:8], stamp[8:10], stamp[10:12])
    return stamp


def display_name(node, catalogue=None, slug=None):
    """What the page calls a device: its own `name`, else its project,
    catalogue and build time."""
    if node.get("name"):
        return str(node["name"])
    project = node.get("project") or slug or node.get("kind") or "device"
    return " ".join(str(p) for p in (project, catalogue or node.get("catalogue"),
                                     when_of(str(node.get("stamp", "")))) if p)


# ---- node.yaml ---------------------------------------------------------------

def _inside(path, key, value):
    inner = os.path.normpath(str(value))
    if os.path.isabs(inner) or inner == ".." or inner.startswith("../"):
        raise DeviceError("%s: `%s` leaves the package" % (path, key))
    return inner


def _load_mapping(path):
    try:
        with open(path, encoding="utf-8") as f:
            doc = yaml.safe_load(f)
    except OSError as err:
        raise DeviceError("%s: %s" % (path, err.strerror)) from err
    except yaml.YAMLError as err:
        raise DeviceError("%s: %s" % (path, err)) from err
    if not isinstance(doc, dict):
        raise DeviceError("%s: not a mapping" % path)
    return doc


def _check_maps(path, doc):
    for key in ("tools", "env"):
        value = doc.get(key)
        if value is None:
            continue
        if not isinstance(value, dict):
            raise DeviceError("%s: `%s` is a mapping" % (path, key))
        doc[key] = {str(k): str(v) for k, v in value.items()}


def read_node_yaml(directory):
    """`node.yaml` of an expanded package, checked: a dict with every required
    key, its stamp a string, and its `elf`, `fixed` and `tools` inside the
    package."""
    path = os.path.join(directory, NODE_YAML)
    doc = _load_mapping(path)
    for key in ("kind", "arch", "stamp", "elf"):
        if doc.get(key) in (None, ""):
            raise DeviceError("%s: no `%s`" % (path, key))
    doc["stamp"] = str(doc["stamp"])
    if not doc["stamp"].isdigit():
        raise DeviceError("%s: stamp %r is not all digits" % (path, doc["stamp"]))
    _check_maps(path, doc)
    for key in ("elf", "fixed"):
        if doc.get(key) not in (None, ""):
            doc[key] = _inside(path, key, doc[key])
    for tool, value in (doc.get("tools") or {}).items():
        doc["tools"][tool] = _inside(path, "tools.%s" % tool, value)
        if not os.path.isfile(os.path.join(directory, doc["tools"][tool])):
            raise DeviceError("%s: tool %s is not in the package" % (path, value))
    if not os.path.isfile(os.path.join(directory, doc["elf"])):
        raise DeviceError("%s: elf %s is not in the package" % (path, doc["elf"]))
    if doc.get("fixed") and not os.path.isdir(os.path.join(directory, doc["fixed"])):
        raise DeviceError("%s: fixed %s is not in the package" % (path, doc["fixed"]))
    return doc


def read_origin(directory):
    try:
        with open(os.path.join(directory, ORIGIN_YAML), encoding="utf-8") as f:
            doc = yaml.safe_load(f)
    except (OSError, yaml.YAMLError):
        return {}
    return doc if isinstance(doc, dict) else {}


def _env(base, env):
    """An `env` mapping, a value starting `./` or `../` taken as a path from `base`."""
    return {k: (os.path.normpath(os.path.join(base, v)) if v.startswith(("./", "../")) else v)
            for k, v in (env or {}).items()}


def package_result(directory, catalogue=None):
    """What `resolve` hands back for an expanded package directory."""
    node = read_node_yaml(directory)
    origin = read_origin(directory)
    catalogue = catalogue or origin.get("catalogue") or node.get("catalogue")
    parts = split_image_name(os.path.basename(directory) + ".zip")
    return {
        "ref": "%s/%s" % (catalogue, os.path.basename(directory)) if catalogue
               else os.path.basename(directory),
        "elf": os.path.join(directory, node["elf"]),
        "fixed": os.path.join(directory, node["fixed"]) if node.get("fixed") else None,
        "tools": {k: os.path.join(directory, v) for k, v in (node.get("tools") or {}).items()},
        "env": _env(directory, node.get("env")),
        "kind_type": str(node["kind"]),
        "stamp": node["stamp"],
        "arch": str(node["arch"]),
        "name": display_name(node, catalogue, parts[0] if parts else None),
        "stands_for": node.get("stands_for"),
        "source": origin.get("url") or directory,
        "catalogue": catalogue,
        "entry": node.get("entry"),
        "dir": directory,
        "node": node,
    }


# ---- local devices -------------------------------------------------------------

def local_dir(devices_dir=None):
    return os.path.join(devices_dir or DEVICES_DIR, LOCAL)


def local_names(devices_dir=None):
    base = local_dir(devices_dir)
    if not os.path.isdir(base):
        return []
    return sorted(e[:-5] for e in os.listdir(base) if e.endswith(".yaml") and not e.startswith("."))


def local_result(name, devices_dir=None, arch=None):
    """A local device: `devices/local/<name>.yaml`, its paths from the file."""
    path = os.path.join(local_dir(devices_dir), name + ".yaml")
    doc = _load_mapping(path)
    for key in ("kind", "elf"):
        if doc.get(key) in (None, ""):
            raise DeviceError("%s: no `%s`" % (path, key))
    _check_maps(path, doc)
    base = os.path.dirname(path)

    def where(value):
        return os.path.normpath(os.path.join(base, os.path.expanduser(str(value))))

    elf = where(doc["elf"])
    if not os.path.isfile(elf):
        raise DeviceError("device %s: no executable at %s (build it first)" % (name, elf))
    fixed = where(doc["fixed"]) if doc.get("fixed") else None
    stamp = datetime.datetime.fromtimestamp(os.stat(elf).st_mtime, datetime.timezone.utc)
    return {
        "ref": name,
        "elf": elf,
        "fixed": fixed if fixed and os.path.isdir(fixed) else None,
        "tools": {k: where(v) for k, v in (doc.get("tools") or {}).items()},
        "env": _env(base, doc.get("env")),
        "kind_type": str(doc["kind"]),
        "stamp": stamp.strftime("%Y%m%d%H%M%S"),
        "arch": str(doc.get("arch") or arch or machine_arch()),
        "name": str(doc.get("name") or name),
        "stands_for": doc.get("stands_for"),
        "source": path,
        "catalogue": LOCAL,
        "entry": None,
        "dir": base,
        "node": doc,
    }


# ---- what is installed -------------------------------------------------------

def installed(devices_dir=None, arch=None):
    """Every expanded package, as (catalogue, name, slug, entry, stamp, dir),
    newest stamp first; only packages whose entry is for `arch` when given."""
    devices_dir = devices_dir or DEVICES_DIR
    found = []
    try:
        catalogues = sorted(os.listdir(devices_dir))
    except FileNotFoundError:
        return found
    for catalogue in catalogues:
        cdir = os.path.join(devices_dir, catalogue)
        if catalogue.startswith(".") or catalogue == LOCAL or not os.path.isdir(cdir):
            continue
        for name in os.listdir(cdir):
            parts = split_image_name(name + ".zip")
            pdir = os.path.join(cdir, name)
            if not parts or not os.path.isdir(pdir):
                continue
            slug, entry, stamp = parts
            if arch is not None and entry_arch(entry) != arch:
                continue
            found.append((catalogue, name, slug, entry, stamp, pdir))
    found.sort(key=lambda p: (p[4], p[0], p[1]), reverse=True)
    return found


def listing(devices_dir=None, arch=None):
    """Every device, for the Devices tab: the newest `KEEP` packages of each
    catalogue for this machine, newest first, then local devices. Each row
    is {ref, name, stands_for, kind, arch, stamp, catalogue, runs_here,
    newest_of, error?}; `newest_of` names the catalogue this package is what
    that catalogue's name resolves to."""
    arch = arch or machine_arch()
    rows, newest, shown = [], {}, {}
    for catalogue, name, slug, entry, stamp, pdir in installed(devices_dir, arch):
        shown[catalogue] = shown.get(catalogue, 0) + 1
        if shown[catalogue] > KEEP:
            continue
        row = {"ref": "%s/%s" % (catalogue, name), "catalogue": catalogue, "stamp": stamp,
               "arch": entry_arch(entry), "runs_here": True, "newest_of": None, "local": False}
        try:
            got = package_result(pdir, catalogue)
            row.update(name=got["name"], stands_for=got["stands_for"], kind=got["kind_type"],
                       source=got["source"])
        except DeviceError as err:
            row.update(name=name, error=str(err))
        if "error" not in row and catalogue not in newest:
            newest[catalogue] = row
            row["newest_of"] = catalogue
        rows.append(row)
    for name in local_names(devices_dir):
        row = {"ref": name, "catalogue": LOCAL, "local": True, "newest_of": None, "runs_here": True}
        try:
            got = local_result(name, devices_dir, arch)
            row.update(name=got["name"], stands_for=got["stands_for"], kind=got["kind_type"],
                       stamp=got["stamp"], arch=got["arch"], source=got["source"])
        except DeviceError as err:
            row.update(name=name, error=str(err))
        rows.append(row)
    return rows


# ---- resolve -----------------------------------------------------------------

def _looks_like_path(ref):
    return (os.sep in ref or ref.startswith(".") or ref.startswith("~")
            or ref.endswith(".zip"))


def resolve(ref, base_dir=None, devices_dir=None, arch=None):
    """A node's `device:` value as the paths a kind needs.

    Returns {ref, elf, fixed, tools, env, kind_type, stamp, arch, name,
    stands_for, source, catalogue, entry, dir, node}: `fixed` may be None,
    `source` is where the build came from (a URL, a catalogue directory, or
    the path given), `node` is the package's `node.yaml` (None for a
    workspace build). A relative path is taken from `base_dir`. Raises
    DeviceError naming what is missing, with the command that would supply it.
    """
    arch = arch or machine_arch()
    devices_dir = devices_dir or DEVICES_DIR
    if not isinstance(ref, (str, int)) or str(ref).strip() == "":
        raise DeviceError("device: empty")
    ref = str(ref).strip()

    catalogue, _, package = ref.partition("/")
    if package and "/" not in package and split_image_name(package + ".zip"):
        hits = [p for p in installed(devices_dir) if p[0] == catalogue and p[1] == package]
        if hits:
            return _package_here(hits[0], ref, arch)

    if _looks_like_path(ref):
        path = os.path.expanduser(ref)
        if not os.path.isabs(path):
            path = os.path.join(base_dir or os.getcwd(), path)
        return _resolve_path(os.path.normpath(path), arch)

    if ref in local_names(devices_dir):
        return local_result(ref, devices_dir, arch)

    if split_image_name(ref + ".zip"):
        hits = [p for p in installed(devices_dir) if p[1] == ref]
        if not hits:
            raise DeviceError("device %s: no such package in %s (simesh devices list shows "
                              "what there is)" % (ref, devices_dir))
        if len(hits) > 1:
            raise DeviceError("device %s: that package is in several catalogues (%s); name "
                              "it as <catalogue>/%s" % (ref, ", ".join(p[0] for p in hits), ref))
        return _package_here(hits[0], ref, arch)

    if ref.isdigit():
        hits = [p for p in installed(devices_dir, arch) if p[4] == ref]
        if not hits:
            raise DeviceError("device %s: no %s package with that stamp in %s "
                              "(simesh devices list shows what there is)"
                              % (ref, arch, devices_dir))
        if len({p[0] for p in hits}) > 1:
            raise DeviceError("device %s: that stamp is in several catalogues (%s); "
                              "name the package instead"
                              % (ref, ", ".join(sorted({p[0] for p in hits}))))
        return package_result(hits[0][5], hits[0][0])

    hits = [p for p in installed(devices_dir, arch) if p[0] == ref]
    if not hits:
        raise DeviceError("device %s: no %s package from `%s` in %s; "
                          "simesh devices refresh %s fetches one"
                          % (ref, arch, ref, devices_dir, ref))
    return package_result(hits[0][5], ref)


def _package_here(hit, ref, arch):
    """An installed package named outright, refused unless it runs here."""
    got = package_result(hit[5], hit[0])
    if got["arch"] != arch:
        raise DeviceError("device %s: built for %s, this machine is %s"
                          % (ref, got["arch"], arch))
    return got


def _resolve_path(path, arch):
    if os.path.isfile(path):
        raise DeviceError("device %s: a file; name a package directory or a "
                          "build.linux directory (Import on the Devices tab takes a zip)" % path)
    if not os.path.isdir(path):
        raise DeviceError("device %s: no such directory" % path)
    if os.path.isfile(os.path.join(path, NODE_YAML)):
        got = package_result(path)
        if got["arch"] != arch:
            raise DeviceError("device %s: built for %s, this machine is %s"
                              % (path, got["arch"], arch))
        return got
    elf = os.path.join(path, WORKSPACE_ELF)
    if os.path.isfile(elf):
        fixed = os.path.join(path, WORKSPACE_FIXED)
        when = datetime.datetime.fromtimestamp(os.stat(elf).st_mtime, datetime.timezone.utc)
        return {
            "ref": path,
            "elf": elf,
            "fixed": fixed if os.path.isdir(fixed) else None,
            "tools": {},
            "env": {},
            "kind_type": WORKSPACE_KIND,
            "stamp": when.strftime("%Y%m%d%H%M%S"),
            "arch": arch,
            "name": "workspace %s" % path,
            "stands_for": None,
            "source": path,
            "catalogue": None,
            "entry": None,
            "dir": path,
            "node": None,
        }
    raise DeviceError("device %s: neither a device package (no %s) nor a workspace "
                      "build (no %s)" % (path, NODE_YAML, WORKSPACE_ELF))


# ---- catalogues --------------------------------------------------------------

class _Links(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []

    def handle_starttag(self, tag, attrs):
        if tag == "a":
            got = {k: (v or "") for k, v in attrs}
            if got.get("href"):
                self.links.append(got)


def parse_listing(text):
    """A catalogue listing's links, as attribute dicts (`href` plus whatever
    `data-*` facts the row carries)."""
    parser = _Links()
    parser.feed(text)
    parser.close()
    return parser.links


def newest_packages(links, keep=KEEP):
    """The newest `keep` device packages per `hw-simesh-*` entry of a
    listing, as {entry: [{href, name, slug, stamp, arch, attrs}, …]}, newest
    first. Other images are left out."""
    every = {}
    for attrs in links:
        href = attrs["href"]
        name = urllib.parse.unquote(href.rstrip("/").rsplit("/", 1)[-1])
        parts = split_image_name(name)
        if not parts:
            continue
        slug, entry, stamp = parts
        arch = entry_arch(entry)
        if arch is None:
            continue
        mine = every.setdefault(entry, {})
        mine.setdefault(stamp, {"href": href, "name": name, "slug": slug,
                                "stamp": stamp, "arch": arch, "attrs": attrs})
    return {entry: [pkgs[s] for s in sorted(pkgs, reverse=True)[:keep]]
            for entry, pkgs in every.items()}


def builds_sources(builds_dir=None):
    """The catalogue directories in `builds/` beside SIMesh: each one holding
    an `index.html`."""
    base = builds_dir or BUILDS_DIR
    try:
        names = sorted(os.listdir(base))
    except OSError:
        return []
    return [os.path.join(base, n) for n in names
            if not n.startswith(".") and os.path.isfile(os.path.join(base, n, "index.html"))]


def default_sources():
    return list(WEB_SOURCES) + builds_sources()


class Source:
    """Where one catalogue is: `location` is a URL ending in `/`, or a local
    directory; `name` is the catalogue's name, the directory under devices/."""

    def __init__(self, spec, base=DEFAULT_BASE):
        self.spec = spec
        if "://" in spec:
            self.local = False
            self.location = spec if spec.endswith("/") else spec + "/"
        elif os.path.isdir(spec) and os.path.isfile(os.path.join(spec, "index.html")):
            self.local = True
            self.location = os.path.abspath(spec)
        elif os.sep not in spec and not spec.startswith("."):
            self.local = False
            self.location = urllib.parse.urljoin(base if base.endswith("/") else base + "/",
                                                 spec + "/")
        else:
            raise DeviceError("%s: not a URL, a catalogue name, or a directory "
                              "holding an index.html" % spec)
        last = self.location.rstrip("/").rsplit("/", 1)[-1]
        if not self.local:
            last = urllib.parse.unquote(last)
        if last.startswith("catalogue-"):
            last = last[len("catalogue-"):]
        if not last or last.startswith(".") or os.sep in last:
            raise DeviceError("%s: cannot tell the catalogue's name" % spec)
        if last in (LOCAL, IMPORTED):
            if not self.local:
                raise DeviceError("%s: a catalogue cannot be called %s" % (spec, last))
            last = "builds-" + last
        self.name = last

    def where(self, href):
        if self.local:
            if "://" in href or os.path.isabs(href):
                raise DeviceError("%s: link %s leaves the directory" % (self.spec, href))
            return os.path.join(self.location, urllib.parse.unquote(href))
        return urllib.parse.urljoin(self.location, href)

    async def listing(self, session):
        if self.local:
            path = os.path.join(self.location, "index.html")
            return await asyncio.to_thread(_read_text, path)
        async with session.get(self.location + "index.html") as resp:
            if resp.status != 200:
                raise DeviceError("%sindex.html: HTTP %d" % (self.location, resp.status))
            return await resp.text()


def _read_text(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


# ---- expanding ---------------------------------------------------------------

def expand(zip_path, dest, origin, arch):
    """Unzip a package to `dest`, whole or not at all.

    The members go under a `.part-` sibling first; `node.yaml` is read and its
    architecture and stamp checked against the filename before the rename, so
    a package directory that exists is one that was checked. Members that
    would land outside the package are refused. Permission bits the zip
    carries are kept, and the ELF and the tools are made executable either way.
    """
    parent = os.path.dirname(dest)
    os.makedirs(parent, exist_ok=True)
    part = os.path.join(parent, PART_PREFIX + os.path.basename(dest))
    shutil.rmtree(part, ignore_errors=True)
    try:
        with zipfile.ZipFile(zip_path) as zf:
            for info in zf.infolist():
                inner = os.path.normpath(info.filename)
                if os.path.isabs(info.filename) or inner == ".." or inner.startswith("../"):
                    raise DeviceError("%s: member %s leaves the package"
                                      % (zip_path, info.filename))
                target = os.path.join(part, inner)
                if info.is_dir():
                    os.makedirs(target, exist_ok=True)
                    continue
                os.makedirs(os.path.dirname(target), exist_ok=True)
                with zf.open(info) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst, CHUNK)
                mode = (info.external_attr >> 16) & 0o777
                if mode:
                    os.chmod(target, mode)
        node = read_node_yaml(part)
        parts = split_image_name(os.path.basename(dest) + ".zip")
        if str(node["arch"]) != arch:
            raise DeviceError("%s: built for %s, this machine is %s"
                              % (os.path.basename(zip_path), node["arch"], arch))
        if parts and node["stamp"] != parts[2]:
            raise DeviceError("%s: node.yaml says stamp %s, the filename %s"
                              % (os.path.basename(zip_path), node["stamp"], parts[2]))
        if parts and entry_arch(parts[1]) != str(node["arch"]):
            raise DeviceError("%s: node.yaml says arch %s, the filename %s"
                              % (os.path.basename(zip_path), node["arch"], entry_arch(parts[1])))
        for inner in [node["elf"]] + list((node.get("tools") or {}).values()):
            path = os.path.join(part, inner)
            os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        with open(os.path.join(part, ORIGIN_YAML), "w", encoding="utf-8") as f:
            yaml.safe_dump(origin, f, sort_keys=False)
        if not os.path.isfile(os.path.join(dest, NODE_YAML)):
            shutil.rmtree(dest, ignore_errors=True)
            os.rename(part, dest)
    except zipfile.BadZipFile as err:
        raise DeviceError("%s: %s" % (os.path.basename(zip_path), err)) from err
    finally:
        shutil.rmtree(part, ignore_errors=True)
    return dest


def _entry_packages(cdir, entry):
    """(stamp, name) of one entry's packages in one catalogue's directory,
    newest first."""
    mine = []
    try:
        names = os.listdir(cdir)
    except FileNotFoundError:
        return mine
    for name in names:
        parts = split_image_name(name + ".zip")
        if parts and parts[1] == entry and os.path.isdir(os.path.join(cdir, name)):
            mine.append((parts[2], name))
    mine.sort(reverse=True)
    return mine


def newer_installed(cdir, entry, stamp):
    return [n for s, n in _entry_packages(cdir, entry) if s > stamp]


def prune(cdir, entry, keep=KEEP):
    """Remove all but the newest `keep` packages of one entry in one
    catalogue's directory. Returns the removed directory names."""
    mine = _entry_packages(cdir, entry)
    gone = []
    for _, name in mine[keep:]:
        shutil.rmtree(os.path.join(cdir, name), ignore_errors=True)
        gone.append(name)
    return gone


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def import_zip(zip_path, filename, devices_dir=None, arch=None):
    """A device zip someone handed over, whatever it is called, into the
    `imported` catalogue. Returns the package's `resolve` result.

    It is named from its `node.yaml` as a catalogue would name it,
    `<slug>_hw-simesh-<arch>_<stamp>`, the slug from its `project` or else its
    `kind`; it must be for this machine's architecture, and a package already
    there under that name is kept, not replaced."""
    arch = arch or machine_arch()
    shown = os.path.basename(filename or "") or "the upload"
    try:
        with zipfile.ZipFile(zip_path) as zf:
            node = yaml.safe_load(zf.read(NODE_YAML))
    except KeyError as err:
        raise DeviceError("%s holds no %s at its top" % (shown, NODE_YAML)) from err
    except zipfile.BadZipFile as err:
        raise DeviceError("%s: %s" % (shown, err)) from err
    except yaml.YAMLError as err:
        raise DeviceError("%s: %s: %s" % (shown, NODE_YAML, err)) from err
    if not isinstance(node, dict) or any(node.get(k) in (None, "") for k in ("kind", "arch", "stamp")):
        raise DeviceError("%s: %s needs kind, arch and stamp" % (shown, NODE_YAML))
    if str(node["arch"]) != arch:
        raise DeviceError("%s is built for %s, and this machine is %s" % (shown, node["arch"], arch))
    name = "%s_%s%s_%s" % (slug_of(node.get("project") or node["kind"]), ENTRY_PREFIX, arch,
                           node["stamp"])
    dest = os.path.join(devices_dir or DEVICES_DIR, IMPORTED, name)
    if os.path.isfile(os.path.join(dest, NODE_YAML)):
        raise DeviceError("%s has been imported already, as %s" % (shown, name))
    expand(zip_path, dest, {"catalogue": IMPORTED, "url": shown, "fetched": now_utc()}, arch)
    return package_result(dest, IMPORTED)


# ---- refresh -----------------------------------------------------------------

async def _download(session, url, path):
    size = 0
    async with session.get(url) as resp:
        if resp.status != 200:
            raise DeviceError("%s: HTTP %d" % (url, resp.status))
        with open(path, "wb") as f:
            async for chunk in resp.content.iter_chunked(CHUNK):
                f.write(chunk)
                size += len(chunk)
    return size


async def refresh_one(source, session, devices_dir=None, arch=None, say=print):
    """Bring one catalogue's packages for this machine up to date. Returns one
    result per `hw-simesh-*` entry and package, newest first: {catalogue,
    entry, stamp, state, dir, reason}, state being `fetched`, `current`,
    `skipped` or `failed`; an entry for another machine is one `skipped`."""
    arch = arch or machine_arch()
    text = await source.listing(session)
    newest = newest_packages(parse_listing(text))
    results = []
    if not newest:
        say("%s: no device packages in %s" % (source.name, source.location))
        return results
    cdir = os.path.join(devices_dir or DEVICES_DIR, source.name)
    for entry in sorted(newest):
        fetched = await _refresh_entry(source, session, cdir, entry, newest[entry], arch,
                                       results, say)
        if fetched:
            gone = await asyncio.to_thread(prune, cdir, entry)
            if gone:
                say("%s: removed %s" % (source.name, ", ".join(gone)))
    return results


async def _refresh_entry(source, session, cdir, entry, pkgs, arch, results, say):
    """One entry's newest packages, fetched where missing; whether any was."""
    if pkgs[0]["arch"] != arch:
        results.append({"catalogue": source.name, "entry": entry, "stamp": pkgs[0]["stamp"],
                        "state": "skipped", "dir": None,
                        "reason": "for %s, this machine is %s" % (pkgs[0]["arch"], arch)})
        say("%s: skipping %s: it is for %s, this machine is %s"
            % (source.name, entry, pkgs[0]["arch"], arch))
        return False
    any_fetched = False
    for pkg in pkgs:
        result = {"catalogue": source.name, "entry": entry, "stamp": pkg["stamp"],
                  "state": None, "dir": None, "reason": None}
        results.append(result)
        dest = os.path.join(cdir, pkg["name"][:-4])
        result["dir"] = dest
        if os.path.isfile(os.path.join(dest, NODE_YAML)):
            result["state"] = "current"
            continue
        # Another source of this catalogue may have newer ones: a package
        # pruning would remove at once is not fetched at all.
        if len(newer_installed(cdir, entry, pkg["stamp"])) >= KEEP:
            result.update(state="skipped", dir=None, reason="older than the %d kept" % KEEP)
            continue
        where = source.where(pkg["href"])
        origin = {"catalogue": source.name, "url": where, "fetched": now_utc()}
        tmp_zip = None
        try:
            if source.local:
                zip_path = where
            else:
                os.makedirs(cdir, exist_ok=True)
                tmp_zip = os.path.join(cdir, PART_PREFIX + pkg["name"])
                await _download(session, where, tmp_zip)
                zip_path = tmp_zip
            await asyncio.to_thread(expand, zip_path, dest, origin, arch)
        except (DeviceError, OSError) as err:
            result.update(state="failed", dir=None, reason=str(err))
            say("%s: %s %s failed: %s" % (source.name, entry, pkg["stamp"], err))
            continue
        finally:
            if tmp_zip:
                try:
                    os.unlink(tmp_zip)
                except FileNotFoundError:
                    pass
        result["state"] = "fetched"
        any_fetched = True
        say("%s: %s %s fetched" % (source.name, entry, pkg["stamp"]))
    if not any_fetched:
        say("%s: %s is current" % (source.name, entry))
    return any_fetched


async def refresh(sources=None, devices_dir=None, arch=None, say=print,
                  base=DEFAULT_BASE):
    """Fetch the newest `KEEP` packages of every `hw-simesh-*` entry of each
    source (catalogue names, URLs or directories; by default
    `default_sources()`). A source that cannot be read is reported and the
    others still run. Returns every result, a source that failed as one
    result with entry None."""
    import aiohttp

    arch = arch or machine_arch()
    results = []
    timeout = aiohttp.ClientTimeout(total=FETCH_TIMEOUT_S)
    async with aiohttp.ClientSession(timeout=timeout) as session:
        for spec in (sources or default_sources()):
            try:
                source = Source(spec, base)
                results.extend(await refresh_one(source, session, devices_dir, arch, say))
            except (DeviceError, OSError, aiohttp.ClientError, asyncio.TimeoutError) as err:
                reason = str(err) or type(err).__name__
                say("%s: %s" % (spec, reason))
                results.append({"catalogue": spec, "entry": None, "stamp": None,
                                "state": "failed", "dir": None, "reason": reason})
    return results


# ---- the CLI -----------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(prog="simesh devices",
                                 description="the station builds a node can run")
    sub = ap.add_subparsers(dest="verb", required=True)
    p = sub.add_parser("refresh", help="fetch the newest package of each catalogue "
                       "(default: %s, then every catalogue in %s)"
                       % (" ".join(WEB_SOURCES), BUILDS_DIR))
    p.add_argument("sources", nargs="*", metavar="SOURCE",
                   help="a catalogue name, a catalogue URL, or a local catalogue directory")
    sub.add_parser("list", help="the devices there are")
    p = sub.add_parser("import", help="a device zip into the `imported` catalogue")
    p.add_argument("zip")
    p = sub.add_parser("resolve", help="what a node's device: value runs, as JSON")
    p.add_argument("device")
    p.add_argument("--base-dir", default=None,
                   help="the directory relative paths are taken from")
    args = ap.parse_args(argv)

    if args.verb == "refresh":
        results = asyncio.run(refresh(args.sources))
        return 1 if any(r["state"] == "failed" for r in results) else 0
    if args.verb == "list":
        for row in listing():
            mark = "" if row.get("runs_here") else "   (not this machine's arch)"
            mark += "   (newest %s)" % row["newest_of"] if row.get("newest_of") else ""
            mark += "   ! %s" % row["error"] if row.get("error") else ""
            print("%-10s %-50s %s%s" % (row["catalogue"], row["ref"], row.get("name", ""), mark))
        return 0
    try:
        if args.verb == "import":
            got = import_zip(args.zip, os.path.basename(args.zip))
            print("imported %s as %s" % (got["name"], got["ref"]))
            return 0
        got = resolve(args.device, args.base_dir)
    except DeviceError as err:
        print("simesh devices: %s" % err, file=sys.stderr)
        return 1
    print(json.dumps(got, indent=2, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
