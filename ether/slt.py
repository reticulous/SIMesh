"""The loss table file: every ordered pair's path loss for one nodeset on one geodata.

    "SLT1" | u32 header_len | header (UTF-8 JSON) | u32 n
           | f32 loss_db[n·n]     row-major, [from][to]; the diagonal unused
           | u8  flags[n·n]       FLAG_* below
           | u16 samples[n·n]     measurement count, 0 when modelled

Little-endian throughout. The header names the nodes in matrix order, so a
row is found by node name and never by position in some other list:

    { geodata, geodata_hash, pack_manifest_hash, nodeset_geometry, band, f0_hz,
      model: "P.1812-8" | "log-distance", p_time_pct, p_loc_pct, exponent?,
      radius_m?, nodes: [ {name, id, lat, lon, height_m, height_from} ],
      computed_at, planner_version }

A table is computed at one frequency `f0_hz` in its band; a frame on carrier
f is `loss + 20·log10(f / f0)` from it. Only the free-space term scales that
way, which is why that correction is used within a band and each band has its
own table. "Never heard" is +inf. LOSSTABLE.md at the repository root is the
format's specification.
"""

import json
import math
import struct
from array import array

MAGIC = b"SLT1"

FLAG_NEAR_FIELD = 1 << 0    # free space (plus knife edge) rather than the full model
FLAG_OFF_PACK = 1 << 1      # no terrain under the path: it leaves the pack
FLAG_BEYOND_RADIUS = 1 << 2  # farther apart than the compute radius
FLAG_LOS_CLEAR = 1 << 3     # the first Fresnel zone is clear
FLAG_MEASURED = 1 << 4      # the loss comes from a running network, not the model

NEVER = math.inf

# The bands a table is computed for, by name, and each one's centre and edges
# in Hz. 2.4 GHz has none: the SX1262 is the only chip, and the planner's
# terminal clutter correction stops at 3 GHz.
BANDS = {
    "433": (433_920_000, 410_000_000, 525_000_000),
    "868": (868_000_000, 850_000_000, 880_000_000),
    "915": (915_000_000, 902_000_000, 930_000_000),
}


def band_for(freq_hz):
    """The band a carrier falls in, or None."""
    if freq_hz is None:
        return None
    for name, (_, low, high) in BANDS.items():
        if low <= freq_hz <= high:
            return name
    return None


def f0_of(band):
    return BANDS[band][0]


class Table:
    """One band's matrix, with the nodes it is indexed by."""

    def __init__(self, header, loss=None, flags=None, samples=None):
        self.header = dict(header)
        self.names = [d["name"] for d in self.header.get("nodes") or ()]
        self.index = {name: i for i, name in enumerate(self.names)}
        n = len(self.names)
        self.loss = loss if loss is not None else array("f", [NEVER]) * (n * n)
        self.flags = flags if flags is not None else array("B", [0]) * (n * n)
        self.samples = samples if samples is not None else array("H", [0]) * (n * n)

    @property
    def n(self):
        return len(self.names)

    @property
    def band(self):
        return self.header.get("band")

    @property
    def f0_hz(self):
        return self.header.get("f0_hz")

    def cell(self, a, b):
        """The flat index of the pair (from a, to b), by node name."""
        return self.index[a] * self.n + self.index[b]

    def get(self, a, b):
        """Loss in dB from node a to node b at f0; +inf when never heard."""
        return self.loss[self.cell(a, b)]

    def at(self, a, b, freq_hz):
        """Loss from a to b on this carrier: the within-band free-space correction."""
        loss = self.get(a, b)
        if freq_hz and self.f0_hz and math.isfinite(loss):
            loss += 20.0 * math.log10(freq_hz / self.f0_hz)
        return loss

    def put(self, a, b, loss_db, flags=0, samples=0):
        i = self.cell(a, b)
        self.loss[i] = loss_db
        self.flags[i] = flags
        self.samples[i] = samples

    def flag(self, a, b):
        return self.flags[self.cell(a, b)]

    # ---- the file --------------------------------------------------------

    def dumps(self):
        head = json.dumps(self.header, separators=(",", ":"), sort_keys=True).encode("utf-8")
        loss, flags, samples = array("f", self.loss), array("B", self.flags), array("H", self.samples)
        if struct.pack("=H", 1) != struct.pack("<H", 1):
            loss.byteswap()
            samples.byteswap()
        return b"".join((MAGIC, struct.pack("<I", len(head)), head,
                         struct.pack("<I", self.n),
                         loss.tobytes(), flags.tobytes(), samples.tobytes()))

    def write(self, path):
        with open(path, "wb") as handle:
            handle.write(self.dumps())

    @classmethod
    def loads(cls, data):
        if data[:4] != MAGIC:
            raise ValueError("not a loss table (no SLT1 magic)")
        (head_len,) = struct.unpack_from("<I", data, 4)
        header = json.loads(data[8:8 + head_len].decode("utf-8"))
        at = 8 + head_len
        (n,) = struct.unpack_from("<I", data, at)
        at += 4
        cells = n * n
        loss = array("f")
        loss.frombytes(data[at:at + 4 * cells])
        at += 4 * cells
        flags = array("B")
        flags.frombytes(data[at:at + cells])
        at += cells
        samples = array("H")
        samples.frombytes(data[at:at + 2 * cells])
        if struct.pack("=H", 1) != struct.pack("<H", 1):
            loss.byteswap()
            samples.byteswap()
        if len(loss) != cells or len(flags) != cells or len(samples) != cells:
            raise ValueError("loss table is truncated")
        table = cls(header, loss, flags, samples)
        if table.n != n:
            raise ValueError("loss table header names %d nodes, matrix is %d" % (table.n, n))
        return table

    @classmethod
    def read(cls, path):
        with open(path, "rb") as handle:
            return cls.loads(handle.read())
