"""Scripts: plain Python against the simesh library, run from top to end.

    testbed/scripts/<name>.py

    '''What this script is for, in its first line.'''
    from simesh import *

    time("real")
    firmware("all", "reticulous_dev_latest")
    on_first_boot(nodes(tag="tcp-peer"), "tcp peer add {addr:internet}:4965")

    up("all")
    announce(nodes(tag="lora"), spread=300)

A script says what its simulation runs and what is done to it, in order
(simesh.library is the whole of it): declarations first, then whatever it
does, the first of which starts its simulation. It is run by the runner
(simesh.runner), a process of its own, never inside simd: what a station
is given at its first boot travels to simd as data, `on_first_boot()`'s
rules, not as the script's code. A script may define `report(run_dir)`,
which returns the run's report as Markdown once the script has run to its
end.

A simulation started by a script keeps a copy of it in its run, and a
snapshot keeps that copy.

Listing a script reads it without running it: its docstring, whether it has
a report, and the files it imports or includes (`references`), from its
syntax tree, and theirs in turn.

Two files beside the scripts are everyone's:

    testbed/scripts/globals.py        settings every script shares; the names
                                      in SHARED are also read by the page and
                                      the analysis, without running anything
    testbed/nodesets/<name>.py        a nodeset's own setup, which a script
                                      includes for each nodeset of its world
"""

import ast
import importlib.util
import os
import sys

import store

REPORT = "report"
DEFAULT_TEXT = '''"""A new script."""
from simesh import *

time("real")
firmware("all", "reticulous_dev_latest")
include("scripts/startup.py")

up("all")
'''
GLOBALS = "globals"
# The names of globals.py read outside a script, and what each must be: the
# radio every station without the `no-radio` tag is set to, which the page's
# coverage and links, the loss tables' band and the analysis take as given.
SHARED = (("FREQ_MHZ", float), ("SF", int), ("BW_KHZ", float), ("CR", int))
NODESET_SETUP_TEXT = '''"""{name}'s own setup: what only this nodeset's nodes need."""
from simesh import *
'''


def script_path(name):
    return os.path.join(store.SCRIPTS_DIR, store.check_name(name, "script") + ".py")


def globals_path(directory=None):
    """globals.py: the store's, or a run's own copy in `directory`."""
    return os.path.join(directory or store.SCRIPTS_DIR, GLOBALS + ".py")


def shared(path=None):
    """globals.py's SHARED names, read without running it: {name: value}.
    Each must be assigned a literal at the file's top level."""
    path = path or globals_path()
    try:
        with open(path, encoding="utf-8") as handle:
            tree = parse(handle.read(), os.path.basename(path))
    except OSError as err:
        raise store.StoreError("%s: %s" % (path, err)) from err
    found = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 \
                and isinstance(node.targets[0], ast.Name):
            try:
                found[node.targets[0].id] = ast.literal_eval(node.value)
            except ValueError:
                continue
    out = {}
    for name, kind in SHARED:
        if name not in found:
            raise store.StoreError("%s: %s is not set to a number at its top level" % (path, name))
        try:
            out[name] = kind(found[name])
        except (TypeError, ValueError) as err:
            raise store.StoreError("%s: %s is a number, not %r" % (path, name, found[name])) from err
    return out


def shared_radio(path=None):
    """The radio globals.py sets every station to, in the intent's words."""
    got = shared(path)
    return {"freq_mhz": got["FREQ_MHZ"], "sf": got["SF"], "bw_khz": got["BW_KHZ"],
            "cr": got["CR"]}


def nodeset_setup_path(name):
    return os.path.join(store.NODESETS_DIR, store.check_name(name, "nodeset") + ".py")


def read_nodeset_setup(name):
    """A nodeset's setup script: (text, whether it has one yet). One it does
    not have yet reads as a fresh one's text."""
    path = nodeset_setup_path(name)
    if not os.path.isfile(path):
        return NODESET_SETUP_TEXT.format(name=name), False
    with open(path, encoding="utf-8") as handle:
        return handle.read(), True


def write_nodeset_setup(name, text):
    """Check a nodeset's setup parses, then put it in place."""
    parse(text, name + ".py")
    store.write_text(nodeset_setup_path(name), text)


def names():
    """Every script on disk, by name."""
    return store.listing(store.SCRIPTS_DIR, ".py")


def parse(text, where):
    """A script's syntax tree, or StoreError saying where it does not parse."""
    try:
        return ast.parse(text, filename=where)
    except SyntaxError as err:
        raise store.StoreError("%s, line %s: %s" % (where, err.lineno, err.msg)) from err


def has_report(tree):
    return any(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == REPORT
               for node in tree.body)


