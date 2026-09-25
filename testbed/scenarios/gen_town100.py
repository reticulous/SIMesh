#!/usr/bin/env python3
"""Write town100-lora.yaml, town100-supe.yaml and town100-supe0.yaml: 100 Reticulous stations
in one town.

    python3 gen_town100.py [SEED]        (default seed 1)

Run from anywhere; the three files land beside this script and differ only in
the SUPE lines of `setup:`: none in town100-lora, channel plan 1 in
town100-supe, channel plan 0 in town100-supe0 (`set s.lora.0.SUPE.afa 1` or
`0`, then `lora 0 supe enable`). One seed gives one map, byte for byte.

The town
    6 km east-west by 4 km north-south, centred on cityF's origin
    (52.374, 4.8897). Not uniform:
    - a main street, a gently bending line across the town from west to
      east, with 20 stations along it about 300 m apart, each up to 50 m
      off the line and 60 m along it from its even spacing;
    - 5 to 7 neighbourhood clusters (the seed picks how many), centres at
      least 1200 m apart and 500 m inside the town's edge, sharing 75
      stations by random weights, each with its own spread (a normal
      spread drawn between 120 and 420 m, times the scale below), so some
      are dense and some loose;
    - 5 isolated stations near the town's edge, each at least 400 m from
      every other station and within reach of at least 2;
    - no two stations closer than 60 m (a draw that lands closer is drawn
      again).
    Station ids 1 to 100 run west to east; names t001 to t100.

Physics
    exponent 3.5, noise figure 6 dB, capture 6 dB. With the ether's
    PL(d) = FSPL(1 m, f) + 10 n log10(d) and an SF7 threshold of -7.5 dB
    SNR over a -117 dBm floor at 125 kHz, a 14 dBm frame at 869.525 MHz
    reaches 1163 m.

Radio (every station)
    lora up; 869.525 MHz; SF7; 125 kHz; CR 4/5; 14 dBm; community radius
    25, set before `lora up` because the radio hands it to rnsd when it
    starts, and wider than the town so that every announce reaches every
    station. Route persistence on, the directory written every 60 s. No
    netgraph community, no TCP or internet backbone.

Checks
    Neighbours are the pairs the ether delivers at SF7 and 14 dBm. The map
    is written only when the median neighbour count is 10 to 20, every
    station has at least 2 neighbours, and the radio graph is one piece. The
    cluster spreads are scaled (from 1.0, in steps of 5 percent, wider when
    the median is too high and narrower when it is too low) until the map
    passes, and the scale, the neighbour counts and the hop diameter are
    printed. A seed whose passing map is not 5 to 7 hops across is refused
    outright: the spread does not move the diameter much, another seed does.

Transport
    On at 33 stations, the rest `set s.rnsd.transport_enabled 0`. Chosen
    greedily: first the station that covers the most stations not yet
    beside a transport (ties to the one farthest from any transport), until
    every station hears one; then stations that join the transports into
    one connected set; then the station farthest from any transport, until
    there are 33.
"""
import math
import os
import random
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "ether"))
import ether as E  # noqa: E402 - the path is set just above

ORIGIN = (52.374, 4.8897)
WIDTH_M, HEIGHT_M = 6000.0, 4000.0
PHYSICS = {"exponent": 3.5, "noise_figure_db": 6, "capture_db": 6}
FREQ_HZ, BW_HZ, SF, TXP_DBM = 869_525_000, 125_000, 7, 14
N_STATIONS = 100
N_STREET = 20
N_EDGE = 5
N_CLUSTERS = (5, 7)
CLUSTER_SPREAD_M = (120.0, 420.0)
CLUSTER_GAP_M = 1200.0
CLUSTER_INSET_M = 500.0
EDGE_BAND_M = 300.0
EDGE_CLEAR_M = 400.0
MIN_SPACING_M = 60.0
N_TRANSPORT = 33
NEIGHBOURS_MEDIAN = (10, 20)
NEIGHBOURS_MIN = 2
DIAMETER = (5, 7)

SHARED = [
    "hostname {name}",
    "auth passwd admin admin",
    "lxmf create {name}",
    "set s.lora.0.community_radius 25",
    "lora up",
    "lora 0 freq 869.525",
    "lora 0 sf 7",
    "lora 0 bw 125",
    "lora 0 cr 5",
    "lora 0 txp 14",
    "set s.rnsd.dir.persist_routes 1",
    "set s.rnsd.dir.persist_s 60",
]
SUPE = ["set s.lora.0.SUPE.afa 1", "lora 0 supe enable"]
SUPE0 = ["set s.lora.0.SUPE.afa 0", "lora 0 supe enable"]


def reach_m():
    """The distance at which a 14 dBm SF7 frame meets the threshold."""
    ph = E.Physics.from_dict(PHYSICS)
    e = E.Ether.__new__(E.Ether)
    e.physics = ph
    need = e.noise(BW_HZ) + e.sensitivity(SF)
    return 10 ** ((TXP_DBM - need - E.fspl_1m_db(FREQ_HZ)) / (10 * ph.exponent))


