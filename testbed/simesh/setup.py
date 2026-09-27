"""One station as a script's `setup(node)` sees it, inside simd.

`setup` runs once per station, when it boots with no state, after its
declared name, role and radio figures and before its radio is started.
Everything here acts on that one station at once, in its kind's own
language, and awaits its answer:

    node.name, node.id, node.tags, node.device, node.kind, node.role, node.radio
    await node.run("tcp peer add {addr:internet}:4965")     a line, macros filled in
    await node.set_role("transport")                        an intent, in its kind's lines
    await node.set_radio(sf=9, tx_dbm=10)
    await node.set_name()
    await node.announce()
    await node.peer_tcp("internet", 4965)
    node.addr("internet")                                   another node's address

`kind` is the kind's type (`reticulous`, `berlinmesh`), so a script with
lines for more than one firmware can say which is which. An intent the kind
has no way to say raises `simesh.setup.Refused`; a line the station could not
be asked raises it too, with what went wrong.
"""


class Refused(Exception):
    """The station could not be asked, or its kind has no way to say it."""


class Node:
    """One station being set up. simd makes these; a script only uses them."""

    def __init__(self, name, record, kind, run, lines, addr):
        self.name = name
        self.id = int(record["id"])
        self.tags = list(record.get("tags") or ())
        self.device = record.get("device")
        self.role = record.get("role")
        self.radio = dict(record.get("radio") or {})
        self.kind = kind
        self._run = run
        self._lines = lines
        self._addr = addr

    def __repr__(self):
        return "<node %s #%d %s>" % (self.name, self.id, self.kind)

    def addr(self, other=None):
        """This station's loopback address in the run's network, or another node's."""
        return self._addr(other or self.name)

    async def run(self, line):
        """One line in this station's own language; what it said back."""
        return await self._run(line)

    async def intent(self, verb, **args):
        """An intent in this kind's lines, each run in turn; the replies, joined."""
        replies = []
        for line in self._lines(verb, **args):
            replies.append(await self.run(line))
        return "\n".join(r.rstrip("\n") for r in replies)

    async def set_name(self):
        return await self.intent("name")

    async def set_role(self, role):
        return await self.intent("role", role=role)

    async def set_radio(self, **radio):
        return await self.intent("radio", **radio)

    async def announce(self):
        return await self.intent("announce")

    async def peer_tcp(self, other, port=4965):
        return await self.intent("peer_tcp", addr=self.addr(other), port=port)