def references(tree, seen=None):
    """The files a script imports or includes that are SIMesh's own, and
    those they import or include in turn: other scripts of the store, and
    the simesh library's modules, as [{name, path, library}], `name` as the
    script spells it. An include whose path is not written out (one per
    nodeset, say) is not followed: nothing but running it says which."""
    wanted = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            wanted += [(a.name, module_file(a.name)) for a in node.names]
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            wanted.append((node.module, module_file(node.module)))
            wanted += [("%s.%s" % (node.module, a.name), module_file("%s.%s" % (node.module, a.name)))
                       for a in node.names]
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name) \
                and node.func.id == "include" and node.args \
                and isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str):
            path = os.path.abspath(os.path.join(store.SIM_DIR, node.args[0].value))
            wanted.append((node.args[0].value, path if os.path.isfile(path) else None))
    seen = set() if seen is None else seen
    out = []
    scripts = os.path.abspath(store.SCRIPTS_DIR) + os.sep
    for name, path in wanted:
        if not path or path in seen:
            continue
        seen.add(path)
        library = not path.startswith(scripts)
        out.append({"name": name, "path": os.path.relpath(path, store.SIM_DIR), "library": library})
        if not library:
            with open(path, encoding="utf-8") as handle:
                try:
                    out += references(parse(handle.read(), os.path.basename(path)), seen)
                except store.StoreError:
                    continue
    return out


def module_file(name):
    """The file of a module a script imports, when it is one of SIMesh's
    own (the library or another script), else None."""
    parts = name.split(".")
    if parts == ["simesh"]:
        parts = ["simesh", "library"]       # what `from simesh import *` is
    if parts[0] == "simesh":
        base = os.path.join(store.SIM_DIR, *parts)
    elif len(parts) == 1:
        base = os.path.join(os.path.abspath(store.SCRIPTS_DIR), parts[0])
    else:
        return None
    for path in (base + ".py", os.path.join(base, "__init__.py")):
        if os.path.isfile(path):
            return os.path.abspath(path)
    return None


def describe(path, name=None):
    """What a listing says of a script: its name, the first line of its
    docstring, whether it has a report, and what it imports."""
    name = name or os.path.splitext(os.path.basename(path))[0]
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    try:
        tree = parse(text, os.path.basename(path))
        doc = (ast.get_docstring(tree) or "").strip().splitlines()
        return {"name": name, "doc": doc[0] if doc else "", REPORT: has_report(tree),
                "references": references(tree)}
    except store.StoreError as err:
        return {"name": name, "doc": "", REPORT: False, "references": [], "error": str(err)}


def listing():
    """Every script's `describe`, with `included_by`: the scripts that
    include or import it. A script some other one includes (startup.py,
    globals.py) is a part of those, not a thing to run on its own."""
    rows = [describe(script_path(each), each) for each in names()]
    by_path = {os.path.relpath(script_path(row["name"]), store.SIM_DIR): row for row in rows}
    for row in rows:
        row["included_by"] = []
    for row in rows:
        for ref in row["references"]:
            other = by_path.get(ref["path"])
            if other is not None and other is not row:
                other["included_by"].append(row["name"])
    return rows


def read(name):
    path = script_path(name)
    if not os.path.isfile(path):
        raise store.StoreError("no script called %r" % name)
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def read_reference(relpath):
    """A file a script refers to, by its path under testbed/: its text."""
    path = os.path.abspath(os.path.join(store.SIM_DIR, relpath))
    lib = os.path.join(os.path.abspath(store.SIM_DIR), "simesh") + os.sep
    scripts = os.path.abspath(store.SCRIPTS_DIR) + os.sep
    if not path.endswith(".py") or not (path.startswith(lib) or path.startswith(scripts)):
        raise store.StoreError("%s is not the library's or a script" % relpath)
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def write(name, text, new=False):
    """Check a script parses, then put it in place."""
    path = script_path(name)
    if new and os.path.exists(path):
        raise store.StoreError("there is already a script called %r" % name)
    if not new and not os.path.isfile(path):
        raise store.StoreError("no script called %r" % name)
    parse(text, name + ".py")
    store.write_text(path, text)
    return describe(path, name)


def _module(path, name):
    for where in (store.SIM_DIR, os.path.abspath(store.SCRIPTS_DIR)):
        if where not in sys.path:
            sys.path.insert(0, where)
    spec = importlib.util.spec_from_file_location(
        "simesh_script_%s" % name.replace("-", "_"), path)
    return spec, importlib.util.module_from_spec(spec)


def run_file(path, name=None):
    """Run a script, its top to its end, as a module of its own: the module."""
    name = name or os.path.splitext(os.path.basename(path))[0]
    spec, module = _module(path, name)
    spec.loader.exec_module(module)
    return module


def module_of(path, name=None):
    """A script's definitions without running it: its imports, its functions
    and classes and its plain assignments, what `report` needs, and none of
    what it does."""
    name = name or os.path.splitext(os.path.basename(path))[0]
    with open(path, encoding="utf-8") as handle:
        tree = parse(handle.read(), os.path.basename(path))
    kept = (ast.Import, ast.ImportFrom, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef,
            ast.Assign, ast.AnnAssign)
    tree.body = [node for node in tree.body if isinstance(node, kept)]
    _, module = _module(path, name)
    exec(compile(tree, path, "exec"), module.__dict__)   # noqa: S102 - the script's own definitions
    return module
