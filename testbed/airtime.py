#!/usr/bin/env python3
"""Airtime, transmit power and losses from an ether record.

    airtime.py RECORD [--scenario YAML] [--from S] [--to S] [--calling HZ]
               [--gap S] [--busy] [--roles] [--json OUT]

Reads `record.tsv` (a virtual-time run's: the first column is T in seconds)
and counts every transmission that starts inside [--from, --to):

- airtime: total seconds on the air, per station (mean and maximum), and as
  a fraction of the window; split into the calling channel (--calling, the
  configured frequency, 869.525 MHz by default) and every other carrier,
  which under SUPE's channel plan are its traffic channels; and by what the
  frame is (Reticulum packet type and context, SUPE frame type), on every
  carrier and on the calling channel alone (`by_kind`, `by_kind_calling`),
  so the announce share can be read off;
- transmit power: `power_dbm` of each frame, on the calling channel and on
  the other carriers, weighted by frame and by airtime, with its minimum,
  quartiles and maximum; and exchanges on the other carriers, where an
  exchange is a run of frames on one carrier with less than --gap seconds of
  silence between one frame's end and the next one's start, at reduced
  power when any frame in it went out below the maximum seen;
- losses: receptions that ended in a CRC failure, frames that at least one
  station received and every one of them lost, and frames nobody was in a
  position to receive.

A reception is tied to its transmission by the instant the frame went on the
air and its payload: the ether starts a frame at the T it takes it, which is
the T its `tx` line is stamped with, and names it in `rx_begin` / `rx_end`
by a number of its own that the `tx` line does not carry.

--busy adds, per station, the share of the window the calling channel was
occupied where it stands: its own frames and every frame it was told of.
Station names come from --scenario (a scenario or run `scenario.yaml`).

--roles (needs --scenario) splits the airtime by role: transports, the
stations whose last `transport_enabled` setup line (shared, then their own)
is 1, and endpoints, every other station. For each, the mean seconds per
station on the calling channel, split into announces and path traffic
(announces, path requests and responses, SUPE ANNOUNCE), SUPE HAIL, the rest
of SUPE's frames, and unicast payload (every other frame); on one traffic
channel, a station's traffic-channel seconds divided by the number of
traffic channels any frame in the window used; and in total.
"""
import argparse
import base64
import collections
import hashlib
import json
import os
import sys

import yaml

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import seq  # noqa: E402 - the path is set just above


def kind_of(payload, part):
    """What a frame is, as one short word: the airtime classes."""
    if not payload:
        return "empty"
    if payload[0] in seq.SUPE_TYPE:
        return "SUPE " + seq.SUPE_TYPE[payload[0]]
    if len(payload) == seq.PWRREQ_LEN and payload[0] == seq.MAGIC_PWRREQ:
        return "power request"
    if part == 2:
        return None                         # the second half: its packet's class
    p = payload[1:]
    if len(p) < 19 or p[0] & 0x80:
        return "other"
    flags = p[0]
    hdr2 = bool(flags & 0x40)
    need = 2 + (32 if hdr2 else 16) + 1
    if len(p) < need:
        return "other"
    ptype = flags & 0x03
    dest = p[2 + (16 if hdr2 else 0):][:16]
    ctx = p[need - 1]
    data = p[need:]
    if ptype == 1:
        if ctx == 0x0B:
            return "path response"
        aspect = seq.NAME_HASHES.get(data[64:74]) if len(data) >= 74 else None
        return "announce " + (aspect or "other")
    if ptype == 2:
        return "link request"
    if ptype == 3:
        return "link proof" if ctx == 0xFF else "proof"
    name = seq.PLAIN_DESTS.get(dest)
    if name == "rnstransport.path.request":
        return "path request"
    if ctx in (0xFB, 0xFC, 0xFE, 0xFA):
        return "link control"
    if 0x01 <= ctx <= 0x07:
        return "resource"
    return "data"


