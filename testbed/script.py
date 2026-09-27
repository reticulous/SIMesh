"""Scripts: Python against the simesh library.

    testbed/scripts/<name>.py

    '''What this script is for, in its first line.'''
    import simesh

    async def setup(node):          # per node, on an empty store
        if "tcp-peer" in node.tags:
            await node.run("tcp peer add {addr:internet}:4965")

    async def main(sim):            # the driver
        await sim.all_up()
        await sim.nodes(tag="lora").announce(spread=300)

A script has either or both of two entry points, and may have a report:

- **`setup(node)`** sets a station up: it runs inside the
  simulation's simd, once per station, when a station boots with no state (a
  node just placed, every station of a new simulation, a station after a
  factory reset), after the node's declared settings (its name, role and
  radio figures, in its kind's language) and before its radio is started,
  so a setting the radio reads when it starts takes. `node` is a
  `simesh.setup.Node`: its name,
  id, tags, device, role and radio, `run(line)` in its kind's own language
  with the macros filled in, and the intents (`set_name`, `set_role`,
  `set_radio`, `announce`, `peer_tcp`). It runs in simd's own loop, so it
  must only await, never block.
- **`main(sim)`** is a driver: the front runs the script as a process of its
  own (`simesh.runner`), attached to a running simulation or starting one,
  with its output streamed to the page. `sim` is a `simesh.Sim`. When main
  started the simulation, the simulation is paused when main ends: its
  stations stopped and their state kept in its run, to be resumed.
- **`report(run_dir)`**, `def` or `async def`, runs after main has ended
  (and after that pause), in a process of its own, and returns the run's
  report as Markdown; the front keeps it as the run's `report.md` and the
  page shows it.

A simulation started with a script keeps a copy of it in its run, and a
snapshot keeps that copy, so a factory reset after a snapshot is loaded sets
the station up the way it was set up the first time.

Listing a script reads it without running it: its docstring and which of
the two it defines come from its syntax tree.
"""

import ast
import importlib.util
import os
import sys

import store

SETUP, MAIN, REPORT = "setup", "main", "report"
DEFAULT_TEXT = '''"""A new script."""
import simesh


async def setup(node):
    """Once per station with no state: after its declared settings, before its radio starts."""


async def main(sim):
    """The driver, attached to a running simulation."""
    await sim.all_up()
'''


def script_path(name):
    return os.path.join(store.SCRIPTS_DIR, store.check_name(name, "script") + ".py")


def names():
    """Every script on disk, by name."""
    return store.listing(store.SCRIPTS_DIR, ".py")


def parse(text, where):
    """A script's syntax tree, or StoreError saying where it does not parse."""
    try:
        return ast.parse(text, filename=where)
    except SyntaxError as err:
        raise store.StoreError("%s, line %s: %s" % (where, err.lineno, err.msg)) from err


def entry_points(tree):
    """Which of setup and main a script defines, as async functions, and
    whether it has a report, async or not."""
    found = {node.name for node in tree.body if isinstance(node, ast.AsyncFunctionDef)}
    plain = {node.name for node in tree.body if isinstance(node, ast.FunctionDef)}
    for name in (SETUP, MAIN):
        if name in plain:
            raise store.StoreError("`%s` must be `async def %s`" % (name, name))
    points = {name: name in found for name in (SETUP, MAIN)}
    points[REPORT] = REPORT in found or REPORT in plain
    return points


def describe(path, name=None):
    """What a listing says of a script: its name, the first line of its
    docstring, and which entry points it has."""
    name = name or os.path.splitext(os.path.basename(path))[0]
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    try:
        tree = parse(text, os.path.basename(path))
        doc = (ast.get_docstring(tree) or "").strip().splitlines()
        return {"name": name, "doc": doc[0] if doc else "", **entry_points(tree)}
    except store.StoreError as err:
        return {"name": name, "doc": "", SETUP: False, MAIN: False, REPORT: False,
                "error": str(err)}


def read(name):
    path = script_path(name)
    if not os.path.isfile(path):
        raise store.StoreError("no script called %r" % name)
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def write(name, text, new=False):
    """Check a script parses, then put it in place."""
    path = script_path(name)
    if new and os.path.exists(path):
        raise store.StoreError("there is already a script called %r" % name)
    if not new and not os.path.isfile(path):
        raise store.StoreError("no script called %r" % name)
    entry_points(parse(text, name + ".py"))
    store.write_text(path, text)
    return describe(path, name)


def load(path, name=None):
    """A script run as a module, for its `setup` (inside simd) or its `main`
    (in the runner). The simesh library is importable from it, and it is
    loaded under a name of its own so two scripts never share a module."""
    if store.SIM_DIR not in sys.path:
        sys.path.insert(0, store.SIM_DIR)
    name = name or os.path.splitext(os.path.basename(path))[0]
    spec = importlib.util.spec_from_file_location(
        "simesh_script_%s" % name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except SyntaxError as err:
        raise store.StoreError("script %s, line %s: %s" % (name, err.lineno, err.msg)) from err
    except Exception as err:             # noqa: BLE001 - a script's own code, run to load it
        raise store.StoreError("script %s would not load: %r" % (name, err)) from err
    for entry in (SETUP, MAIN):
        fn = getattr(module, entry, None)
        if fn is not None and not callable(fn):
            raise store.StoreError("script %s: %s is not a function" % (name, entry))
    return module
