"""Which nodes: a selection, as a condition on each node's facts.

    nodes(tag="lora")                           every node carrying the tag
    nodes(tag="lora", role="transport")         both: keywords in one call are AND
    nodes(tag=("lora", "tcp-peer"))             either tag: a tuple, list or set is any of
    nodes(tag="lora") | nodes(name="internet")  OR
    nodes(tag="lora") & nodes(kind="reticulous")  AND
    nodes(tag="lora") - nodes(role="client")    AND NOT
    ~nodes(firmware="sergeyculum_local_latest")  NOT
    nodes(height_m=lambda h: h > 20)            a function of the value, in a driver only
    nodes()                                     every node

A selection is Python's own set algebra over nodes, `&`, `|`, `-`, `^` and
`~`, and is lazy: it is a condition, not a list, so one written at a
script's top, before there is a simulation, picks whatever nodes it matches
when it is used, a node placed later included. `firmware()` sends its
condition to the simulation, so there it must be plain values (a function
cannot travel); a driver evaluates it where it stands.

What a condition can name, each node's facts:

    name, id            what the nodeset calls it and its station id
    tag                 one of its tags
    firmware            the device name its firmware was given as
    kind                its firmware's kind (reticulous, sergeyculum)
    role                the role it reports, else its role tag (transport, router, repeater)
    antenna             its antenna's type
    max_dbm             its maximum power at the antenna connector
    lat, lon, height_m  where it stands
    status              stopped, starting, setup, up, restarting

`which()` takes whatever a script says for "which nodes": `"all"`, a node's
name, a list of names, a selection, or a `sim.nodes(…)` selection.
"""

FIELDS = ("name", "id", "tag", "firmware", "kind", "role", "antenna", "max_dbm", "lat",
          "lon", "height_m", "status")
ALL = "all"


class Nodes:
    """A condition on a node's facts; see the module's docstring."""

    def __init__(self, tree):
        self.tree = tree

    def __and__(self, other):
        return Nodes({"and": [self.tree, which(other).tree]})

    def __or__(self, other):
        return Nodes({"or": [self.tree, which(other).tree]})

    def __sub__(self, other):
        return Nodes({"and": [self.tree, {"not": which(other).tree}]})

    def __xor__(self, other):
        return (self - other) | (which(other) - self)

    def __invert__(self):
        return Nodes({"not": self.tree})

    __rand__, __ror__ = __and__, __or__

    def __repr__(self):
        return "nodes(%s)" % describe(self.tree)

    def matches(self, facts):
        """Whether a node with these facts is one of these."""
        return _matches(self.tree, facts)

    def pick(self, every):
        """The names of those of `every` ({name: facts}) that match, in id order."""
        chosen = [(facts.get("id") or 0, name) for name, facts in every.items()
                  if self.matches(dict(facts, name=name))]
        return [name for _, name in sorted(chosen)]

    def to_json(self):
        """The condition as plain data, for the simulation; ValueError for one
        that holds a function."""
        _check_plain(self.tree)
        return self.tree

    @classmethod
    def from_json(cls, tree):
        _check_tree(tree)
        return cls(tree)


def nodes(**where):
    """The nodes whose facts are all as given; see the module's docstring."""
    for field in where:
        if field not in FIELDS:
            raise TypeError("nodes() has no field %r (it has %s)" % (field, ", ".join(FIELDS)))
    if not where:
        return Nodes({ALL: True})
    return Nodes({"where": {k: (list(v) if isinstance(v, (tuple, list, set, frozenset)) else v)
                            for k, v in where.items()}})


def which(given):
    """What a script says for "which nodes", as a selection."""
    if isinstance(given, Nodes):
        return given
    if isinstance(given, str):
        return Nodes({ALL: True}) if given == ALL else Nodes({"names": [given]})
    names = getattr(given, "names", None)          # a Sim's Selection
    if isinstance(names, list):
        return Nodes({"names": list(names)})
    try:
        return Nodes({"names": [str(n) for n in given]})
    except TypeError:
        raise TypeError("which nodes? \"all\", a node's name, a list of names or nodes(…), "
                        "not %r" % (given,)) from None


def _value_matches(want, have):
    if callable(want):
        return bool(want(have))
    if isinstance(want, list):
        return have in want
    return have == want


def _matches(tree, facts):
    if ALL in tree:
        return True
    if "names" in tree:
        return facts.get("name") in tree["names"]
    if "and" in tree:
        return all(_matches(t, facts) for t in tree["and"])
    if "or" in tree:
        return any(_matches(t, facts) for t in tree["or"])
    if "not" in tree:
        return not _matches(tree["not"], facts)
    for field, want in tree["where"].items():
        if field == "tag":
            tags = facts.get("tags") or ()
            if callable(want):
                if not any(want(t) for t in tags):
                    return False
            elif not any(t in (want if isinstance(want, list) else [want]) for t in tags):
                return False
        elif not _value_matches(want, facts.get(field)):
            return False
    return True


def _check_plain(tree):
    if "where" in tree:
        for field, want in tree["where"].items():
            if callable(want) or (isinstance(want, list) and any(callable(w) for w in want)):
                raise ValueError("nodes(%s=<function>) cannot travel to the simulation: "
                                 "name the values" % field)
    for key in ("and", "or"):
        for sub in tree.get(key, ()):
            _check_plain(sub)
    if "not" in tree:
        _check_plain(tree["not"])


def _check_tree(tree):
    if not isinstance(tree, dict) or len(tree) != 1:
        raise ValueError("a selection is one of all, names, where, and, or, not: %r" % (tree,))
    key, value = next(iter(tree.items()))
    if key in ("and", "or"):
        for sub in value:
            _check_tree(sub)
    elif key == "not":
        _check_tree(value)
    elif key == "where":
        if not isinstance(value, dict) or any(f not in FIELDS for f in value):
            raise ValueError("a selection's fields are %s" % ", ".join(FIELDS))
    elif key == "names":
        if not isinstance(value, list):
            raise ValueError("a selection's names are a list")
    elif key != ALL:
        raise ValueError("a selection is one of all, names, where, and, or, not: %r" % (tree,))


def describe(tree):
    """A selection as a reader would write it."""
    if ALL in tree:
        return ""
    if "names" in tree:
        return "names=%r" % (tree["names"],)
    if "where" in tree:
        return ", ".join("%s=%r" % kv for kv in tree["where"].items())
    if "not" in tree:
        return "~(%s)" % describe(tree["not"])
    join = " & " if "and" in tree else " | "
    return join.join("(%s)" % describe(t) for t in tree.get("and") or tree.get("or"))
