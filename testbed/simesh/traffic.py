"""The LXMF traffic driver: a whole run, on the run's own clock, on any
firmware whose kind can say an address, an announce and a message.

    from simesh import *
    from simesh import traffic

    time("max")
    firmware("all", "reticulous_dev_latest")
    on_first_boot("all", "lxmf create {name}")
    traffic.run(OPTIONS)                    # into the run directory as traffic.json
    pause()

Phases, each skipped when its option says so:

  up        wait until every station is up.
  identity  each station's LXMF delivery address (the `address` meta
            command), asked until every station has one.
  warm      rounds of announces across the stations (the `announce` meta
            command; warm_rounds, each spread over warm_spread seconds with
            warm_gap between rounds), then, on the stations whose kind can
            count its paths (`reticulous`: `rnpath -s`), a sample every
            settle_every seconds until their total does not rise between
            two samples (or settle_max has passed). With no such station,
            two settle_every waits. warm_rounds 0 skips the phase.
  snapshot  a snapshot, warm_snapshot, when given.
  traffic   one message every `every` seconds for `traffic` seconds (the
            `message` meta command, `send_msg`), the sender, recipient,
            size class and text all drawn from `seed`, so two runs with one
            seed and one set of station names send the same messages at the
            same instants after the traffic starts. Each send is preceded
            by the `path` meta command at the sender, the route length at
            send time where the kind can say it. Size classes: short 20-55
            characters, two-frame 115-300, and 420-650 (over 500 B on the
            wire: a link and a resource), weighted 167 : 76 : 39. Every text
            starts with its marker, `marker` and a four-digit number.
  drain     `drain` seconds after the last send slot.
  gather    every `gather` line on the stations of each kind it names.
  snapshot  end_snapshot, when given.

Once every station is up the driver says the run's phases (warm-up,
traffic and drain, each with the T it ends at), and again whenever warm-up
runs longer, so the page can say which phase the run is in and when it will
be done. The result holds the phases (wall and T at each boundary), every
clock message, the warm-up samples, every send with its route and reply,
and the gathered output. How much of it was delivered is counted from the
stations' logs afterwards, which is Reticulous's for now
(simesh.reticulum.delivery, `report`).
"""

import asyncio
import json
import os
import random
import re
import time

from simesh import library

OPTIONS = {"warm_rounds": 3, "warm_spread": 300.0, "warm_gap": 120.0,
           "settle_every": 180.0, "settle_max": 3600.0, "warm_snapshot": None,
           "traffic": 3600.0, "every": 5.0, "seed": 17, "marker": "G", "drain": 600.0,
           "gather": None, "end_snapshot": None}

WORDS = ("mesh relay gateway lora packet announce proof link path hop station "
         "field city river bridge north south east west signal").split()
CLASSES = ["short"] * 167 + ["two"] * 76 + ["big"] * 39
# What each kind is asked at the end, by kind.
DEFAULT_GATHER = {"reticulous": ["rnpath -s", "lxmf unfinished", "lxmf msgs received",
                                 "lxmf msgs delivered", "lora 0", "lora 0 supe"]}
# The kinds that can count the paths they know, and how.
PATH_COUNT = {"reticulous": "rnpath -s"}
QUEUED = re.compile(r"queued (\S+)")
RESULT_FILE = "traffic.json"


def body(rng, cls, marker):
    n = {"short": rng.randint(20, 55), "two": rng.randint(115, 300)}.get(cls) or rng.randint(420, 650)
    text = marker
    while len(text) < n:
        text += "-" + rng.choice(WORDS)
    return text[:n]


def schedule(names, seed, duration, every, marker):
    """The traffic: (n, at, src, dst, cls, text), at in seconds after its start."""
    rng = random.Random(seed)
    names = sorted(names)
    out = []
    n = 0
    while n * every < duration:
        src = rng.choice(names)
        dst = rng.choice([s for s in names if s != src])
        cls = rng.choice(CLASSES)
        out.append((n + 1, n * every, src, dst, cls, body(rng, cls, "%s%04d" % (marker, n + 1))))
        n += 1
    return out


def paths_total(text):
    m = re.search(r"(\d+) paths total", text or "")
    return int(m.group(1)) if m else None


def route_hops(text):
    """The hop count out of a `path` answer (`rnpath -j`'s JSON): (hops or
    None, whether it could be read)."""
    try:
        paths = json.loads(text[text.index("{"):]).get("paths") or []
    except ValueError:
        return None, False
    return (paths[0]["hops"] if paths else None), True


class Options:
    """The phase options, `OPTIONS` with whatever a script gives over them."""

    def __init__(self, **given):
        unknown = set(given) - set(OPTIONS)
        if unknown:
            raise ValueError("no traffic option %s" % ", ".join(sorted(unknown)))
        self.__dict__.update(OPTIONS, **given)


def run(opts=None, out_path=None):
    """The whole run on the script's simulation (simesh.library): the
    result, written to `out_path` (the run directory's traffic.json by
    default) as it grows."""
    sim = library.runtime.held()
    out_path = out_path or os.path.join(sim.run_dir, RESULT_FILE)
    return library.runtime.call(run_on(sim, opts or {}, out_path))


