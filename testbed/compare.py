#!/usr/bin/env python3
"""Two runs of one scenario, side by side, from their records.

    compare.py A/record.tsv B/record.tsv --scenario A/scenario.yaml
    compare.py A/record.tsv --scenario A/scenario.yaml          one run's figures

A run in real time and a run in virtual time are the same network only if the
same things happen in both. Frame for frame they cannot agree — the firmware
draws its own random numbers — so this compares what a run achieves and when:

- per station: when it joined the ether, when its radio first listened, the
  announces it originated and forwarded, its frames by type, and how many of
  its frames some receiver lost to a collision;
- per pair: the fewest hops at which one station heard the other's
  announces, which is the path the record shows it could have learned;
- milestones on the run's own clock, from the first hello (or from the zero
  `--starts` gives each run, such as the instant its scenario loaded): the
  last station to join, the last radio to listen, the first announce of the
  last station to announce, and the first path at each hop count.

With `--cli A.json B.json`, each run's final `rnpath` answers (a JSON object,
station name to what it printed) are compared too: paths known by hop count.
With `--logs A B`, each run's directory, the messages its senders logged as
delivered are counted as well. `--until S` keeps the first S seconds of each
run, so a record that ran on while its run was being stopped does not count
the extra.
"""

import argparse
import collections
import json
import os
import re
import sys
import time

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SIM_DIR)

import seq  # noqa: E402

KINDS = ("ANNOUNCE", "DATA", "LINKREQUEST", "PROOF")


