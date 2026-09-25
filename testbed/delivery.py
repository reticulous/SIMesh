#!/usr/bin/env python3
"""Delivery of a traffic.py run, from its output and the stations' logs.

    delivery.py TRAFFIC.json RUN_DIR [--json OUT]

A message is proven delivered when its sender logs `delivered mid=<mid>`
(`DIRECT delivered`, `DIRECT resource delivered`, …) for the mid its `lxmf
send` answered with. Counted by:

- route hops at send time: the `rnpath -j` the sender answered just before
  the send (`no path` where it held none);
- radio hops: the shortest path in the scenario's radio graph (pairs the
  ether delivers at the calling channel's SF, bandwidth and power) whose
  intermediate stations are transports;
- size class (traffic.py's short, two-frame, over 500 B);
- latency: from the instant the send was answered to the sender's
  `delivered` line.

Log stamps are node time, which in a virtual-time run is the ether's epoch
(its `welcome` line in the run's `record.tsv`) plus T. Undelivered messages
are tallied by the sender's last lxmf line for the mid.
"""
import argparse
import collections
import datetime
import json
import os
import re
import sys

import yaml

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(SIM_DIR, "..", "ether"))
import ether as E  # noqa: E402 - the path is set just above

ANSI = re.compile(r"\x1b\[[0-9;]*m")
STAMP = re.compile(r"^(\w{3} +\d+ \d\d:\d\d:\d\d\.\d{3}) ")
MID = re.compile(r"mid=(o_\S+)")


def epoch_of(run_dir):
    with open(os.path.join(run_dir, "record.tsv"), encoding="utf-8") as f:
        for line in f:
            if '"welcome"' in line:
                return json.loads(line.split("\t", 3)[3])["epoch"] / 1e6
    return None


def radio_of(lines):
    """The calling channel's SF, bandwidth and power from `lora 0 …` setup lines."""
    sf, bw, power = 8, 125_000, 14
    for line in lines:
        m = re.match(r"lora 0 (sf|bw|txp) (\d+(?:\.\d+)?)$", line.strip())
        if m:
            v = float(m.group(2))
            if m.group(1) == "sf":
                sf = int(v)
            elif m.group(1) == "bw":
                bw = int(v * 1000)
            else:
                power = v
    return sf, bw, power


def graph(path, freq=869_525_000):
    data = yaml.safe_load(open(path))
    sf, bw, power = radio_of(data.get("setup") or [])
    e = E.Ether.__new__(E.Ether)
    e.physics, e.places, e.obstructions, e.stations = E.Physics.from_dict(data.get("physics")), {}, {}, {}
    origin = tuple(data["origin"])
    ids, transport = {}, {}
    lines = list(data.get("setup") or [])
    for name, node in data["nodes"].items():
        x, y = E.project(origin, node["pos"][0], node["pos"][1])
        e.places[node["id"]] = E.Placement(x, y, node.get("gain_db", 0.0))
        ids[name] = node["id"]
        own = lines + list(node.get("setup") or [])
        transport[name] = [l for l in own if "transport_enabled" in l][-1:] == ["set s.rnsd.transport_enabled 1"]
    for w in data.get("obstructions") or []:
        a, b = w["between"]
        e.obstructions[frozenset((ids[a], ids[b]))] = float(w["db"])
    adj = collections.defaultdict(set)
    for a in ids:
        for b in ids:
            if a != b and e.audible(e.level(ids[a], ids[b], freq, power), bw, sf):
                adj[a].add(b)
    return adj, transport


def radio_hops(adj, transport, src, dst):
    frontier, seen, d = {src}, {src}, 0
    while frontier:
        d += 1
        nxt = set()
        for n in frontier:
            for m in adj[n]:
                if m == dst:
                    return d
                if m not in seen and transport.get(m):
                    seen.add(m)
                    nxt.add(m)
        frontier = nxt
    return None


