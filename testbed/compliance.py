#!/usr/bin/env python3
"""EN 300 220 compliance of a run: which nodes went over their time or power
budget on the air.

    compliance.py RUN [--record PATH] [--json]

Reads the run's `record.tsv` (or --record) and, for every node and every EU
short-range-device band entry it transmitted in, finds:

- **time**: its seconds on the air in the worst sliding hour, against the
  entry's duty cycle; where the entry permits polite spectrum access (PSA,
  EN 300 220-1 clause 5.21) instead, also its worst hour in any 200 kHz of
  the entry, its longest frame and its shortest pause on one carrier,
  against PSA's 100 s per hour, 1 s and 100 ms. Within either is within;
- **power**: its highest e.r.p., each frame's `power_dbm` plus the peak
  gain of the node's antenna less 2.15 dB (dBi to dBd), against the entry's
  limit.

A frame counts in the entry its whole occupied bandwidth (carrier ± half its
bandwidth) lies in. One that lies in none but whose carrier is in 433.05–434.79
or 863–870 MHz is on spectrum no entry allows, and is over by itself; one
outside those ranges is outside what EN 300 220's harmonised entries cover
and is counted, not judged. Whether a node listened before talking is not in
the record: a verdict within PSA assumes it did.

Without --json it prints the report section the runner appends to every
run's `report.md` (`section`).
"""
import argparse
import bisect
import collections
import json
import math
import os
import sys

import antennas as antennas_module
from simesh import record as record_module
from simesh.view import RunView

HOUR_S = 3600.0
DBI_TO_DBD = 2.15
PSA_S = 100.0                   # Tcum_on per hour, per 200 kHz
PSA_PORTION_HZ = 200_000
PSA_TON_MAX_S = 1.0             # one transmission
PSA_TOFF_MIN_S = 0.1            # between transmissions on one carrier


def mw_dbm(mw):
    return 10 * math.log10(mw)


# EN 300 220-2 annex B, table B.1, the non-specific short-range-device
# entries: (low Hz, high Hz, e.r.p. limit dBm, duty cycle or None, PSA
# permitted, e.r.p. at or below which no duty cycle applies, or None).
# 868.6–868.7, 869.2–869.4 and 869.65–869.7 MHz are alarm allocations and in
# no entry.
ENTRIES = [
    (433_050_000, 434_790_000, mw_dbm(10), 0.10, False, None),
    (863_000_000, 865_000_000, mw_dbm(25), 0.001, True, None),
    (865_000_000, 868_000_000, mw_dbm(25), 0.01, True, None),
    (868_000_000, 868_600_000, mw_dbm(25), 0.01, True, None),
    (868_700_000, 869_200_000, mw_dbm(25), 0.001, True, None),
    (869_400_000, 869_650_000, mw_dbm(500), 0.10, False, None),
    (869_700_000, 870_000_000, mw_dbm(25), 0.01, False, mw_dbm(5)),
]
COVERED = [(433_050_000, 434_790_000), (863_000_000, 870_000_000)]
EDGE_HZ = 1_000                 # a carrier's crystal offset, within an edge


def entry_of(freq, bw):
    """The entry a frame's occupied bandwidth lies in; None for none."""
    lo, hi = freq - bw / 2, freq + bw / 2
    for entry in ENTRIES:
        if lo >= entry[0] - EDGE_HZ and hi <= entry[1] + EDGE_HZ:
            return entry
    return None


def covered(freq):
    return any(lo <= freq <= hi for lo, hi in COVERED)


def band_name(entry):
    return "%s–%s MHz" % tuple(("%.3f" % (f / 1e6)).rstrip("0").rstrip(".")
                               for f in entry[:2])


class OnAir:
    """A set of transmissions, for the most time on the air in any window."""

    def __init__(self, spans):
        self.starts = sorted(a for a, _ in spans)
        self.ends = sorted(b for _, b in spans)
        self.sum_starts = [0.0]
        for a in self.starts:
            self.sum_starts.append(self.sum_starts[-1] + a)
        self.sum_ends = [0.0]
        for b in self.ends:
            self.sum_ends.append(self.sum_ends[-1] + b)

    def upto(self, t):
        """Seconds on the air before t."""
        i = bisect.bisect_left(self.starts, t)
        j = bisect.bisect_left(self.ends, t)
        return (i * t - self.sum_starts[i]) - (j * t - self.sum_ends[j])

    def worst(self, window=HOUR_S):
        """(seconds, start) of the window with the most time on the air: one
        starting at a frame's start or ending at a frame's end is it."""
        best = (0.0, self.starts[0] if self.starts else 0.0)
        for lo in self.starts + [b - window for b in self.ends]:
            got = self.upto(lo + window) - self.upto(lo)
            if got > best[0] + 1e-9:
                best = (got, lo)
        return best