def inside(x, y, margin=0.0):
    return abs(x) <= WIDTH_M / 2 - margin and abs(y) <= HEIGHT_M / 2 - margin


def spaced(p, pts, gap=MIN_SPACING_M):
    return all(math.hypot(p[0] - q[0], p[1] - q[1]) >= gap for q in pts)


def draw(seed, scale, reach):
    """One map: a list of (x, y, where), `where` saying which part of the town."""
    rng = random.Random(seed)
    pts = []

    # The main street: west edge to east edge, bending once north and back.
    y0, y1, bend = rng.uniform(-900, -300), rng.uniform(300, 900), rng.uniform(300, 600)
    for i in range(N_STREET):
        f = (i + 0.5) / N_STREET
        while True:
            x = -WIDTH_M / 2 + f * WIDTH_M + rng.uniform(-60, 60)
            y = y0 + (y1 - y0) * f + bend * math.sin(math.pi * f) + rng.uniform(-50, 50)
            if inside(x, y) and spaced((x, y), pts):
                break
        pts.append((x, y, "street"))

    # The neighbourhoods.
    k = rng.randint(*N_CLUSTERS)
    centres = []
    while len(centres) < k:
        c = (rng.uniform(-WIDTH_M / 2 + CLUSTER_INSET_M, WIDTH_M / 2 - CLUSTER_INSET_M),
             rng.uniform(-HEIGHT_M / 2 + CLUSTER_INSET_M, HEIGHT_M / 2 - CLUSTER_INSET_M))
        if spaced(c, centres, CLUSTER_GAP_M):
            centres.append(c)
    spreads = [rng.uniform(*CLUSTER_SPREAD_M) for _ in centres]
    weights = [rng.uniform(0.5, 1.5) for _ in centres]
    n_cl = N_STATIONS - N_STREET - N_EDGE
    sizes = [int(n_cl * w / sum(weights)) for w in weights]
    for i in range(n_cl - sum(sizes)):
        sizes[i % k] += 1
    for c, s, n, idx in zip(centres, spreads, sizes, range(k)):
        for _ in range(n):
            while True:
                x = rng.gauss(c[0], s * scale)
                y = rng.gauss(c[1], s * scale)
                if inside(x, y) and spaced((x, y), pts):
                    break
            pts.append((x, y, "cluster %d" % (idx + 1)))

    # The outliers: near an edge, clear of everyone, heard by two.
    for _ in range(N_EDGE):
        for _attempt in range(20000):
            side = rng.randrange(4)
            u = rng.uniform(-1, 1)
            d = rng.uniform(0, EDGE_BAND_M)
            if side < 2:
                x, y = u * WIDTH_M / 2, (HEIGHT_M / 2 - d) * (1 if side else -1)
            else:
                x, y = (WIDTH_M / 2 - d) * (1 if side == 3 else -1), u * HEIGHT_M / 2
            near = sum(1 for q in pts if math.hypot(x - q[0], y - q[1]) <= reach)
            if spaced((x, y), pts, EDGE_CLEAR_M) and near >= NEIGHBOURS_MIN:
                pts.append((x, y, "edge"))
                break
        else:
            return None
    pts.sort(key=lambda p: (p[0], p[1]))
    return pts


def neighbours(pts, reach):
    n = len(pts)
    adj = [set() for _ in range(n)]
    for i in range(n):
        for j in range(i + 1, n):
            if math.hypot(pts[i][0] - pts[j][0], pts[i][1] - pts[j][1]) <= reach:
                adj[i].add(j)
                adj[j].add(i)
    return adj


def hops_from(adj, src, via=None):
    """Shortest hop counts from src; with `via`, only through stations in it."""
    dist = {src: 0}
    frontier = [src]
    while frontier:
        nxt = []
        for a in frontier:
            if via is not None and a != src and a not in via:
                continue
            for b in adj[a]:
                if b not in dist:
                    dist[b] = dist[a] + 1
                    nxt.append(b)
        frontier = nxt
    return dist


def diameter(adj, via=None):
    worst, whole = 0, True
    for s in range(len(adj)):
        d = hops_from(adj, s, via)
        whole &= len(d) == len(adj)
        worst = max(worst, max(d.values()))
    return worst, whole


def choose_transports(pts, adj):
    n = len(pts)
    chosen = set()

    def far(i):
        if not chosen:
            return math.hypot(pts[i][0], pts[i][1])
        return min(math.hypot(pts[i][0] - pts[t][0], pts[i][1] - pts[t][1]) for t in chosen)

    covered = lambda i: any(t in adj[i] for t in chosen)
    while not all(covered(i) for i in range(n)):
        best = max((i for i in range(n) if i not in chosen),
                   key=lambda i: (sum(1 for j in adj[i] if not covered(j)), far(i)))
        chosen.add(best)
    # Join the transports into one set: a station beside two parts at once.
    while True:
        parts, seen = [], set()
        for t in sorted(chosen):
            if t in seen:
                continue
            part, stack = set(), [t]
            while stack:
                a = stack.pop()
                if a in part:
                    continue
                part.add(a)
                stack.extend(b for b in adj[a] if b in chosen and b not in part)
            parts.append(part)
            seen |= part
        if len(parts) == 1:
            break
        owner = {t: k for k, p in enumerate(parts) for t in p}
        best = max((i for i in range(n) if i not in chosen),
                   key=lambda i: (len({owner[b] for b in adj[i] if b in owner}), far(i)))
        chosen.add(best)
    while len(chosen) < N_TRANSPORT:
        chosen.add(max((i for i in range(n) if i not in chosen), key=far))
    return chosen