def read_logs(run_dir, epoch, year):
    """mid -> {'delivered': T or None, 'last': last lxmf line}, from every station's log."""
    out = {}
    nodes = os.path.join(run_dir, "nodes")
    for name in os.listdir(nodes):
        p = os.path.join(nodes, name, "log")
        if not os.path.isfile(p):
            continue
        for raw in open(p, errors="replace"):
            if "mid=o_" not in raw:
                continue
            line = ANSI.sub("", raw).rstrip()
            m = MID.search(line)
            s = STAMP.match(line)
            if not m or not s:
                continue
            t = datetime.datetime.strptime("%d %s" % (year, s.group(1)), "%Y %b %d %H:%M:%S.%f")
            t = t.replace(tzinfo=datetime.timezone.utc).timestamp() - epoch
            rec = out.setdefault((name, m.group(1)), {"delivered": None, "last": None})
            rec["last"] = line[s.end():]
            if "delivered mid=" in line and rec["delivered"] is None:
                rec["delivered"] = t
    return out


def pct(b):
    return "%d/%d (%.1f%%)" % (b[0], b[1], 100.0 * b[0] / b[1]) if b[1] else "0/0"


def quantiles(v):
    v = sorted(v)
    if not v:
        return None
    q = lambda f: v[min(len(v) - 1, int(f * (len(v) - 1) + 0.5))]
    return {"n": len(v), "min": v[0], "p25": q(.25), "median": q(.5), "p75": q(.75),
            "p90": q(.9), "max": v[-1], "mean": sum(v) / len(v)}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("traffic")
    ap.add_argument("run_dir")
    ap.add_argument("--json")
    args = ap.parse_args()
    drive = json.load(open(args.traffic))
    epoch = epoch_of(args.run_dir)
    year = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).year
    logs = read_logs(args.run_dir, epoch, year)
    adj, transport = graph(os.path.join(args.run_dir, "scenario.yaml"))

    total = [0, 0]
    by_route = collections.defaultdict(lambda: [0, 0])
    by_radio = collections.defaultdict(lambda: [0, 0])
    by_cls = collections.defaultdict(lambda: [0, 0])
    lat, lat_cls = [], collections.defaultdict(list)
    lat_route = collections.defaultdict(list)
    last_words = collections.Counter()
    no_mid = 0
    rows = []
    for s in drive.get("sends", []):
        if not s.get("mid"):
            no_mid += 1
        rec = logs.get((s["src"], s.get("mid")), {}) if s.get("mid") else {}
        t_sent = (s.get("t_sent") or 0) / 1e6
        ok = rec.get("delivered") is not None
        rh = s.get("hops")
        route = "no path" if rh is None else (str(rh) if rh <= 6 else "7+")
        gh = radio_hops(adj, transport, s["src"], s["dst"])
        radio = "none" if gh is None else (str(gh) if gh <= 6 else "7+")
        for b in (total, by_route[route], by_radio[radio], by_cls[s["cls"]]):
            b[0] += ok
            b[1] += 1
        if ok:
            d = rec["delivered"] - t_sent
            lat.append(d)
            lat_cls[s["cls"]].append(d)
            lat_route[route].append(d)
        else:
            w = rec.get("last") or ("no mid" if not s.get("mid") else "no log line")
            w = re.sub(r"\b(o_\S+|[0-9a-f]{8,}|lxmf\.id\d\.\S+)", "…", w)
            w = re.sub(r"\d+", "N", w)
            last_words[w] += 1
        rows.append({"marker": s["marker"], "src": s["src"], "dst": s["dst"], "cls": s["cls"],
                     "route_hops": rh, "radio_hops": gh, "delivered": ok,
                     "latency_s": (rec["delivered"] - t_sent) if ok else None})
    order = lambda k: (k in ("no path", "none"), k == "7+", k)
    out = {"sent": total[1], "delivered": total[0], "no_mid": no_mid,
           "overall": pct(total),
           "by_route_hops": {k: pct(by_route[k]) for k in sorted(by_route, key=order)},
           "by_radio_hops": {k: pct(by_radio[k]) for k in sorted(by_radio, key=order)},
           "by_class": {k: pct(by_cls[k]) for k in ("short", "two", "big") if k in by_cls},
           "latency_s": quantiles(lat),
           "latency_by_class": {k: quantiles(v) for k, v in lat_cls.items()},
           "latency_by_route_hops": {k: quantiles(lat_route[k]) for k in sorted(lat_route, key=order)},
           "undelivered_last_word": last_words.most_common(25)}
    text = json.dumps(out, indent=1)
    print(text)
    if args.json:
        with open(args.json, "w") as f:
            json.dump(dict(out, messages=rows), f, indent=1)


if __name__ == "__main__":
    main()
