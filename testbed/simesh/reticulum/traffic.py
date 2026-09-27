"""The LXMF traffic driver: a whole run on one simulation, on the run's own clock.

    async def main(sim):                    # a script's main (scripts/lxmf-traffic.py)
        await traffic.run_on(sim, OPTIONS, out_path)

Phases, each skipped when its option says so:

  up        wait until every station is up.
  warm      rounds of `lora 0 a` across the stations (warm_rounds, each spread
            over warm_spread seconds with warm_gap between rounds), then
            `rnpath -s` on every station every settle_every seconds until
            their total of paths does not rise between two samples (or
            settle_max has passed). warm_rounds 0 skips the phase.
  snapshot  `snapshot_save_as` warm_snapshot, when given.
  traffic   one `lxmf send` every `every` seconds for `traffic` seconds, the
            sender, recipient, size class and text all drawn from `seed`, so
            two runs with one seed and one set of station names send the same
            messages at the same instants after the traffic starts. Each send
            is preceded by `rnpath -j <recipient>` at the sender, which is the
            route length at send time. Size classes: short 20-55
            characters, two-frame 115-300, and 420-650 (over 500 B on the
            wire: a link and a resource), weighted 167 : 76 : 39. Every text
            starts with its marker, `marker` and a four-digit number.
  drain     `drain` seconds after the last send slot.
  gather    every `gather` line on every station, in turn.
  snapshot  `snapshot_save_as` end_snapshot, when given.

Once every station is up the driver sends simd its plan (warm-up, traffic
and drain, each with the T it ends at), and again whenever warm-up runs
longer, so the page can say which phase the run is in and when it will be
done. Every instant is on simd's run clock (`after` on the `command`
message), so a real-time and a virtual-time run act at the same instants of
the run. The result holds the phases (wall and T at each boundary), every
clock message, the warm-up samples, every send with its route and reply,
and the gathered output. Commands go only to the stations of one kind
(`kind`, `reticulous` by default), since a CLI line is that kind's language;
those stations are the senders and recipients. `OPTIONS` are the defaults.
"""

import asyncio
import json
import random
import re
import time

KIND = "reticulous"
OPTIONS = {"kind": KIND, "warm_rounds": 3, "warm_spread": 300.0, "warm_gap": 120.0,
           "settle_every": 180.0, "settle_max": 3600.0, "warm_snapshot": None,
           "traffic": 3600.0, "every": 5.0, "seed": 17, "marker": "G", "drain": 600.0,
           "gather": None, "end_snapshot": None}

WORDS = ("mesh relay gateway lora packet announce proof link path hop station "
         "field city river bridge north south east west signal").split()
CLASSES = ["short"] * 167 + ["two"] * 76 + ["big"] * 39
DEFAULT_GATHER = ["rnpath -s", "lxmf unfinished", "lxmf msgs received",
                  "lxmf msgs delivered", "lora 0", "lora 0 supe"]

DEST = re.compile(r"\*\s*\d+\s+\S+\s+([0-9a-f]{32})")
QUEUED = re.compile(r"queued (\S+)")


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
    """The hop count out of an `rnpath -j` answer: (hops or None, parsed)."""
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


async def run_on(sim, opts, out_path):
    """The whole run on a `simesh.Sim`. `opts` is an Options (or a mapping of
    them); the result is written to `out_path` as it grows, and returned."""
    if isinstance(opts, dict):
        opts = Options(**opts)
    result = sim.result
    result.update({"args": dict(vars(opts)), "phases": [], "wall_start": time.time()})

    def phase(name, sim):
        result["phases"].append([name, round(time.time(), 3), sim.t])
        print("%s: run %.1f s, wall %.1f s" % (name, sim.run_s, time.time() - result["wall_start"]),
              flush=True)

    def dump():
        with open(out_path, "w") as f:
            json.dump(result, f, indent=1)

    await sim.all_up()
    result["up"] = sim.up
    phase("all_up", sim)
    await run_phases(opts, sim, result, phase, dump)
    result["wall_end"] = time.time()
    result["t_end"] = sim.t
    dump()
    print("done", flush=True)
    return result


