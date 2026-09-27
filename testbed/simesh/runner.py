"""A script's `main`, run on a simulation.

    simesh run SCRIPT [--sim NAME [--pause]] [--port P]
    simesh run SCRIPT --geodata G --nodeset N [--time T] [--name S] [--port P]
    simesh run SCRIPT --report RUN_DIR

The front runs a script as `python3 -m simesh.runner` when the Scripts tab
says Run, with the script's output streamed to the page; `simesh run` from a
shell is the same. SCRIPT is a path, or the name of one of the store's. With
`--sim` it attaches to that running simulation; with `--geodata` and
`--nodeset` it asks the front for a new one, with the script's own `setup`
as its setup when the script is one of the store's (`testbed/scripts/`),
and attaches to that. Either way `main(sim)` is awaited, and
the process exits 0 when it returns, 1 when it raises (the traceback
printed) and 2 when the script has no `main`.

When main ends, a simulation this run started is paused (the front stops
its stations and keeps their state, to be resumed); `--pause` does the same
to a `--sim` one, which is how the front runs a script on a simulation it
started for it. Then, when main returned, the script's `report(run_dir)`
writes the run's `report.md`. `--report` writes only that, for a run that
has ended.

The script's directory is its working directory's business; what it writes
is its own choice. `sim.name` is the simulation, and `sim.run_dir` its run
directory, which is the natural place.
"""

import argparse
import asyncio
import inspect
import os
import sys
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import runs as runs_module          # noqa: E402 - the path is set just above
import script as script_module      # noqa: E402
import store                        # noqa: E402
from simesh import sim as sim_module  # noqa: E402


def script_file(given):
    """A path as it is, or a name of the store's scripts as that script."""
    if os.path.exists(given) or os.sep in given or given.endswith(".py"):
        return given
    return script_module.script_path(given)


async def write_report(module, run_dir):
    """The script's `report(run_dir)` as the run's report.md; whether it had one."""
    report = getattr(module, script_module.REPORT, None)
    if report is None:
        return False
    text = report(run_dir)
    if inspect.isawaitable(text):
        text = await text
    path = os.path.join(run_dir, runs_module.REPORT_FILE)
    store.write_text(path, str(text or ""))
    print("report: %s" % path, flush=True)
    return True


async def run(args):
    args.script = script_file(args.script)
    module = script_module.load(os.path.abspath(args.script))
    if args.report:
        if not await write_report(module, os.path.abspath(args.report)):
            print("%s has no report(run_dir)" % args.script, flush=True)
            return 2
        return 0
    main = getattr(module, script_module.MAIN, None)
    if main is None:
        print("%s has no main(sim): nothing to run" % args.script, flush=True)
        return 2
    if args.sim:
        sim = await sim_module.attach(args.sim, args.port)
    else:
        # A simulation keeps a script of the store's by name, so only one of
        # those can be its setup; a file from elsewhere drives it and no more.
        path = os.path.abspath(args.script)
        name = os.path.splitext(os.path.basename(path))[0]
        in_store = os.path.dirname(path) == os.path.abspath(store.SCRIPTS_DIR)
        setup = getattr(module, script_module.SETUP, None)
        if setup and not in_store:
            print("%s is not in %s: its setup is not the new simulation's" % (args.script,
                                                                            store.SCRIPTS_DIR),
                  flush=True)
        sim = await sim_module.start(args.geodata, args.nodeset,
                                     name if setup and in_store else None,
                                     args.time, args.name, args.build, args.port)
    print("attached to %s" % sim.name, flush=True)
    run_dir = None
    try:
        await main(sim)
        run_dir = sim.run_dir
    finally:
        await sim.close()
        # One this run started is its to pause; one it attached to runs on.
        if not args.sim or args.pause:
            try:
                await sim.pause()
                print("paused %s" % sim.name, flush=True)
            except sim_module.SimError as err:
                print("! %s not paused: %s" % (sim.name, err), flush=True)
        await sim.session.close()
    if run_dir:
        await write_report(module, run_dir)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="simesh run", description=__doc__.split("\n")[0])
    ap.add_argument("script")
    ap.add_argument("--sim", help="the running simulation to attach to")
    ap.add_argument("--geodata")
    ap.add_argument("--nodeset")
    ap.add_argument("--time", default="real")
    ap.add_argument("--name", help="the new simulation's name")
    ap.add_argument("--build")
    ap.add_argument("--port", type=int, default=sim_module.DEFAULT_PORT)
    ap.add_argument("--pause", action="store_true",
                    help="pause the --sim simulation when main ends, as one started here is")
    ap.add_argument("--report", metavar="RUN_DIR",
                    help="only write that run's report, with the script's report(run_dir)")
    args = ap.parse_args(argv)
    if not args.report and not args.sim and not (args.geodata and args.nodeset):
        ap.error("give --sim, or --geodata and --nodeset, or --report")
    try:
        return asyncio.run(run(args))
    except (store.StoreError, sim_module.SimError) as err:
        print("! %s" % err, flush=True)
        return 1
    except KeyboardInterrupt:
        return 130
    except Exception:                       # noqa: BLE001 - the script's own, shown whole
        traceback.print_exc()
        sys.stderr.flush()
        return 1


if __name__ == "__main__":
    sys.exit(main())