def quartiles(values):
    v = sorted(values)
    if not v:
        return None
    q = lambda f: v[min(len(v) - 1, int(f * (len(v) - 1) + 0.5))]
    return [v[0], q(0.25), q(0.5), q(0.75), v[-1]]


def names_from(path):
    if not path:
        return {}
    if os.path.isdir(path):
        path = os.path.join(path, "scenario.yaml")
    data = yaml.safe_load(open(path))
    return {int(n["id"]): name for name, n in data["nodes"].items()}


def transports_from(path):
    """Station ids whose last `transport_enabled` setup line, shared then their own, is 1."""
    if os.path.isdir(path):
        path = os.path.join(path, "scenario.yaml")
    data = yaml.safe_load(open(path))
    shared = list(data.get("setup") or [])
    out = set()
    for node in data["nodes"].values():
        t = [l for l in shared + list(node.get("setup") or []) if "transport_enabled" in l]
        if t and t[-1].strip().endswith(" 1"):
            out.add(int(node["id"]))
    return out


def calling_class(kind):
    """The class a calling-channel frame counts under in the role split."""
    if kind.startswith("announce") or kind in ("SUPE ANNOUNCE", "path request", "path response"):
        return "announces_and_path"
    if kind == "SUPE HAIL":
        return "hails"
    if kind.startswith("SUPE "):
        return "supe_other"
    return "unicast"


def union_len(intervals):
    total, end = 0, None
    start = None
    for a, b in sorted(intervals):
        if end is None or a > end:
            if end is not None:
                total += end - start
            start, end = a, b
        elif b > end:
            end = b
    if end is not None:
        total += end - start
    return total


def analyse(args):
    lo = int(args.frm * 1e6) if args.frm is not None else None
    hi = int(args.to * 1e6) if args.to is not None else None
    calling = args.calling
    tol = 125_000 // 4

    frames = []                     # one dict per transmission in the window
    by_key = {}                     # (t_us, payload digest) -> index into frames
    begins = {}                     # (receiver, ether frame id) -> the frame's start
    carrier = {}                    # (start, length) -> carrier, for --busy
    halves = set()
    busy = collections.defaultdict(list)
    for line in open(args.record, encoding="utf-8"):
        if line.startswith("#"):
            continue
        is_tx = '"type":"tx"' in line
        if not is_tx and '"type":"rx_' not in line:
            continue
        stamp, direction, sid, blob = line.rstrip("\n").split("\t", 3)
        msg = json.loads(blob)
        if is_tx and direction == "in":
            t_us = int(round(float(stamp) * 1e6))
            payload = base64.b64decode(msg.get("payload") or "")
            part = 0
            s = int(sid)
            if payload and payload[0] & seq.RNODE_FLAG_SPLIT and payload[0] not in seq.SUPE_TYPE:
                part = 2 if s in halves else 1
                halves.symmetric_difference_update({s})
            if (lo is not None and t_us < lo) or (hi is not None and t_us >= hi):
                continue
            span = max(0, int(msg.get("t_end", 0)) - int(msg.get("t0", 0)))
            f = {"sid": s, "t": t_us, "span": span, "freq": msg.get("freq"),
                 "power": msg.get("power_dbm"), "part": part,
                 "kind": kind_of(payload, part), "rx": 0, "clean": 0, "crc": 0}
            if f["kind"] is None:
                prev = next((g for g in reversed(frames[-64:]) if g["sid"] == s), None)
                f["kind"] = prev["kind"] if prev else "data"
            by_key[(t_us, hashlib.blake2b(payload, digest_size=8).digest())] = len(frames)
            carrier[(t_us, span)] = f["freq"]
            frames.append(f)
            if args.busy and abs(f["freq"] - calling) <= tol:
                busy[s].append((t_us, t_us + span))
        elif direction == "out" and msg.get("type") == "rx_begin":
            if msg.get("cad"):
                continue
            t0 = int(msg["t0"])
            if (lo is not None and t0 < lo) or (hi is not None and t0 >= hi):
                continue
            begins[(int(sid), msg["id"])] = t0
            if args.busy:
                # rx_begin does not name the carrier; the transmission that
                # started at that instant and runs that long does.
                t_end = int(msg["t_end"])
                freq = carrier.get((t0, t_end - t0))
                if freq is not None and abs(freq - calling) <= tol:
                    busy[int(sid)].append((t0, t_end))
        elif direction == "out" and msg.get("type") == "rx_end":
            t0 = begins.pop((int(sid), msg["id"]), None)
            if t0 is None:
                continue
            payload = base64.b64decode(msg.get("payload") or "")
            i = by_key.get((t0, hashlib.blake2b(payload, digest_size=8).digest()))
            if i is None:
                continue
            f = frames[i]
            f["rx"] += 1
            if msg.get("verdict") == "clean":
                f["clean"] += 1
            else:
                f["crc"] += 1

    window = ((hi if hi is not None else max(f["t"] + f["span"] for f in frames))
              - (lo if lo is not None else min(f["t"] for f in frames))) / 1e6
    return frames, window, busy