async def run_phases(opts, sim, result, phase, dump):
    # A station is up before its setup lines have all landed, so its
    # identity may not exist yet: ask until every station has one.
    dests = {}
    for attempt in range(60):
        listing = await sim.command("lxmf", after=2.0 if attempt else 0.0, kind=opts.kind)
        for name, out in listing["results"].items():
            m = DEST.search(out)
            if m:
                dests[name] = m.group(1)
        if len(dests) >= len(listing["results"]):
            break
    result["dests"] = dests
    # The Reticulous stations, whether or not each has shown its identity:
    # the schedule is drawn over them, so one seed sends the same messages.
    senders = sorted(listing["results"])
    missing = sorted(set(senders) - set(dests))
    if missing:
        print("no lxmf destination from: %s" % " ".join(missing), flush=True)
    phase("identities", sim)

    async def send_plan(warm_end):
        """The phases left, from where warm-up is expected to end:
        sent at the start and again whenever that moves."""
        phases = [("warm-up", warm_end)] if opts.warm_rounds > 0 else []
        if opts.traffic > 0:
            phases += [("traffic", warm_end + 1.0 + opts.traffic),
                       ("drain", warm_end + 1.0 + opts.traffic + opts.drain)]
        if phases:
            await sim.plan(*phases)

    # Warm-up at its shortest: every round, then two path samples.
    rounds = opts.warm_rounds
    await send_plan(sim.run_s + rounds * opts.warm_spread
                    + max(0, rounds - 1) * opts.warm_gap + 2 * opts.settle_every
                    if rounds > 0 else sim.run_s)

    if opts.warm_rounds > 0:
        result["warm"] = {"rounds": [], "samples": []}
        for r in range(opts.warm_rounds):
            reply = await sim.command("lora 0 a", after=opts.warm_gap if r else 0.0,
                                      stagger=opts.warm_spread, kind=opts.kind)
            result["warm"]["rounds"].append([time.time(), reply.get("t")])
            phase("warm_round_%d" % (r + 1), sim)
        settle_from = sim.run_s
        prev = None
        while True:
            reply = await sim.command("rnpath -s", after=opts.settle_every, kind=opts.kind)
            per = {n: paths_total(t) for n, t in reply["results"].items()}
            total = sum(v for v in per.values() if v)
            result["warm"]["samples"].append({"t": reply.get("t"), "wall": time.time(),
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
        phase("warm_settled", sim)
    if opts.warm_snapshot:
        await sim.save_snapshot(opts.warm_snapshot)
        await sim.command("show s.net.hostname", name=senders[0], kind=opts.kind)
        phase("warm_snapshot", sim)

    if opts.traffic > 0:
        plan = schedule(senders, opts.seed, opts.traffic, opts.every, opts.marker)
        start = sim.run_s + 1.0
        result["traffic_start_run_s"] = start
        await send_plan(start - 1.0)
        sends = result["sends"] = []

        async def one(n, at, src_name, dst, cls, text):
            rec = {"n": n, "marker": "%s%04d" % (opts.marker, n), "src": src_name,
                   "dst": dst, "cls": cls, "len": len(text), "at": at}
            sends.append(rec)
            peer = dests.get(dst)
            if not peer:
                rec["error"] = "recipient has no destination"
                return
            r = await sim.command("rnpath -j %s" % peer, name=src_name,
                                  after=max(0.0, start + at - sim.run_s), kind=opts.kind)
            out = r["results"].get(src_name, "")
            rec["t_path"] = r.get("t")
            rec["hops"], parsed = route_hops(out)
            if not parsed:
                rec["path_reply"] = out[:200]
            # A millisecond of `after` makes the send a task of simd's
            # own: a command without one is run inside the socket's
            # reader, which holds every later message behind it.
            r = await sim.command("lxmf send %s %s" % (peer, text), name=src_name,
                                  after=0.001, kind=opts.kind)
            out = r["results"].get(src_name, "")
            rec["t_sent"] = r.get("t")
            m = QUEUED.search(out)
            rec["mid"] = m.group(1) if m else None
            if not m:
                rec["reply"] = out[:300]

        phase("traffic_start", sim)
        jobs = [asyncio.ensure_future(one(*p)) for p in plan]
        print("scheduled %d messages over %.0f s" % (len(plan), opts.traffic), flush=True)
        t_end = start + opts.traffic
        while sim.run_s < t_end:
            await asyncio.sleep(5)
            dump()
        await asyncio.gather(*jobs)
        phase("traffic_end", sim)
        await sim.wait_run(t_end + opts.drain)
        phase("drain_end", sim)

    gathered = result["gathered"] = {}
    for line in opts.gather or DEFAULT_GATHER:
        r = await sim.command(line, kind=opts.kind)
        gathered[line] = {"t": r.get("t"), "results": r["results"]}
    phase("gathered", sim)
    if opts.end_snapshot:
        await sim.save_snapshot(opts.end_snapshot)
        await sim.command("show s.net.hostname", name=senders[0], kind=opts.kind)
        phase("end_snapshot", sim)


# ---- the report ----------------------------------------------------------

def t_text(us):
    """T as `hh:mm:ss`, or `Nd + hh:mm` from a day on."""
    s = int((us or 0) // 1_000_000)
    d, s = divmod(s, 86400)
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return "%dd + %02d:%02d" % (d, h, m) if d else "%02d:%02d:%02d" % (h, m, s)


HELD = "Reticulum held until"
RELEASED = "starting Reticulum"


def held_stations(run_dir):
    """The stations whose Reticulum was still waiting for a device password
    when the run ended. Every station with no password yet says it is held
    as it boots, and setup sets one moments later, which starts it: only a
    held line with no start after it counts."""
    from simesh.reticulum import delivery

    held = []
    for name, path in delivery.station_logs(run_dir):
        waiting = False
        with open(path, errors="replace") as handle:
            for line in handle:
                if HELD in line:
                    waiting = True
                elif RELEASED in line:
                    waiting = False
        if waiting:
            held.append(name)
    return held


def report(run_dir, traffic_path):
    """A traffic run's report, as Markdown: what ran, the phases, and the
    delivery counted from the run's own logs (`delivery.analyse_run`)."""
    import os

    from simesh.reticulum import delivery

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
    held = held_stations(run_dir)
    if held:
        lines += ["**Reticulum stayed held on %s** for want of a device password, so "
                  "their radios never carried Reticulum: a setup script sets one "
                  "(`auth passwd admin <pw>`)." % ", ".join(held), ""]
    if not dests:
        lines += ["**No station showed an LXMF identity**, so nothing could be sent: "
                  "the simulation needs a setup script that creates them "
                  "(`lxmf create {name}`).", ""]
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
