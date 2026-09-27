#!/usr/bin/env python3
"""Simulations from a shell: start, stop, list and plan them through the
front. The `simesh` launcher's `new`, `stop`, `list` and `plan` verbs.

    simesh new [NAME] (--geodata G --nodeset N [--script S] | --snapshot S)
                      [--time max|<k>x|real] [--stagger N] [--build B]
                      [--pairwise]
    simesh stop NAME
    simesh pause NAME                    stopped, its state kept in its run
    simesh resume NAME                   a paused one, as it ended, in a new run
    simesh list [--json]
    simesh plan NAME PHASE=UNTIL ...     UNTIL in seconds of T; +N is N after now

`new` starts the front (front.py, in the background, logging to
runs/front.log) when nothing answers on the port, waits while the front
computes the loss tables (progress on stderr), and prints the new
simulation's name, control websocket, ether, network and run as JSON.
`--script` sets every station up with that script's `setup` after its
declared settings. `--build` runs every node whose device is of that build's
kind from it instead (a catalogue name, a package, a stamp, a local device
or a path such as a workspace's build.linux). `--pairwise` puts that
simulation's ether on the pairwise rule. A script's `main` drives it with
`simesh run <script> --sim <name>`. `--port` (default 8800) is the front's.
"""
import argparse
import asyncio
import json
import os
import subprocess
import sys
import time

import aiohttp

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
FRONT_START_S = 15.0


class NotFront(Exception):
    pass


async def connect(session, port):
    """A websocket to the front, and its greeting; NotFront if something else answers."""
    ws = await session.ws_connect("http://127.0.0.1:%d/ws?quiet=1" % port, max_msg_size=0)
    first = await ws.receive_json(timeout=10)
    if first.get("type") != "hello" or not first.get("front"):
        await ws.close()
        raise NotFront("port %d is a simd on its own, not the front: stop it, or give "
                       "--port the front's" % port)
    return ws


async def front(session, port, start):
    """The front's websocket, starting front.py first when nothing answers and `start`."""
    try:
        return await connect(session, port)
    except (aiohttp.ClientError, OSError):
        if not start:
            raise
    os.makedirs(os.path.join(SIM_DIR, "runs"), exist_ok=True)
    out = open(os.path.join(SIM_DIR, "runs", "front.log"), "a")
    subprocess.Popen([sys.executable, "-u", os.path.join(SIM_DIR, "front.py"),
                      "--bind", "0.0.0.0:%d" % port],
                     stdin=subprocess.DEVNULL, stdout=out, stderr=out,
                     start_new_session=True, cwd=SIM_DIR)
    print("started the front on port %d (log: runs/front.log)" % port, file=sys.stderr)
    deadline = time.monotonic() + FRONT_START_S
    while True:
        await asyncio.sleep(0.3)
        try:
            return await connect(session, port)
        except (aiohttp.ClientError, OSError):
            if time.monotonic() > deadline:
                raise


async def reply(ws, kind, progress=False):
    """The next message of one type, skipping the registry's ticks and the
    rest; with `progress`, loss-table progress is shown on stderr meanwhile."""
    while True:
        msg = await ws.receive_json(timeout=60)
        if msg.get("type") == kind and "sim" not in msg:
            if progress:
                print(file=sys.stderr)
            return msg
        if progress and msg.get("type") == "losses_progress":
            print("\rlosses %s MHz: %s/%s pairs" % (msg.get("band"), msg.get("done"),
                                                    msg.get("total")),
                  end="", file=sys.stderr, flush=True)


async def registry(ws):
    await ws.send_str(json.dumps({"type": "sims"}))
    return await reply(ws, "sims")