def main():
    seed = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    reach = reach_m()
    print("seed %d; SF7 at 14 dBm reaches %.0f m (exponent %.1f)" % (seed, reach, PHYSICS["exponent"]))
    scale = 1.0
    tried = set()
    while True:
        pts = draw(seed, scale, reach)
        tried.add(round(scale, 4))
        if pts is None:
            sys.exit("no room for the edge stations at scale %.2f" % scale)
        adj = neighbours(pts, reach)
        counts = [len(a) for a in adj]
        med = statistics.median(counts)
        diam, whole = diameter(adj)
        ok = NEIGHBOURS_MEDIAN[0] <= med <= NEIGHBOURS_MEDIAN[1] \
            and min(counts) >= NEIGHBOURS_MIN and whole
        print("  cluster spread x%.2f: neighbours min %d median %g max %d, diameter %d hops%s -> %s" % (
            scale, min(counts), med, max(counts), diam, "" if whole else " (not one piece)",
            "pass" if ok else "refused"))
        if ok:
            break
        nxt = scale * (1.05 if med > NEIGHBOURS_MEDIAN[1] else 0.95)
        if round(nxt, 4) in tried or not 0.2 <= nxt <= 5.0:
            sys.exit("no cluster spread passes for seed %d" % seed)
        scale = nxt

    if not DIAMETER[0] <= diam <= DIAMETER[1]:
        sys.exit("seed %d: the town is %d hops across, not %d to %d" % (seed, diam, *DIAMETER))
    transports = choose_transports(pts, adj)
    tdiam, twhole = diameter(adj, via=transports)
    counts = sorted(len(a) for a in adj)
    print("stations %d: street %d, clusters %s, edge %d" % (
        len(pts), sum(1 for p in pts if p[2] == "street"),
        "/".join(str(sum(1 for p in pts if p[2] == "cluster %d" % k))
                 for k in range(1, 8) if any(p[2] == "cluster %d" % k for p in pts)),
        sum(1 for p in pts if p[2] == "edge")))
    print("expected neighbours per station: min %d p10 %d median %g p90 %d max %d, mean %.1f" % (
        counts[0], counts[len(counts) // 10], statistics.median(counts), counts[9 * len(counts) // 10],
        counts[-1], sum(counts) / len(counts)))
    print("hop diameter %d; through transports only %d%s; transports %d" % (
        diam, tdiam, "" if twhole else " (not every pair joined)", len(transports)))
    lacking = [i for i in range(len(pts)) if not any(t in adj[i] for t in transports)]
    if lacking:
        sys.exit("stations without a transport neighbour: %s" % lacking)

    nodes = []
    for i, (x, y, where) in enumerate(pts):
        lat, lon = E.unproject(ORIGIN, x, y)
        nodes.append('  t%03d: { id: %d, pos: [%.6f, %.6f], setup: ["set s.rnsd.transport_enabled %d"] }'
                     '   # %s, %d neighbours'
                     % (i + 1, i + 1, lat, lon, 1 if i in transports else 0, where, len(adj[i])))
    head = [
        "# town100-lora, town100-supe, town100-supe0: 100 Reticulous stations in a 6 x 4 km town, written by",
        "# gen_town100.py %d (see its docstring for the parameters). Main street, %d" % (
            seed, len({p[2] for p in pts if p[2].startswith("cluster")})),
        "# neighbourhood clusters (spread x%.2f), %d edge stations; SF7 reaches %.0f m;" % (
            scale, N_EDGE, reach),
        "# neighbours per station median %g (min %d), hop diameter %d (%d through transports)." % (
            statistics.median(counts), counts[0], diam, tdiam),
        "# Transport on at %d stations. The files differ only in their SUPE lines." % len(transports),
        "origin: [%s, %s]" % ORIGIN,
        "physics: { exponent: %s, noise_figure_db: %s, capture_db: %s }" % (
            PHYSICS["exponent"], PHYSICS["noise_figure_db"], PHYSICS["capture_db"]),
        "kinds:",
        "  reticulous:",
        '    elf: "../../../reticulous/esp-idf/build.linux/reticulous.elf"',
        '    fixed: "../../../reticulous/esp-idf/build.linux/data_merged"',
        "setup:",
    ]
    for name, extra in (("town100-lora.yaml", []), ("town100-supe.yaml", SUPE),
                        ("town100-supe0.yaml", SUPE0)):
        lines = head + ['  - "%s"' % l for l in SHARED + extra] + ["nodes:"] + nodes
        with open(os.path.join(HERE, name), "w") as f:
            f.write("\n".join(lines) + "\n")
        print("wrote", os.path.join(HERE, name))


if __name__ == "__main__":
    main()
