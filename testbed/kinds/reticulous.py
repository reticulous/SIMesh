"""The reticulous firmware (spangap/reticulous), built for the Linux host.

Spec keys beyond the common ones:

    fixed: <dir>      the build's merged /fixed tree (esp-idf/build.linux/data_merged)

A station of this kind is set up and asked things over framed RPC on its
console pty (rpc.py): every setup line, every Run command line, the flush and
the transport poll is one frame, answered with what the command printed. It
has a web UI on port 80, a store that coalesces writes until `save`, and
`state/boot` once it has booted.
"""

import asyncio
import os

import rpc as rpc_module

from . import CommandError, Kind

TRANSPORT_KEY = "s.rnsd.transport_enabled"
MARKER_WAIT_S = 20.0        # how long a started station has to print the framed-RPC marker
SLOW_S = rpc_module.EXEC_BOUND_S - 0.5   # a reply this late may have been cut at the bound
SETTLE_POLL_S = 0.5

# A line whose effect lands after its reply, and the one key that says it has.
CONFIRM = {
    "lora up": ("s.lora.0.enable", "1"),
}


class Reticulous(Kind):
    type_name = "reticulous"

    def __init__(self, name, spec, scenario_dir):
        super().__init__(name, spec, scenario_dir)
        self.fixed = self.path(self.spec.get("fixed"))

    def env(self, station):
        env = super().env(station)
        # What this firmware reads: its board (hw-linux) takes its identity,
        # directory, address, ether and /fixed tree from these names.
        env.update(SPANGAP_NODE_ID=str(station.node_id),
                   SPANGAP_NODE_DIR=station.dir,
                   SPANGAP_BIND_ADDR=station.addr,
                   SPANGAP_ETHER=station.ether_addr)
        if self.fixed:
            env["SPANGAP_FIXED_DIR"] = self.fixed
        return env

    def client(self, station):
        if station.rpc is None:
            raise CommandError("%s is not running" % station.name)
        return station.rpc

    async def wait_up(self, station, timeout):
        return await self.client(station).wait_ready(
            timeout, MARKER_WAIT_S, lambda s: self.pause(station, s))

    async def run(self, station, line, timeout=None):
        try:
            return await self.client(station).query(line, timeout=timeout)
        except rpc_module.RpcError as err:
            raise CommandError(str(err)) from err

    async def show(self, station, key):
        return rpc_module.parse_setting(await self.run(station, "show %s" % key), key)

    async def settle(self, station, line, elapsed):
        """After a line, wait until what it asked for has landed.

        A reply that came back at the device's exec bound may have been cut
        there with the command still running, so the next frame waits for the
        CLI to answer again. A line in CONFIRM is followed until its key says
        it took.
        """
        loop = asyncio.get_running_loop()
        deadline = loop.time() + 2 * rpc_module.QUERY_TIMEOUT_S
        if elapsed >= SLOW_S:
            while loop.time() < deadline:
                try:
                    if (await self.run(station, rpc_module.PROBE)).strip():
                        break
                except CommandError:
                    pass
                await self.pause(station, SETTLE_POLL_S)
        want = CONFIRM.get(" ".join(line.split()))
        if want is None:
            return
        key, value = want
        while loop.time() < deadline:
            if await self.show(station, key) == value:
                return
            await self.pause(station, SETTLE_POLL_S)
        raise CommandError("%s: %s never read %s" % (line, key, value))

    async def setup(self, station, lines):
        """Each line as its own frame, confirmed where it has to be, then `save`.

        The store coalesces writes for `s.storage.flash_delay` seconds — a
        minute by default — so a station set up and then reset inside that
        window would come back with none of it. `save` is not a setting,
        which is why the testbed sends it and the scenario does not.
        """
        loop = asyncio.get_running_loop()
        failed = []
        for line in lines:
            if not line.strip() or line.strip().startswith("#"):
                continue
            began = loop.time()
            try:
                await self.run(station, line)
                await self.settle(station, line, loop.time() - began)
            except CommandError as err:
                failed.append("%s: %s" % (line, err))
        await self.flush(station)
        if failed:
            raise CommandError("; ".join(failed))

    async def flush(self, station):
        try:
            await self.run(station, "save")
        except CommandError:
            pass

    async def transport(self, station):
        value = await self.show(station, TRANSPORT_KEY)
        if value is None:
            return None
        return value not in ("0", "")

    def web_port(self):
        return 80

    def configured(self, station):
        return os.path.exists(os.path.join(station.dir, "state", "boot"))