async def run_on(sim, opts, out_path):
    """The whole run on a held `simesh.Sim`. `opts` is an Options (or a
    mapping of them); the result is written to `out_path` as it grows, and
    returned."""
    if isinstance(opts, dict):
        opts = Options(**opts)
    result = sim.result
    result.update({"args": dict(vars(opts)), "phases": [], "wall_start": time.time()})

    def phase(name):
        result["phases"].append([name, round(time.time(), 3), sim.t])
        print("%s: run %.1f s, wall %.1f s" % (name, sim.run_s, time.time() - result["wall_start"]),
              flush=True)

    def dump():
        with open(out_path, "w") as f:
            json.dump(result, f, indent=1)

    await sim.all_up()
    result["up"] = sim.up
    phase("all_up")
    await run_phases(opts, sim, result, phase, dump)
    result["wall_end"] = time.time()
    result["t_end"] = sim.t
    dump()
    print("done", flush=True)
    return result


def meta(verb, **args):
    return {"type": "meta", "verb": verb, "args": args}


async def run_phases(opts, sim, result, phase, dump):
    stations = sorted(n for n, s in sim.stations.items() if s.firmware or s.kind)
    by_kind = {}
    for name in stations:
        by_kind.setdefault(sim.stations[name].kind, []).append(name)
    # A station is up before its first-boot lines have all landed, so its
    # identity may not exist yet: ask until every station has one.
    dests = {}
    for attempt in range(60):
        got = await sim.ask(meta("address"), [n for n in stations if n not in dests],
                            after=2.0 if attempt else 0.0)
        for name, addr in got.items():
            if re.fullmatch(r"[0-9a-f]{32}", addr or ""):
                dests[name] = addr
        if len(dests) >= len(stations):
            break
    result["dests"] = dests
    # Every station, whether or not it has shown its identity: the schedule
    # is drawn over them, so one seed sends the same messages.
    senders = stations
    missing = sorted(set(senders) - set(dests))
    if missing:
        print("no lxmf destination from: %s" % " ".join(missing), flush=True)
    phase("identities")

    async def send_plan(warm_end):
        """The phases left, from where warm-up is expected to end: sent at
        the start and again whenever that moves."""
        phases = [("warm-up", warm_end)] if opts.warm_rounds > 0 else []
        if opts.traffic > 0:
            phases += [("traffic", warm_end + 1.0 + opts.traffic),
                       ("drain", warm_end + 1.0 + opts.traffic + opts.drain)]
        if phases:
            await sim.plan(*phases)

    rounds = opts.warm_rounds
    await send_plan(sim.run_s + rounds * opts.warm_spread
                    + max(0, rounds - 1) * opts.warm_gap + 2 * opts.settle_every
                    if rounds > 0 else sim.run_s)

    if opts.warm_rounds > 0:
        result["warm"] = {"rounds": [], "samples": []}
        for r in range(opts.warm_rounds):
            await sim.ask(meta("announce"), senders, spread=opts.warm_spread,
                          after=opts.warm_gap if r else 0.0)
            result["warm"]["rounds"].append([time.time(), sim.t])
            phase("warm_round_%d" % (r + 1))
        counted = [(kind, names) for kind, names in by_kind.items() if kind in PATH_COUNT]
        settle_from = sim.run_s
        prev = None
        while True:
            if not counted:
                await sim.sleep(2 * opts.settle_every)
                break
            per = {}
            for kind, names in counted:
                reply = await sim.ask({"type": "command", "line": PATH_COUNT[kind]}, names,
                                      after=opts.settle_every)
                per.update({n: paths_total(t) for n, t in reply.items()})
            total = sum(v for v in per.values() if v)
            result["warm"]["samples"].append({"t": sim.t, "wall": time.time(),
                                              "total": total, "per": per})
            print("  paths %d at run %.0f s" % (total, sim.run_s), flush=True)
            dump()
            if prev is not None and total <= prev:
                break
            if sim.run_s - settle_from >= opts.settle_max:
                print("  settle-max reached, still rising", flush=True)
                break
            prev = total
            await send_plan(sim.run_s + opts.settle_every)   # one more sample at least
        phase("warm_settled")
    if opts.warm_snapshot:
        await sim.snapshot(opts.warm_snapshot)
        phase("warm_snapshot")

    if opts.traffic > 0:
        plan = schedule(senders, opts.seed, opts.traffic, opts.every, opts.marker)
        start = sim.run_s + 1.0
        result["traffic_start_run_s"] = start
        await send_plan(start - 1.0)
        sends = result["sends"] = []

        async def one(n, at, src, dst, cls, text):
            rec = {"n": n, "marker": "%s%04d" % (opts.marker, n), "src": src,
                   "dst": dst, "cls": cls, "len": len(text), "at": at}
            sends.append(rec)
            if dst not in dests:
                rec["error"] = "recipient has no destination"
                return
            # The route and the send as one sequence, so both happen at the
            # T the message is due, with no round trip to here between them.
            # The `after` (never less than a millisecond) makes it a task of
            # simd's own: a message without one is handled inside the
            # socket's reader, which holds every later message behind it.
            results, t = await sim.sequence(
                [meta("path", to=dst), meta("message", to=dst, text=text)], [src],
                after=max(0.001, start + at - sim.run_s))
            path_out, out = (r.get(src, "") for r in results)
            rec["t_sent"] = rec["t_path"] = t if t is not None else sim.t
            rec["hops"], parsed = route_hops(path_out)
            if not parsed:
                rec["path_reply"] = path_out[:200]
            m = QUEUED.search(out)
            rec["mid"] = m.group(1) if m else None
            if not m:
                rec["reply"] = out[:300]

        phase("traffic_start")
        jobs = [asyncio.ensure_future(one(*p)) for p in plan]
        print("scheduled %d messages over %.0f s" % (len(plan), opts.traffic), flush=True)
        t_end = start + opts.traffic
        while sim.run_s < t_end:
            await asyncio.sleep(5)
            dump()
        await asyncio.gather(*jobs)
        phase("traffic_end")
        await sim.until(t_end + opts.drain)
        phase("drain_end")

    gathered = result["gathered"] = {}
    for kind, lines in (opts.gather or DEFAULT_GATHER).items():
        for line in lines:
            names = by_kind.get(kind) or []
            if names:
                reply = await sim.ask({"type": "command", "line": line}, names)
                gathered["%s: %s" % (kind, line)] = {"t": sim.t, "results": reply}
    phase("gathered")
    if opts.end_snapshot:
        await sim.snapshot(opts.end_snapshot)
        phase("end_snapshot")


