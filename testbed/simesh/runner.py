"""A script, run: its top to its end, against its simulation.

    simesh run SCRIPT --geodata G --nodeset N [--nodeset N2 …] [--name S] [--port P]
    simesh run SCRIPT --sim NAME [--port P]
    simesh run SCRIPT --report RUN_DIR

The front runs a script as `python3 -m simesh.runner` when the Scripts tab
says Run, on the Nodes tab's geodata and nodesets, with its output streamed
to the page; `simesh run` from a shell is the same. SCRIPT is a path, or the
name of one of the store's.

The script is plain Python run from its top (simesh.library): its
declarations (`time`, `firmware`, `on_first_boot`) are collected until the
first thing it does, which starts its simulation with them on `--geodata`
and `--nodeset` (several are merged), or attaches to `--sim` and says its
rules to that one. A script that only declares starts its simulation at its
end. The simulation runs on when the script ends, until something stops it;
a script that wants it paused says `pause()`.

Then, when the script has a `report(run_dir)` and ran to its end, that
writes the run's `report.md`, and the run's ETSI compliance section
(`compliance.py`) follows it. `--report` writes only that, for a run that
has ended. The process exits 0 when the script ran to its end and 1 when it
raised (the traceback printed).
"""

import argparse
import inspect
import os
import sys
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import compliance                   # noqa: E402 - the path is set just above
import runs as runs_module          # noqa: E402
import script as script_module      # noqa: E402
import store                        # noqa: E402
from simesh import library          # noqa: E402
from simesh import sim as sim_module  # noqa: E402


def script_file(given):
    """A path as it is, or a name of the store's scripts as that script."""
    if os.path.exists(given) or os.sep in given or given.endswith(".py"):
        return given
    return script_module.script_path(given)


def write_report(module, run_dir):
    """The script's `report(run_dir)` as the run's report.md, the run's ETSI
    compliance section after it; whether the script had a report."""
    import asyncio

    report = getattr(module, script_module.REPORT, None)
    if report is None:
        return False
    text = report(run_dir)
    if inspect.isawaitable(text):
        text = asyncio.run(text)
    text = str(text or "").rstrip("\n")
    try:
        tail = compliance.section(run_dir)
    except Exception as err:        # noqa: BLE001 - the script's report still stands
        tail = "## ETSI compliance\n\nNot checked: %s\n" % err
    path = os.path.join(run_dir, runs_module.REPORT_FILE)
    store.write_text(path, (text + "\n\n" if text else "") + tail)
    print("report: %s" % path, flush=True)
    return True


def run(args):
    path = os.path.abspath(script_file(args.script))
    if args.report:
        module = script_module.module_of(path)
        if not write_report(module, os.path.abspath(args.report)):
            print("%s has no report(run_dir)" % args.script, flush=True)
            return 2
        return 0
    name = os.path.splitext(os.path.basename(path))[0]
    in_store = os.path.dirname(path) == os.path.abspath(store.SCRIPTS_DIR)
    library.runtime.configure(
        geodata=args.geodata, nodesets=args.nodeset or None, name=args.name, build=args.build,
        port=args.port, sim=args.sim, script=name if in_store else None)
    try:
        module = script_module.run_file(path, name)
        sim = library.runtime.held()          # a script that only declared starts it now
        print("simulation %s" % sim.name, flush=True)
        run_dir = sim.run_dir
    finally:
        library.runtime.close()
    if run_dir:
        write_report(module, run_dir)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="simesh run", description=__doc__.split("\n")[0])
    ap.add_argument("script")
    ap.add_argument("--sim", help="the running simulation to run it on")
    ap.add_argument("--geodata")
    ap.add_argument("--nodeset", action="append",
                    help="a nodeset, given once or more; several are merged")
    ap.add_argument("--name", help="the new simulation's name")
    ap.add_argument("--build")
    ap.add_argument("--port", type=int, default=sim_module.DEFAULT_PORT)
    ap.add_argument("--report", metavar="RUN_DIR",
                    help="only write that run's report, with the script's report(run_dir)")
    args = ap.parse_args(argv)
    if not args.report and not args.sim and not (args.geodata and args.nodeset):
        ap.error("give --sim, or --geodata and --nodeset, or --report")
    try:
        return run(args)
    except (store.StoreError, sim_module.SimError, library.ScriptError) as err:
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