def report(frames, window, busy, args):
    names = names_from(args.scenario)
    calling, tol = args.calling, 125_000 // 4
    on_call = lambda f: abs(f["freq"] - calling) <= tol
    out = {"window_s": window, "frames": len(frames)}
    stations = sorted(set(names) | {f["sid"] for f in frames})
    n_st = len(stations) or 1

    per = collections.defaultdict(lambda: [0, 0])     # sid -> [calling us, other us]
    for f in frames:
        per[f["sid"]][0 if on_call(f) else 1] += f["span"]
    tot_call = sum(v[0] for v in per.values()) / 1e6
    tot_other = sum(v[1] for v in per.values()) / 1e6
    each = {s: (per[s][0] + per[s][1]) / 1e6 for s in stations}
    top = max(each.items(), key=lambda x: x[1]) if each else (None, 0)
    out["airtime"] = {
        "total_s": tot_call + tot_other, "calling_s": tot_call, "traffic_channels_s": tot_other,
        "stations": n_st,
        "per_station_mean_s": (tot_call + tot_other) / n_st,
        "per_station_mean_calling_s": tot_call / n_st,
        "per_station_mean_traffic_s": tot_other / n_st,
        "per_station_max_s": top[1], "per_station_max_name": names.get(top[0], top[0]),
        "per_station_mean_fraction": (tot_call + tot_other) / n_st / window if window else None,
        "per_station_max_fraction": top[1] / window if window else None,
    }
    freqs = collections.Counter()
    for f in frames:
        freqs[round(f["freq"] / 1e5) / 10] += f["span"]
    out["by_carrier_mhz"] = {"%.1f" % k: v / 1e6 for k, v in sorted(freqs.items())}
    kinds = collections.defaultdict(lambda: [0, 0])
    for f in frames:
        kinds[f["kind"]][0] += 1
        kinds[f["kind"]][1] += f["span"]
    out["by_kind"] = {k: {"frames": v[0], "s": v[1] / 1e6}
                      for k, v in sorted(kinds.items(), key=lambda x: -x[1][1])}
    on_calling = collections.defaultdict(lambda: [0, 0])
    for f in frames:
        if on_call(f):
            on_calling[f["kind"]][0] += 1
            on_calling[f["kind"]][1] += f["span"]
    out["by_kind_calling"] = {k: {"frames": v[0], "s": v[1] / 1e6}
                              for k, v in sorted(on_calling.items(), key=lambda x: -x[1][1])}
    ann = sum(v[1] for k, v in kinds.items() if k.startswith("announce") or k == "SUPE ANNOUNCE")
    out["announce_s"] = ann / 1e6
    out["other_s"] = (sum(v[1] for v in kinds.values()) - ann) / 1e6

    def power(sel):
        fs = [f for f in frames if sel(f) and f["power"] is not None]
        if not fs:
            return None
        air = sum(f["span"] for f in fs)
        return {"frames": len(fs),
                "mean_by_frame": sum(f["power"] for f in fs) / len(fs),
                "mean_by_airtime": sum(f["power"] * f["span"] for f in fs) / air if air else None,
                "min_q1_median_q3_max": quartiles([f["power"] for f in fs]),
                "below_max_frames": sum(1 for f in fs if f["power"] < max(g["power"] for g in fs))}
    out["power_calling"] = power(on_call)
    out["power_traffic_channels"] = power(lambda f: not on_call(f))

    other = sorted((f for f in frames if not on_call(f)), key=lambda f: (f["freq"], f["t"]))
    top_power = max((f["power"] for f in frames if f["power"] is not None), default=None)
    ex, cur, last = [], None, None
    for f in other:
        if cur is None or f["freq"] != cur["freq"] or f["t"] - last > args.gap * 1e6:
            cur = {"freq": f["freq"], "frames": 0, "reduced": False, "stations": set()}
            ex.append(cur)
        cur["frames"] += 1
        cur["stations"].add(f["sid"])
        cur["reduced"] |= f["power"] is not None and f["power"] < top_power
        last = f["t"] + f["span"]
    out["exchanges"] = {"count": len(ex), "reduced_power": sum(1 for e in ex if e["reduced"]),
                        "frames_mean": sum(e["frames"] for e in ex) / len(ex) if ex else None}

    heard = [f for f in frames if f["rx"]]
    out["losses"] = {
        "receptions": sum(f["rx"] for f in frames),
        "receptions_clean": sum(f["clean"] for f in frames),
        "receptions_crc": sum(f["crc"] for f in frames),
        "frames_received_somewhere": len(heard),
        "frames_lost_at_every_receiver": sum(1 for f in heard if not f["clean"]),
        "frames_nobody_received": len(frames) - len(heard),
    }
    if getattr(args, "roles", False):
        transports = transports_from(args.scenario)
        chans = {f["freq"] for f in frames if not on_call(f)}
        classes = ("announces_and_path", "hails", "supe_other", "unicast")
        acc = collections.defaultdict(lambda: collections.Counter())
        for f in frames:
            if on_call(f):
                acc[f["sid"]][calling_class(f["kind"])] += f["span"]
            else:
                acc[f["sid"]]["traffic"] += f["span"]
        out["roles"] = {"traffic_channels": len(chans)}
        for role, members in (("transport", [s for s in stations if s in transports]),
                              ("endpoint", [s for s in stations if s not in transports])):
            n = len(members) or 1
            mean = lambda key: sum(acc[s][key] for s in members) / 1e6 / n
            calling_s = {c: mean(c) for c in classes}
            traffic_s = mean("traffic")
            out["roles"][role] = {
                "stations": len(members),
                "calling_s": sum(calling_s.values()),
                "calling_by_class_s": calling_s,
                "per_traffic_channel_s": traffic_s / len(chans) if chans else None,
                "traffic_channels_s": traffic_s,
                "total_s": sum(calling_s.values()) + traffic_s,
            }
    if busy:
        share = {names.get(s, s): union_len(v) / 1e6 / window for s, v in busy.items()}
        vals = sorted(share.values())
        out["busy_calling"] = {"min_q1_median_q3_max": quartiles(vals),
                               "mean": sum(vals) / len(vals), "top": sorted(
                                   share.items(), key=lambda x: -x[1])[:5]}
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("record")
    ap.add_argument("--scenario")
    ap.add_argument("--from", dest="frm", type=float)
    ap.add_argument("--to", type=float)
    ap.add_argument("--calling", type=int, default=869_525_000)
    ap.add_argument("--gap", type=float, default=1.0,
                    help="seconds of silence that end an exchange on a traffic channel")
    ap.add_argument("--busy", action="store_true")
    ap.add_argument("--roles", action="store_true",
                    help="airtime per transport and per endpoint (needs --scenario)")
    ap.add_argument("--json")
    args = ap.parse_args()
    if args.roles and not args.scenario:
        ap.error("--roles needs --scenario")
    frames, window, busy = analyse(args)
    out = report(frames, window, busy, args)
    text = json.dumps(out, indent=1, default=list)
    if args.json:
        with open(args.json, "w") as f:
            f.write(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