def frames_of(path):
    """(station id, start s, end s, carrier Hz, bandwidth Hz, power dBm) for
    every transmission in the record."""
    out = []
    for _stamp, direction, sid, msg in record_module.lines(path):
        if direction != "in" or msg.get("type") != "tx":
            continue
        t0, t_end = msg.get("t0"), msg.get("t_end")
        if t0 is None or t_end is None:
            continue
        out.append((sid, t0 / 1e6, t_end / 1e6, int(msg.get("freq") or 0),
                    int(msg.get("bw") or 125_000), float(msg.get("power_dbm") or 0)))
    return out


def shortest_pause(frames):
    """Seconds of the shortest silence between two frames on one carrier."""
    by_carrier = collections.defaultdict(list)
    for f in frames:
        by_carrier[f[3]].append((f[1], f[2]))
    best = None
    for spans in by_carrier.values():
        spans.sort()
        for (_, end), (start, _) in zip(spans, spans[1:]):
            gap = start - end
            best = gap if best is None or gap < best else best
    return best


def judge(frames, entry, gain_dbi):
    """One node's frames in one entry, judged."""
    lo_hz, hi_hz, erp_limit, duty, psa, free_below = entry
    air = OnAir([(f[1], f[2]) for f in frames])
    worst_s, worst_at = air.worst()
    erp = max(f[5] for f in frames) + gain_dbi - DBI_TO_DBD
    power_ok = erp <= erp_limit + 0.005
    allowed = None if (free_below is not None and erp <= free_below + 0.005) else duty * HOUR_S
    row = {"band": band_name(entry), "frames": len(frames),
           "worst_hour_s": round(worst_s, 3), "worst_hour_from": round(worst_at, 3),
           "allowed_s": allowed, "duty": None if allowed is None else duty,
           "erp_dbm": round(erp, 2), "erp_limit_dbm": round(erp_limit, 2),
           "power_ok": power_ok}
    time_ok = allowed is None or worst_s <= allowed + 1e-6
    row["time"] = "duty cycle" if time_ok else "over"
    if not time_ok and psa:
        portion_s = 0.0
        for c in sorted({f[3] for f in frames}):
            near = [(f[1], f[2]) for f in frames
                    if f[3] - f[4] / 2 < c + PSA_PORTION_HZ / 2
                    and f[3] + f[4] / 2 > c - PSA_PORTION_HZ / 2]
            portion_s = max(portion_s, OnAir(near).worst()[0])
        longest = max(f[2] - f[1] for f in frames)
        pause = shortest_pause(frames)
        row["psa"] = {"worst_hour_s": round(portion_s, 3), "longest_frame_s": round(longest, 3),
                      "shortest_pause_s": None if pause is None else round(pause, 3)}
        if (portion_s <= PSA_S + 1e-6 and longest <= PSA_TON_MAX_S
                and (pause is None or pause >= PSA_TOFF_MIN_S)):
            row["time"] = "PSA"
    return row


def analyse(run_dir, record_path=None):
    """Per node, its rows by entry, and what it sent outside every entry."""
    view = RunView(run_dir)
    path = record_path or view.record_path
    if not os.path.isfile(path):
        return None
    by_node = collections.defaultdict(list)
    for f in frames_of(path):
        by_node[f[0]].append(f)
    result = {}
    for sid in sorted(by_node):
        name = view.names.get(sid, "station %d" % sid)
        node = view.nodes.get(name) or {}
        gain = antennas_module.spec_of(node.get("antenna"))["peak_dbi"]
        grouped = collections.defaultdict(list)
        banned, uncovered = [], []
        for f in by_node[sid]:
            entry = entry_of(f[3], f[4])
            if entry is not None:
                grouped[entry].append(f)
            elif covered(f[3]):
                banned.append(f)
            else:
                uncovered.append(f)
        rows = [judge(grouped[e], e, gain) for e in ENTRIES if e in grouped]
        result[name] = {
            "gain_dbi": gain, "rows": rows,
            "no_entry": {"frames": len(banned),
                         "seconds": round(sum(f[2] - f[1] for f in banned), 3),
                         "carriers_mhz": sorted({round(f[3] / 1e6, 3) for f in banned})},
            "outside": {"frames": len(uncovered),
                        "seconds": round(sum(f[2] - f[1] for f in uncovered), 3)}}
    return result