# ---- the report ----------------------------------------------------------

def t_text(us):
    """T as `hh:mm:ss`, or `Nd + hh:mm` from a day on."""
    s = int((us or 0) // 1_000_000)
    d, s = divmod(s, 86400)
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return "%dd + %02d:%02d" % (d, h, m) if d else "%02d:%02d:%02d" % (h, m, s)


def report(run_dir, traffic_path=None):
    """A traffic run's report, as Markdown: what ran, the phases, and the
    delivery counted from the run's own logs (`delivery.analyse_run`)."""
    from simesh.reticulum import delivery

    traffic_path = traffic_path or os.path.join(run_dir, RESULT_FILE)
    name = os.path.basename(os.path.normpath(run_dir))
    lines = ["# LXMF traffic: %s" % name, ""]
    if not os.path.isfile(traffic_path):
        return "\n".join(lines + ["No %s: the driver did not get as far as writing it."
                                  % os.path.basename(traffic_path), ""])
    try:
        drive, out, _ = delivery.analyse_run(traffic_path, run_dir)
    except ValueError as err:
        with open(traffic_path, encoding="utf-8") as handle:
            drive, out = json.load(handle), None
        problem = str(err)
    dests = drive.get("dests") or {}
    stations = sorted(set(drive.get("up") or {}) | set(dests))
    args = drive.get("args") or {}
    lines += ["| | |", "|---|---|",
              "| ended at | T %s |" % t_text(drive.get("t_end")),
              "| wall | %.0f s |" % ((drive.get("wall_end") or 0) - (drive.get("wall_start") or 0)),
              "| sends | one every %s s for %s s, seed %s |"
              % (args.get("every"), args.get("traffic"), args.get("seed")),
              "| LXMF identities | %d%s |" % (len(dests), " of %d stations" % len(stations)
                                             if stations else ""),
              ""]
    if not dests:
        lines += ["**No station showed an LXMF identity**, so nothing could be sent: "
                  "the stations' first-boot lines create them (`lxmf create {name}`).", ""]
    lines += ["## Phases", "", "| phase | T |", "|---|---|"]
    lines += ["| %s | %s |" % (p[0], t_text(p[2])) for p in drive.get("phases") or []]
    lines.append("")
    if out is None:
        lines += ["## Delivery", "", "Not counted: %s." % problem, ""]
        return "\n".join(lines)

    def table(title, figures):
        rows = ["| %s | delivered |" % title, "|---|---|"]
        return rows + ["| %s | %s |" % (k, v) for k, v in figures.items()] + [""]

    lines += ["## Delivery", "", "**%s** delivered." % out["overall"], ""]
    lines += table("route hops at send", out["by_route_hops"])
    lines += table("radio hops", out["by_radio_hops"])
    lines += table("size", out["by_class"])
    lat = out.get("latency_s")
    if lat:
        lines += ["Latency: median %.1f s, 90%% within %.1f s, longest %.1f s (%d messages)."
                  % (lat["median"], lat["p90"], lat["max"], lat["n"]), ""]
    if out.get("undelivered_last_word"):
        lines += ["## Undelivered, by the sender's last word", "", "| last line | messages |",
                  "|---|---|"]
        lines += ["| `%s` | %d |" % (w.replace("|", "\\|"), n)
                  for w, n in out["undelivered_last_word"][:10]]
        lines.append("")
    return "\n".join(lines)
