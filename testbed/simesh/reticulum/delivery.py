"""Delivery of LXMF messages, from the senders' logs.

A message is proven delivered when its sender logs `delivered mid=<mid>`
(`DIRECT delivered`, `DIRECT resource delivered`, …) for the mid its `lxmf
send` answered with. `analyse` counts a traffic driver's sends by:

- route hops at send time: the `rnpath -j` the sender answered just before
  the send (`no path` where it held none);
- radio hops: the shortest path in the run's radio graph (the pairs the
  ether delivers at each sender's declared carrier, SF, bandwidth and
  power, by the run's loss tables) whose intermediate stations forward;
- size class (the driver's short, two-frame, over 500 B);
- latency: from the instant the send was answered to the sender's
  `delivered` line.

Log stamps are node time, which in a virtual-time run is the ether's epoch
(its `welcome` line in the run's record) plus T, and in a real run the wall
clock. Undelivered messages are tallied by the sender's last lxmf line for
the mid.
"""

import calendar
import collections
import datetime
import os
import re

ANSI = re.compile(r"\x1b\[[0-9;]*m")
STAMP = re.compile(r"^(\w{3} +\d+ \d\d:\d\d:\d\d\.\d{3}) ")
MID = re.compile(r"mid=(o_\S+)")
LOG_STAMP = re.compile(r"(\w{3}) +(\d+) (\d\d):(\d\d):(\d\d\.\d+)")
MONTHS = {m: i for i, m in enumerate(
    "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split(), 1)}


def station_logs(run_dir):
    """(node name, log path) for every station log in the run."""
    nodes = os.path.join(run_dir, "nodes")
    if not os.path.isdir(nodes):
        return
    for name in sorted(os.listdir(nodes)):
        path = os.path.join(nodes, name, "log")
        if os.path.isfile(path):
            yield name, path


def read_logs(run_dir, epoch, year):
    """(sender, mid) -> {'delivered': T or None, 'last': last lxmf line}."""
    out = {}
    for name, path in station_logs(run_dir):
        with open(path, errors="replace") as handle:
            for raw in handle:
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


def log_deliveries(run_dir, zero_wall, year):
    """Each sender's `DIRECT delivered` instants, in seconds after `zero_wall`
    (the wall-clock instant the caller's clock starts at): name -> [s, ...]."""
    out = {}
    for name, path in station_logs(run_dir):
        times = []
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if "DIRECT delivered" not in line and "DIRECT resource delivered" not in line:
                    continue
                m = LOG_STAMP.search(line)
                if not m:
                    continue
                mon, day, hh, mm, ss = m.groups()
                wall = calendar.timegm((year, MONTHS.get(mon, 1), int(day), int(hh), int(mm), 0)) \
                    + float(ss)
                times.append(wall - zero_wall)
        if times:
            out[name] = times
    return out


def radio_hops(adj, forwarders, src, dst):
    """Fewest hops from src to dst over `adj` (name -> names it reaches),
    passing only through `forwarders`; None when there is no such path."""
    frontier, seen, d = {src}, {src}, 0
    while frontier:
        d += 1
        nxt = set()
        for n in frontier:
            for m in adj.get(n, ()):
                if m == dst:
                    return d
                if m not in seen and m in forwarders:
                    seen.add(m)
                    nxt.add(m)
        frontier = nxt
    return None


def pct(b):
    return "%d/%d (%.1f%%)" % (b[0], b[1], 100.0 * b[0] / b[1]) if b[1] else "0/0"


def quantiles(v):
    v = sorted(v)
    if not v:
        return None
    q = lambda f: v[min(len(v) - 1, int(f * (len(v) - 1) + 0.5))]
    return {"n": len(v), "min": v[0], "p25": q(.25), "median": q(.5), "p75": q(.75),
            "p90": q(.9), "max": v[-1], "mean": sum(v) / len(v)}


def analyse(drive, logs, adj, forwarders):
    """The figures, and one row per message.

    `drive` is the traffic driver's result, `logs` what `read_logs` found,
    `adj` the radio graph by name, `forwarders` the names that forward.
    """
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
        gh = radio_hops(adj, forwarders, s["src"], s["dst"])
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
    return out, rows


def analyse_run(traffic_path, run_dir):
    """A traffic run's figures: (the driver's result, figures, rows), counted
    against the run's own record, logs and radio graph. Raises ValueError
    when the record holds no epoch (a run not in virtual time)."""
    import json

    from simesh import record
    from simesh.view import RunView

    with open(traffic_path, encoding="utf-8") as handle:
        drive = json.load(handle)
    view = RunView(run_dir)
    epoch = record.epoch_of(view.record_path)
    if epoch is None:
        raise ValueError("%s has no welcome line: not a virtual-time record" % view.record_path)
    year = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).year
    logs = read_logs(view.dir, epoch, year)
    graph = view.radio_graph(view.calling_hz())
    adj = {view.names[a]: {view.names[b] for b in bs} for a, bs in graph.items()}
    forwarders = {view.names[s] for s in view.forwarders()}
    out, rows = analyse(drive, logs, adj, forwarders)
    return drive, out, rows
