"""Antennas: what a node radiates with, as gain by direction.

    testbed/antennas/catalogue.yaml     every antenna there is, by type
    testbed/antennas/<type>.svg         its picture, for the Antennas tab

A node carries one antenna, `antenna: { type, azimuth_deg?, elevation_deg? }`
in its nodeset; a directional one is aimed by its azimuth (degrees clockwise
from grid north) and its elevation (degrees above the horizon), an omni is
the same all round and neither applies.

**The pattern** is made from a few figures per type (the catalogue's header
says what each is). The loss below the peak in a direction is the vertical
term, `12·((el − tilt) / vbw)²`, plus for a directional antenna the
horizontal one, `12·(az / hbw)²`, together never more than `floor_db`: each
term is 3 dB at half its beamwidth, as a half-power beamwidth says, and the
floor stands for the side lobes, the back and what the case fills in. `el`
and `az` are measured from where the antenna points. The same formula is on
the page (ui/src/lib/antennas.ts), which draws it and uses it for coverage.

**Between two nodes** the direction is the line between their antennas, in
three dimensions: the azimuth from one's position to the other's, and the
elevation of the line from one's antenna (the ground under it plus its
height) to the other's, with the earth's curvature at the effective radius
(k = 4/3) taking its drop off the far end. So a pair's gain is the sum of
each end's gain toward the other, and a low node under a high collinear's
narrow beam hears less of it than one on the horizon.
"""

import math
import os

import yaml

import store

HERE = os.path.dirname(os.path.abspath(__file__))
ANTENNAS_DIR = os.path.join(HERE, "antennas")
CATALOGUE = os.path.join(ANTENNAS_DIR, "catalogue.yaml")
DEFAULT_TYPE = "whip_sma_quarter_wave"
KINDS = ("omni", "directional")
EARTH_RADIUS_M = 6371008.8
K_FACTOR = 4.0 / 3.0

_catalogue = None


def catalogue():
    """Every antenna, {type: {label, description, kind, peak_dbi, vbw_deg,
    tilt_deg, hbw_deg?, floor_db}}, in the file's order."""
    global _catalogue
    if _catalogue is None:
        with open(CATALOGUE, encoding="utf-8") as handle:
            doc = yaml.safe_load(handle) or {}
        out = {}
        for name, spec in doc.items():
            if spec.get("kind") not in KINDS:
                raise store.StoreError("antenna %s: kind is one of %s" % (name, ", ".join(KINDS)))
            out[str(name)] = {"label": str(spec.get("label") or name),
                              "description": str(spec.get("description") or ""),
                              "kind": spec["kind"],
                              "peak_dbi": float(spec["peak_dbi"]),
                              "vbw_deg": float(spec["vbw_deg"]),
                              "tilt_deg": float(spec.get("tilt_deg") or 0.0),
                              "hbw_deg": float(spec["hbw_deg"]) if spec.get("hbw_deg") else None,
                              "floor_db": float(spec["floor_db"])}
        _catalogue = out
    return _catalogue


def listing():
    """The catalogue for the page: each antenna with its type and picture."""
    out = []
    for name, spec in catalogue().items():
        picture = os.path.join(ANTENNAS_DIR, name + ".svg")
        svg = None
        if os.path.isfile(picture):
            with open(picture, encoding="utf-8") as handle:
                svg = handle.read()
        out.append({"type": name, **spec, "svg": svg})
    return out


def spec_of(antenna):
    """The catalogue entry of a node's antenna, the default one for a type
    the catalogue does not have."""
    every = catalogue()
    return every.get((antenna or {}).get("type")) or every[DEFAULT_TYPE]


def check(antenna, where):
    """A node's antenna mapping checked and filled out: its type one of the
    catalogue's, the aim of a directional one numbers (0 by default)."""
    if antenna in (None, {}):
        antenna = {"type": DEFAULT_TYPE}
    if not isinstance(antenna, dict):
        raise store.StoreError("%s: antenna is { type, azimuth_deg?, elevation_deg? }" % where)
    kind = str(antenna.get("type") or DEFAULT_TYPE)
    every = catalogue()
    if kind not in every:
        raise store.StoreError("%s: no antenna of type %r (there are %s)"
                               % (where, kind, ", ".join(every)))
    for key in antenna:
        if key not in ("type", "azimuth_deg", "elevation_deg"):
            raise store.StoreError("%s: antenna has no %r (it takes type, azimuth_deg, "
                                   "elevation_deg)" % (where, key))
    out = {"type": kind}
    if every[kind]["kind"] == "directional":
        try:
            out["azimuth_deg"] = float(antenna.get("azimuth_deg") or 0.0) % 360.0
            out["elevation_deg"] = max(-90.0, min(90.0, float(antenna.get("elevation_deg") or 0.0)))
        except (TypeError, ValueError) as err:
            raise store.StoreError("%s: an antenna's aim is in degrees" % where) from err
    return out


def wrap180(deg):
    return (deg + 180.0) % 360.0 - 180.0


def gain(antenna, az_deg, el_deg):
    """The antenna's gain in dBi toward azimuth `az_deg` (clockwise from
    grid north) and elevation `el_deg` (above the horizon)."""
    spec = spec_of(antenna)
    tilt = spec["tilt_deg"]
    off_az = 0.0
    if spec["kind"] == "directional":
        tilt += float((antenna or {}).get("elevation_deg") or 0.0)
        off_az = wrap180(az_deg - float((antenna or {}).get("azimuth_deg") or 0.0))
    down = 12.0 * ((el_deg - tilt) / spec["vbw_deg"]) ** 2
    if spec["kind"] == "directional" and spec["hbw_deg"]:
        down += 12.0 * (off_az / spec["hbw_deg"]) ** 2
    return spec["peak_dbi"] - min(down, spec["floor_db"])


def direction(a_xy, a_top, b_xy, b_top):
    """(azimuth, elevation) in degrees of the line from antenna a to antenna
    b: positions in metres of a projected frame (x east, y north), tops in
    metres above sea level."""
    dx, dy = b_xy[0] - a_xy[0], b_xy[1] - a_xy[1]
    d = math.hypot(dx, dy)
    az = math.degrees(math.atan2(dx, dy)) % 360.0
    drop = d * d / (2.0 * K_FACTOR * EARTH_RADIUS_M)
    el = math.degrees(math.atan2(b_top - a_top - drop, max(d, 1e-6)))
    return az, el


def pair_gain(a, b):
    """The gain in dB a pair's two antennas add to it, each toward the
    other; `a` and `b` are {xy, top, antenna}. The same both ways."""
    az_ab, el_ab = direction(a["xy"], a["top"], b["xy"], b["top"])
    az_ba, el_ba = direction(b["xy"], b["top"], a["xy"], a["top"])
    return gain(a["antenna"], az_ab, el_ab) + gain(b["antenna"], az_ba, el_ba)
