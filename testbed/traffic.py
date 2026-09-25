#!/usr/bin/env python3
"""A whole LXMF traffic run against one simd, on the run's own clock.

    traffic.py PORT OUT.json (--scenario NAME | --snapshot NAME) [options]

Phases, each skipped when its option says so:

  load      `scenario_load` or `snapshot_load`, then wait until every station
            is up.
  warm      rounds of `lora 0 a` across the fleet (--warm-rounds, each spread
            over --warm-spread seconds with --warm-gap between rounds), then
            `rnpath -s` on every station every --settle-every seconds until
            the fleet's total of paths does not rise between two samples
            (or --settle-max has passed). --warm-rounds 0 skips the phase.
  snapshot  `snapshot_save_as` --warm-snapshot, when given.
  traffic   one `lxmf send` every --every seconds for --traffic seconds, the
            sender, recipient, size class and text all drawn from --seed, so
            two runs with one seed and one set of station names send the same
            messages at the same instants after the traffic starts. Each send
            is preceded by `rnpath -j <recipient>` at the sender, which is the
            route length at send time. Size classes: short 20-55
            characters, two-frame 115-300, and 420-650 (over 500 B on the
            wire: a link and a resource), weighted 167 : 76 : 39. Every text
            starts with its marker, --marker and a four-digit number.
  drain     --drain seconds after the last send slot.
  gather    every --gather line on every station, in turn.
  snapshot  `snapshot_save_as` --end-snapshot, when given.

Every instant is on simd's run clock (`after` on the `command` message), so
a real-time and a virtual-time run act at the same instants of the run.
OUT.json holds the phases (wall and T at each boundary), every clock message,
the warm-up samples, every send with its route and reply, and the gathered
output. Commands go over each station's framed RPC.
"""
import argparse
import asyncio
import collections
import json
import random
import re
import time

import aiohttp

WORDS = ("mesh relay gateway lora packet announce proof link path hop station "
         "field city river bridge north south east west signal").split()
CLASSES = ["short"] * 167 + ["two"] * 76 + ["big"] * 39
DEFAULT_GATHER = ["rnpath -s", "lxmf unfinished", "lxmf msgs received",
                  "lxmf msgs delivered", "lora 0", "lora 0 supe"]


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


class Simd:
    """One websocket to simd: commands answered by line and station, the clock, the nodes."""

    def __init__(self, ws, result):
        self.ws = ws
        self.result = result
        self.t = 0
        self.t_zero = None
        self.names = None
        self.up = {}
        self.waiters = collections.defaultdict(collections.deque)
        self.saved = {}

    async def reader(self):
        async for msg in self.ws:
            if msg.type != aiohttp.WSMsgType.TEXT:
                continue
            m = json.loads(msg.data)
            k = m.get("type")
            if k == "snapshot" and m.get("scenario"):
                self.names = [n["name"] for n in m["nodes"]]
                if m.get("clock"):
                    self.t = m["clock"]["t"]
                    if self.t_zero is None:
                        self.t_zero = self.t
                        self.result["t_load"] = self.t
                        self.result["wall_load"] = time.time()
            elif k == "clock":
                self.t = m["t"]
                self.result["clock"].append([round(time.time(), 3), m["t"], m.get("barriers"),
                                             m.get("observed"), m.get("slow_idles")])
            elif k == "node" and self.t_zero is not None:
                if m.get("status") == "up":
                    self.up.setdefault(m["name"], [time.time(), self.t])
            elif k == "command_result":
                q = self.waiters.get((m["line"], m.get("name")))
                if q:
                    f = q.popleft()
                    if not f.done():
                        f.set_result(m)
            elif k == "scenario" and m.get("scenario"):
                pass
            elif k == "error":
                print("simd error: %s" % m.get("text"), flush=True)
                self.result.setdefault("errors", []).append([time.time(), self.t, m.get("text")])

    def run_s(self):
        return (self.t - self.t_zero) / 1e6

    async def command(self, line, name=None, after=0.0, stagger=0.0, kind="reticulous"):
        f = asyncio.get_running_loop().create_future()
        self.waiters[(line, name)].append(f)
        msg = {"type": "command", "line": line, "kind": kind}
        if name:
            msg["name"] = name
        if after > 0:
            msg["after"] = after
        if stagger > 0:
            msg["stagger"] = stagger
        await self.ws.send_str(json.dumps(msg))
        return await f

    async def wait_run(self, at):
        """Until the run's clock reaches `at` seconds after the load."""
        while self.run_s() < at:
            await asyncio.sleep(0.2)