class Run:
    """One record read for what it achieved."""

    def __init__(self, path, names):
        self.path = path
        self.names = names                  # sid -> name
        self.t0 = None
        self.joined = {}                    # sid -> first hello
        self.listening = {}                 # sid -> first RX state
        self.originated = collections.Counter()
        self.forwarded = collections.Counter()
        self.first_announce = {}            # sid -> first originated announce
        self.types = collections.defaultdict(collections.Counter)
        self.tx = collections.Counter()
        self.collided = collections.Counter()   # frames some receiver lost to a CRC failure
        self.rx_clean = collections.Counter()
        self.rx_crc = collections.Counter()
        self.owner = {}                     # announced destination -> the station that originated it
        self.heard_at = {}                  # (owner, receiver) -> (fewest hops, when first at that count)
        self.first_path = {}                # hops -> when a path of that length first appeared
        self.end = 0.0
        self.hello_wall = None              # the run's zero, on the wall clock
        self.year = 1970
        self.start = getattr(self, "start", None)   # the run's zero in record stamps, if given
        self.read()

    def stamp(self, text):
        return seq.parse_time(text)

    def wall_of(self, text):
        """The wall-clock instant of a real run's stamp; None for T."""
        if ":" not in text:
            return None
        from datetime import datetime
        when = datetime.fromisoformat(text)
        self.year = when.year
        return when.timestamp()

    def read(self):
        halves = set()
        arriving = {}                       # ether frame id -> (sender, payload, when)
        with open(self.path, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("#"):
                    continue
                fields = line.rstrip("\n").split("\t")
                if len(fields) != 4:
                    continue
                stamp, direction, sid, blob = fields
                try:
                    msg = json.loads(blob)
                    sid = int(sid)
                except ValueError:
                    continue
                at = self.stamp(stamp)
                kind = msg.get("type")
                if self.t0 is None and self.start is not None and at >= self.start:
                    self.t0 = self.start
                    wall = self.wall_of(stamp)
                    if wall is not None:
                        self.hello_wall = wall - (at - self.start)
                if kind == "hello" and direction == "in":
                    if self.t0 is None:
                        self.t0 = at
                        self.hello_wall = self.wall_of(stamp)
                    self.joined.setdefault(sid, at)
                if kind == "welcome" and self.hello_wall is None and msg.get("epoch") \
                        and msg.get("mode") == "virtual" and self.t0 is not None:
                    self.hello_wall = msg["epoch"] / 1e6 + self.t0
                    self.year = time.gmtime(self.hello_wall).tm_year
                if self.t0 is None:
                    continue
                at -= self.t0
                if at < 0:
                    at += 86400             # a real run across midnight
                if getattr(self, "until", None) is not None and at > self.until:
                    break
                self.end = max(self.end, at)
                if kind == "state" and direction == "in" and msg.get("mode") == "RX":
                    self.listening.setdefault(sid, at)
                elif kind == "tx" and direction == "in":
                    self.transmitted(sid, msg, at, halves, arriving)
                elif kind == "rx_begin" and direction == "out" and not msg.get("cad"):
                    pass
                elif kind == "rx_end" and direction == "out":
                    self.received(sid, msg, at, arriving)
        if self.t0 is not None:
            self.joined = {s: t - self.t0 for s, t in self.joined.items()}

    def transmitted(self, sid, msg, at, halves, arriving):
        frame = seq.Frame(at, sid, msg)
        payload = frame.payload
        if payload and payload[0] & seq.RNODE_FLAG_SPLIT and payload[0] not in seq.SUPE_TYPE:
            frame.part = 2 if sid in halves else 1
            halves.symmetric_difference_update({sid})
        label = frame.label
        kind = label.split()[0] if label else "?"
        if kind == "SUPE":
            kind = "SUPE"
        elif kind not in KINDS:
            kind = "other"
        self.tx[sid] += 1
        self.types[sid][kind] += 1
        info = self.announce(payload, frame.part)
        if info is not None:
            dest, hops = info
            if hops == 0:
                self.originated[sid] += 1
                self.first_announce.setdefault(sid, at)
                self.owner.setdefault(dest, sid)
            else:
                self.forwarded[sid] += 1
        # The ether numbers the frame in the rx_begin that follows; the record
        # keeps them in order, so the next rx messages naming a new id are
        # this frame's.
        self.last_tx = (sid, info, at)

    @staticmethod
    def announce(payload, part):
        """(destination, hops) of an announce frame, else None."""
        if part == 2 or not payload or payload[0] in seq.SUPE_TYPE:
            return None
        body = payload[1:]
        if len(body) < 19 or body[0] & 0x80:
            return None
        flags = body[0]
        if flags & 0x03 != 1:
            return None
        hdr2 = bool(flags & 0x40)
        dest = body[2 + (16 if hdr2 else 0):][:16]
        return dest.hex(), body[1]

    def received(self, rsid, msg, at, arriving):
        verdict = msg.get("verdict")
        eid = msg.get("id")
        payload = msg.get("payload") or ""
        if verdict == "clean":
            self.rx_clean[rsid] += 1
        else:
            self.rx_crc[rsid] += 1
        frame = arriving.get(eid)
        if frame is None:
            import base64
            raw = base64.b64decode(payload)
            frame = (self.sender_of(eid), raw)
            arriving[eid] = frame
        sender, raw = frame
        if verdict != "clean":
            if sender is not None and (eid, "crc") not in arriving:
                arriving[(eid, "crc")] = True
                self.collided[sender] += 1
            return
        info = self.announce(raw, 0)
        if info is None:
            return
        dest, hops = info
        owner = self.owner.get(dest)
        if owner is None or owner == rsid:
            return
        path = hops + 1
        key = (owner, rsid)
        best = self.heard_at.get(key)
        if best is None or path < best[0]:
            self.heard_at[key] = (path, at)
        self.first_path.setdefault(path, at)

    # ---- the figures -----------------------------------------------------

    def name(self, sid):
        return self.names.get(sid, str(sid))

    def milestones(self):
        out = collections.OrderedDict()
        out["last station joined"] = max(self.joined.values()) if self.joined else None
        out["last radio listening"] = max(self.listening.values()) if self.listening else None
        announcing = [s for s in self.joined if s in self.first_announce]
        out["every station announced"] = (max(self.first_announce[s] for s in announcing)
                                          if announcing and len(announcing) == len(self.joined)
                                          else None)
        for hops in sorted(self.first_path):
            out["first %d-hop path" % hops] = self.first_path[hops]
        return out

    def path_counts(self):
        return collections.Counter(p for p, _ in self.heard_at.values())


def rx_senders(path):
    """Ether frame id -> the station that sent it, from the order of the record."""
    senders = {}
    current = None
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            fields = line.rstrip("\n").split("\t")
            if len(fields) != 4:
                continue
            try:
                msg = json.loads(fields[3])
            except ValueError:
                continue
            kind = msg.get("type")
            if kind == "tx" and fields[1] == "in":
                current = int(fields[2])
            elif kind == "rx_begin" and fields[1] == "out" and current is not None:
                senders.setdefault(msg.get("id"), current)
    return senders


LOG_STAMP = re.compile(r"(\w{3}) +(\d+) (\d\d):(\d\d):(\d\d\.\d+)")
MONTHS = {m: i for i, m in enumerate(
    "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split(), 1)}


def log_deliveries(run_dir, run):
    """Each reticulous sender's `DIRECT delivered` lines, on the run's clock.

    A station stamps its log from time(), which in a virtual-time run is the
    run's epoch plus node time and in a real one the wall clock, so either
    way the stamp less the first hello's wall-clock instant is run time.
    """
    import calendar
    out = {}
    nodes = os.path.join(run_dir, "nodes")
    if not os.path.isdir(nodes) or run.hello_wall is None:
        return out
    year = run.year
    for name in sorted(os.listdir(nodes)):
        path = os.path.join(nodes, name, "log")
        if not os.path.isfile(path):
            continue
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
                times.append(wall - run.hello_wall)
        if times:
            out[name] = times
    return out


def cli_paths(path):
    """Paths known by hop count, per station, out of a gathered rnpath JSON."""
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    if isinstance(data.get("gathered"), dict):
        data = data["gathered"].get("rnpath", {})
    out = {}
    for name, text in data.items():
        m = re.search(r"by hops:\s*([0-9: ]+)", text or "")
        counts = collections.Counter()
        if m:
            for part in m.group(1).split():
                h, _, n = part.partition(":")
                if h.isdigit() and n.isdigit():
                    counts[int(h)] += int(n)
        out[name] = counts
    return out


def fmt(value):
    if value is None:
        return "—"
    if isinstance(value, float):
        return "%.1f" % value
    return str(value)


def table(rows, header):
    widths = [max(len(str(r[i])) for r in rows + [header]) for i in range(len(header))]
    lines = ["  ".join(str(h).ljust(w) for h, w in zip(header, widths))]
    lines.append("  ".join("-" * w for w in widths))
    for r in rows:
        lines.append("  ".join(str(c).ljust(w) for c, w in zip(r, widths)))
    return "\n".join(lines)


def report(runs, labels, cli=None, deliveries=None):
    out = []
    names = runs[0].names
    sids = sorted(set().union(*(r.joined for r in runs)))

    out.append("Milestones, seconds from each run's zero (--starts, else its first hello)")
    keys = []
    marks = []
    for i, r in enumerate(runs):
        m = r.milestones()
        if deliveries:
            firsts = [t for times in deliveries[i].values() for t in times]
            m["first message proven delivered"] = min(firsts) if firsts else None
        marks.append(m)
        for k in m:
            if k not in keys:
                keys.append(k)
    rows = [[k] + [fmt(m.get(k)) for m in marks] for k in keys]
    out.append(table(rows, ["milestone"] + labels))

    if deliveries:
        out.append("")
        out.append("LXMF messages proven delivered at the sender, run seconds")
        senders = sorted(set().union(*(d.keys() for d in deliveries)))
        rows = [[s] + [" ".join("%.0f" % t for t in d.get(s, [])) or "—" for d in deliveries]
                for s in senders]
        out.append(table(rows, ["sender"] + labels))

    out.append("")
    out.append("Per station: announces originated/forwarded, frames, frames lost to a collision somewhere")
    header = ["station"]
    for label in labels:
        header += ["%s ann" % label, "%s fwd" % label, "%s tx" % label, "%s coll" % label,
                   "%s first ann" % label]
    rows = []
    for sid in sids:
        row = [names.get(sid, sid)]
        for r in runs:
            row += [r.originated[sid], r.forwarded[sid], r.tx[sid], r.collided[sid],
                    fmt(r.first_announce.get(sid))]
        rows.append(row)
    out.append(table(rows, header))

    out.append("")
    out.append("Frames by type, all stations")
    kinds = sorted(set().union(*(set(k for s in r.types.values() for k in s) for r in runs)))
    rows = [[k] + [sum(s[k] for s in r.types.values()) for r in runs] for k in kinds]
    rows.append(["all"] + [sum(r.tx.values()) for r in runs])
    rows.append(["receptions clean"] + [sum(r.rx_clean.values()) for r in runs])
    rows.append(["receptions crc"] + [sum(r.rx_crc.values()) for r in runs])
    out.append(table(rows, ["type"] + labels))

    out.append("")
    out.append("Pairs with a path in the record, by hops")
    hops = sorted(set().union(*(r.path_counts() for r in runs)))
    rows = [[h] + [r.path_counts()[h] for r in runs] for h in hops]
    out.append(table(rows, ["hops"] + labels))

    pairs = sorted(set().union(*(r.heard_at for r in runs)))
    differ = [p for p in pairs
              if len({r.heard_at.get(p, (None,))[0] for r in runs}) > 1]
    if differ:
        out.append("")
        out.append("Pairs whose hop count differs")
        rows = [["%s <- %s" % (names.get(b, b), names.get(a, a))]
                + [fmt(r.heard_at.get((a, b), (None,))[0]) for r in runs] for a, b in differ]
        out.append(table(rows, ["pair"] + labels))

    if cli:
        out.append("")
        out.append("rnpath at the end: paths known, by hops")
        stations = sorted(set().union(*(c.keys() for c in cli)))
        rows = []
        for st in stations:
            row = [st]
            for c in cli:
                counts = c.get(st, collections.Counter())
                row.append(" ".join("%d:%d" % (h, counts[h]) for h in sorted(counts)) or "—")
            rows.append(row)
        out.append(table(rows, ["station"] + labels))
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description="compare two runs of one scenario")
    ap.add_argument("records", nargs="+", help="one or two record.tsv files")
    ap.add_argument("--scenario", required=True, help="the scenario, to name the stations")
    ap.add_argument("--labels", default="A,B")
    ap.add_argument("--cli", nargs="*", help="gathered rnpath JSON, one per record")
    ap.add_argument("--until", type=float, help="only the first this many seconds of each run")
    ap.add_argument("--logs", nargs="*",
                    help="each record's run directory, for the stations' logs")
    ap.add_argument("--starts", help="each run's zero, comma separated, in its record's "
                                     "stamps: T in seconds for a virtual run, the time of "
                                     "day in seconds for a real one (default: the first hello)")
    args = ap.parse_args(argv)
    names = seq.read_scenario(args.scenario)
    runs = []
    starts = [float(s) for s in args.starts.split(",")] if args.starts else []
    for i, path in enumerate(args.records):
        senders = rx_senders(path)
        run = Run.__new__(Run)
        run.sender_of = senders.get
        run.until = args.until
        run.start = starts[i] if i < len(starts) else None
        Run.__init__(run, path, names)
        runs.append(run)
    labels = args.labels.split(",")[:len(runs)]
    cli = [cli_paths(p) for p in args.cli] if args.cli else None
    deliveries = None
    if args.logs:
        deliveries = []
        for run, run_dir in zip(runs, args.logs):
            found = log_deliveries(run_dir, run)
            if args.until is not None:
                found = {k: [t for t in v if t <= args.until] for k, v in found.items()}
                found = {k: v for k, v in found.items() if v}
            deliveries.append(found)
    print(report(runs, labels, cli, deliveries))
    return 0


if __name__ == "__main__":
    sys.exit(main())