def t_text(us):
    """T as `hh:mm:ss`, or `Nd + hh:mm` from a day on, as the page shows it."""
    s = int(us // 1_000_000)
    d, s = divmod(s, 86400)
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return "%dd + %02d:%02d" % (d, h, m) if d else "%02d:%02d:%02d" % (h, m, s)


def clock_text(row):
    """Simulated T, real time elapsed, and how fast T runs."""
    if row.get("t") is None:
        return "—"
    t = "T " + t_text(row["t"])
    if row.get("started"):
        end = row.get("ended") or (time.time() if row.get("state") in ("running", "starting")
                                    else None)
        if end:
            t += " / %s real" % t_text((end - row["started"]) * 1e6)
    if row.get("state") in ("paused", "ended"):
        return "%s %s" % (t, row["state"])
    if row.get("mode") == "virtual":
        pace = row.get("rate") or row.get("pace")
        return "%s at %s" % (t, "%.1fx" % pace if pace else "max")
    return t + " real"


def plan_text(row):
    phase = row.get("phase")
    if phase is None:
        return "done" if row.get("done") else "—"
    minutes = "%.0f of %.0f min" % ((row["t"] - phase["from"]) / 6e7,
                                    (phase["until"] - phase["from"]) / 6e7)
    eta = time.strftime(" · done about %H:%M", time.localtime(row["eta"])) \
        if row.get("eta") else ""
    return "%s %s%s" % (phase["name"], minutes, eta)


async def main():
    ap = argparse.ArgumentParser(prog="simesh", description=__doc__.split("\n")[0])
    port = argparse.ArgumentParser(add_help=False)
    port.add_argument("--port", type=int, default=8800, help="the front's port")
    verbs = ap.add_subparsers(dest="verb", required=True)

    def verb(name, text):
        return verbs.add_parser(name, help=text, parents=[port])

    new = verb("new", "start a simulation")
    new.add_argument("name", nargs="?")
    new.add_argument("--geodata")
    new.add_argument("--nodeset")
    new.add_argument("--script")
    new.add_argument("--snapshot")
    new.add_argument("--time", default="real")
    new.add_argument("--stagger", type=float)
    new.add_argument("--build")
    new.add_argument("--pairwise", action="store_true")
    stop = verb("stop", "stop a simulation")
    stop.add_argument("name")
    pause = verb("pause", "stop a simulation with its state kept, to be resumed")
    pause.add_argument("name")
    resume = verb("resume", "start a paused simulation again as it ended")
    resume.add_argument("name")
    listing = verb("list", "the running simulations")
    listing.add_argument("--json", action="store_true")
    plan = verb("plan", "tell a simulation its phases")
    plan.add_argument("name")
    plan.add_argument("phases", nargs="*", metavar="PHASE=UNTIL")
    args = ap.parse_args()
    if args.verb == "new":
        if args.snapshot and any([args.geodata, args.nodeset, args.script]):
            ap.error("new takes --geodata and --nodeset (and --script), or --snapshot, "
                     "not both")
        if not args.snapshot and not (args.geodata and args.nodeset):
            ap.error("new needs --geodata and --nodeset, or --snapshot")

    async with aiohttp.ClientSession() as session:
        try:
            ws = await front(session, args.port, start=args.verb == "new")
        except NotFront as err:
            print(err, file=sys.stderr)
            return 2
        except (aiohttp.ClientError, OSError):
            print("nothing answers on port %d: `simesh` or `simesh new` starts the front"
                  % args.port, file=sys.stderr)
            return 2
        await ws.receive_json(timeout=10)       # the registry it greets with

        if args.verb == "new":
            msg = {"type": "sim_new", "name": args.name, "geodata": args.geodata,
                   "nodeset": args.nodeset, "script": args.script, "snapshot": args.snapshot,
                   "time": args.time, "stagger": args.stagger, "build": args.build,
                   "pairwise": args.pairwise or None}
            await ws.send_str(json.dumps({k: v for k, v in msg.items() if v is not None}))
            answer = await reply(ws, "sim_new", progress=True)
        elif args.verb in ("stop", "pause", "resume"):
            kind = "sim_" + args.verb
            await ws.send_str(json.dumps({"type": kind, "name": args.name}))
            answer = await reply(ws, kind, progress=args.verb == "resume")
        elif args.verb == "list":
            rows = (await registry(ws))["sims"]
            if args.json:
                print(json.dumps(rows, indent=1))
                return 0
            for row in rows:
                counts = row.get("counts") or {}
                state = row["state"] if row["state"] != "exited" \
                    else "exited %s" % row.get("code")
                what = ("snapshot %s" % row["snapshot"]) if row.get("snapshot") else \
                    "/".join(row.get(k) or "—" for k in ("geodata", "nodeset", "script"))
                print("%-16s %-12s %-30s %3d/%-3d up  %-40s %s" % (
                    row["name"], state, what,
                    counts.get("up", 0), row.get("stations", 0),
                    clock_text(row), plan_text(row)))
            if not rows:
                print("no simulations")
            return 0
        else:
            rows = {r["name"]: r for r in (await registry(ws))["sims"]}
            row = rows.get(args.name)
            if row is None or row.get("t") is None:
                print("no running simulation named %s" % args.name, file=sys.stderr)
                return 1
            phases = []
            for item in args.phases:
                name, _, until = item.partition("=")
                at = float(until.lstrip("+"))
                us = int(at * 1e6) + (row["t"] if until.startswith("+") else 0)
                phases.append({"name": name, "until": us})
            await ws.send_str(json.dumps({"type": "plan", "sim": args.name,
                                          "phases": phases}))
            answer = {"ok": True, "name": args.name, "phases": phases}
        await ws.close()
    print(json.dumps(answer, indent=1))
    return 0 if answer.get("ok") else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