def clock(t):
    t = int(t)
    return "%02d:%02d:%02d" % (t // 3600, t // 60 % 60, t % 60)


def section(run_dir, record_path=None):
    """The report's compliance section, as Markdown."""
    result = analyse(run_dir, record_path)
    head = "## ETSI compliance\n\n"
    if result is None:
        return head + "No record: nothing to check.\n"
    if not result:
        return head + "No node transmitted.\n"
    lines = [head.rstrip("\n"), "",
             "Against EN 300 220's harmonised short-range-device entries: each node's "
             "seconds on the air in its worst hour, against the entry's duty cycle or, where "
             "the entry permits it, polite spectrum access (100 s an hour in any 200 kHz, "
             "frames up to 1 s, 100 ms between frames on one carrier, listening before "
             "talking assumed); and its highest e.r.p., transmit power plus its antenna's "
             "peak gain less 2.15 dB.", "",
             "| node | band | worst hour on air | allowed | highest e.r.p. | limit | verdict |",
             "|---|---|---|---|---|---|---|"]
    over = {}
    notes = []
    for name, got in result.items():
        for row in got["rows"]:
            worst = "%.1f s (%.2f %%) from T %s" % (row["worst_hour_s"],
                                                   row["worst_hour_s"] / HOUR_S * 100,
                                                   clock(row["worst_hour_from"]))
            allowed = ("no limit at this power" if row["allowed_s"] is None
                       else "%g s (%g %%)" % (row["allowed_s"], row["duty"] * 100))
            verdict = []
            if row["time"] == "over":
                verdict.append("**over time**")
                over.setdefault(name, []).append("time in %s" % row["band"])
            elif row["time"] == "PSA":
                verdict.append("within PSA")
            if not row["power_ok"]:
                verdict.append("**over power**")
                over.setdefault(name, []).append("power in %s" % row["band"])
            if "psa" in row:
                p = row["psa"]
                allowed += "; PSA: %.1f s, longest frame %.2f s%s" % (
                    p["worst_hour_s"], p["longest_frame_s"],
                    "" if p["shortest_pause_s"] is None
                    else ", shortest pause %.3f s" % p["shortest_pause_s"])
            lines.append("| %s | %s | %s | %s | %.1f dBm | %.1f dBm | %s |" % (
                name, row["band"], worst, allowed, row["erp_dbm"], row["erp_limit_dbm"],
                ", ".join(verdict) or "ok"))
        if got["no_entry"]["frames"]:
            n = got["no_entry"]
            over.setdefault(name, []).append("spectrum no entry allows")
            lines.append("| %s | none (%s MHz) | %.1f s in %d frames | nothing | — | — | "
                         "**not permitted** |" % (
                             name, ", ".join("%g" % c for c in n["carriers_mhz"]),
                             n["seconds"], n["frames"]))
        if got["outside"]["frames"]:
            n = got["outside"]["frames"]
            notes.append("%s sent %d frame%s (%.1f s) outside 433.05–434.79 and 863–870 MHz, "
                         "which no EN 300 220 entry covers; not judged here."
                         % (name, n, "" if n == 1 else "s", got["outside"]["seconds"]))
    lines.append("")
    if over:
        lines.append("**Over budget:** " + "; ".join(
            "%s (%s)" % (name, ", ".join(what)) for name, what in over.items()) + ".")
    else:
        lines.append("Every node stayed within its time and power budgets.")
    for note in notes:
        lines += ["", note]
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run")
    ap.add_argument("--record", help="the record to read (default: the run's record.tsv)")
    ap.add_argument("--json", action="store_true", help="the figures, not the section")
    args = ap.parse_args(argv)
    if args.json:
        print(json.dumps(analyse(args.run, args.record), indent=1))
    else:
        print(section(args.run, args.record), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