async def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("port", type=int)
    ap.add_argument("out")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--scenario")
    src.add_argument("--snapshot")
    ap.add_argument("--warm-rounds", type=int, default=3)
    ap.add_argument("--warm-spread", type=float, default=300.0)
    ap.add_argument("--warm-gap", type=float, default=120.0)
    ap.add_argument("--settle-every", type=float, default=180.0)
    ap.add_argument("--settle-max", type=float, default=3600.0)
    ap.add_argument("--warm-snapshot")
    ap.add_argument("--traffic", type=float, default=3600.0)
    ap.add_argument("--every", type=float, default=5.0)
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--marker", default="G")
    ap.add_argument("--drain", type=float, default=600.0)
    ap.add_argument("--gather", action="append")
    ap.add_argument("--end-snapshot")
    args = ap.parse_args()
    result = {"args": vars(args), "clock": [], "phases": [], "wall_start": time.time()}

    def phase(name, sim):
        result["phases"].append([name, round(time.time(), 3), sim.t])
        print("%s: run %.1f s, wall %.1f s" % (name, sim.run_s(), time.time() - result["wall_start"]),
              flush=True)

    def dump():
        with open(args.out, "w") as f:
            json.dump(result, f, indent=1)

    async with aiohttp.ClientSession() as session:
        async with session.ws_connect("http://127.0.0.1:%d/ws" % args.port, max_msg_size=0) as ws:
            await ws.receive()
            sim = Simd(ws, result)
            task = asyncio.ensure_future(sim.reader())
            if args.scenario:
                await ws.send_str(json.dumps({"type": "scenario_load", "name": args.scenario}))
            else:
                await ws.send_str(json.dumps({"type": "snapshot_load", "name": args.snapshot}))
            while sim.names is None or sim.t_zero is None or len(sim.up) < len(sim.names):
                await asyncio.sleep(0.2)
            result["up"] = sim.up
            phase("all_up", sim)

            # A station is up before its setup lines have all landed, so its
            # identity may not exist yet: ask until every station has one.
            dests = {}
            for attempt in range(60):
                listing = await sim.command("lxmf", after=2.0 if attempt else 0.0)
                for name, out in listing["results"].items():
                    m = re.search(r"\*\s*\d+\s+\S+\s+([0-9a-f]{32})", out)
                    if m:
                        dests[name] = m.group(1)
                if len(dests) >= len(sim.names):
                    break
            result["dests"] = dests
            missing = sorted(set(sim.names) - set(dests))
            if missing:
                print("no lxmf destination from: %s" % " ".join(missing), flush=True)
            phase("identities", sim)

            if args.warm_rounds > 0:
                result["warm"] = {"rounds": [], "samples": []}
                for r in range(args.warm_rounds):
                    reply = await sim.command("lora 0 a", after=args.warm_gap if r else 0.0,
                                              stagger=args.warm_spread)
                    result["warm"]["rounds"].append([time.time(), reply.get("t")])
                    phase("warm_round_%d" % (r + 1), sim)
                settle_from = sim.run_s()
                prev = None
                while True:
                    reply = await sim.command("rnpath -s", after=args.settle_every)
                    per = {n: paths_total(t) for n, t in reply["results"].items()}
                    total = sum(v for v in per.values() if v)
                    result["warm"]["samples"].append({"t": reply.get("t"), "wall": time.time(),
                                                      "total": total, "per": per})
                    print("  paths %d at run %.0f s" % (total, sim.run_s()), flush=True)
                    dump()
                    if prev is not None and total <= prev:
                        break
                    if sim.run_s() - settle_from >= args.settle_max:
                        print("  settle-max reached, still rising", flush=True)
                        break
                    prev = total
                phase("warm_settled", sim)
            if args.warm_snapshot:
                await ws.send_str(json.dumps({"type": "snapshot_save_as", "name": args.warm_snapshot}))
                await sim.command("show s.net.hostname", name=sim.names[0])
                phase("warm_snapshot", sim)

            if args.traffic > 0:
                plan = schedule(sim.names, args.seed, args.traffic, args.every, args.marker)
                start = sim.run_s() + 1.0
                result["traffic_start_run_s"] = start
                sends = result["sends"] = []

                async def one(n, at, src_name, dst, cls, text):
                    rec = {"n": n, "marker": "%s%04d" % (args.marker, n), "src": src_name,
                           "dst": dst, "cls": cls, "len": len(text), "at": at}
                    sends.append(rec)
                    peer = dests.get(dst)
                    if not peer:
                        rec["error"] = "recipient has no destination"
                        return
                    r = await sim.command("rnpath -j %s" % peer, name=src_name,
                                          after=max(0.0, start + at - sim.run_s()))
                    out = r["results"].get(src_name, "")
                    rec["t_path"] = r.get("t")
                    try:
                        paths = json.loads(out[out.index("{"):]).get("paths") or []
                        rec["hops"] = paths[0]["hops"] if paths else None
                    except ValueError:
                        rec["hops"] = None
                        rec["path_reply"] = out[:200]
                    # A millisecond of `after` makes the send a task of simd's
                    # own: a command without one is run inside the socket's
                    # reader, which holds every later message behind it.
                    r = await sim.command("lxmf send %s %s" % (peer, text), name=src_name,
                                          after=0.001)
                    out = r["results"].get(src_name, "")
                    rec["t_sent"] = r.get("t")
                    m = re.search(r"queued (\S+)", out)
                    rec["mid"] = m.group(1) if m else None
                    if not m:
                        rec["reply"] = out[:300]

                phase("traffic_start", sim)
                jobs = [asyncio.ensure_future(one(*p)) for p in plan]
                print("scheduled %d messages over %.0f s" % (len(plan), args.traffic), flush=True)
                t_end = start + args.traffic
                while sim.run_s() < t_end:
                    await asyncio.sleep(5)
                    dump()
                await asyncio.gather(*jobs)
                phase("traffic_end", sim)
                await sim.wait_run(t_end + args.drain)
                phase("drain_end", sim)

            gathered = result["gathered"] = {}
            for line in args.gather or DEFAULT_GATHER:
                r = await sim.command(line)
                gathered[line] = {"t": r.get("t"), "results": r["results"]}
            phase("gathered", sim)
            if args.end_snapshot:
                await ws.send_str(json.dumps({"type": "snapshot_save_as", "name": args.end_snapshot}))
                await sim.command("show s.net.hostname", name=sim.names[0])
                phase("end_snapshot", sim)
            result["wall_end"] = time.time()
            result["t_end"] = sim.t
            task.cancel()
    dump()
    print("done", flush=True)


if __name__ == "__main__":
    asyncio.run(main())
