#!/usr/bin/env python3
"""One firmware process, its pty, its log and its supervisor.

A station is a whole firmware built for Linux and run as an ordinary process,
keeping the station contract (STATION.md); its kind (kinds/) says which
binary and how to talk to it. It gets:

- a **directory** under the run, `run/nodes/<name>/`, its cwd, with its
  state under `state/`;
- a **pty**, because its stdin and stdout are its serial console — the
  supervisor holds the master end and reads it on the pty thread (`Ptys`),
  which takes framed-RPC replies out of what the station writes (rpc.py) and
  hands them to the station's client, appends everything else to `log`, and
  passes the same bytes to whoever is watching the console;
- a **loopback address** its id fixes in the testbed's network (`bind_addr`),
  where its sockets bind;
- a **supervisor** that starts it again when it exits, because a restart on
  this target is a process exit.

Status is what the map draws: `stopped` before anything is started and after
it is told to stop, `starting` from the fork until its kind says it is up, `setup`
while its setup lines are going in, `up` once it is answering, `restarting`
in the gap after an unasked-for exit. Nothing here decides when `setup`
happens — the caller does that, through `on_status`.
"""

import asyncio
import ipaddress
import os
import pty
import sys
import threading
import tty

import rpc as rpc_module

RESTART_DELAY = 0.5         # seconds before a station that exited comes back

# Stations take their addresses from one network, `simd --net`, filled one /24
# at a time with hosts 5 to 254: node 1 is the first network's .5, node 250
# its .254, node 251 the next network's .5. A /22 is four of those, 1000
# stations. Every station binds its own address, so two testbeds on one host
# take two networks; two stations with one address are one port taken twice.
NET = "127.0.0.0/22"
HOST_FIRST = 5
HOST_LAST = 254
HOSTS_PER_NET = HOST_LAST - HOST_FIRST + 1

STOPPED, STARTING, SETUP, UP, RESTARTING = (
    "stopped", "starting", "setup", "up", "restarting")


def log(msg):
    sys.stderr.write("sim: %s\n" % msg)
    sys.stderr.flush()


def set_net(cidr):
    """Take the station network from `simd --net`; a /24 or wider."""
    global NET
    net = ipaddress.ip_network(cidr, strict=False)
    if net.version != 4 or net.prefixlen > 24:
        raise ValueError("the station network must be an IPv4 /24 or wider, not %s" % cidr)
    NET = str(net)


def max_node_id():
    """How many stations the network holds: 250 per /24 in it."""
    net = ipaddress.ip_network(NET)
    return HOSTS_PER_NET * (1 << (24 - net.prefixlen))


def bind_addr(node_id):
    """The station's own loopback address."""
    net = ipaddress.ip_network(NET)
    index = node_id - 1
    if not 0 <= index < max_node_id():
        raise ValueError("node id %d is outside the %d the network %s holds"
                         % (node_id, max_node_id(), NET))
    subnet, host = divmod(index, HOSTS_PER_NET)
    return str(net.network_address + subnet * 256 + HOST_FIRST + host)


class Ptys:
    """The stations' console ptys, read on a thread of their own.

    Everything a station prints is read, split into text and framed-RPC
    replies (rpc.FrameDemux) and appended to its log here, on an event loop of
    this thread's, so that however much a fleet prints, none of it is work for
    the loop that runs the medium. That loop is handed only what it acts on,
    with call_soon_threadsafe, in the order it was read: a reply frame, the
    capability marker, console bytes while a console window is open, the end
    of the stream. The log file is this thread's alone once a station has
    started, so nothing another thread writes can land between two reads.
    """

    def __init__(self):
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever, name="ptys",
                                       daemon=True)
        self.thread.start()

    def call(self, fn, *args):
        """Run fn(*args) on the pty thread, after whatever is queued there."""
        self.loop.call_soon_threadsafe(fn, *args)


_ptys = None


def ptys():
    """The process's pty thread, started on first use."""
    global _ptys
    if _ptys is None:
        _ptys = Ptys()
    return _ptys


class Drain:
    """One start of one station, read on the pty thread.

    `main` is the loop the station belongs to; `rpc` and `station` are only
    ever touched there.
    """

    def __init__(self, station, master, rpc, main):
        self.station = station
        self.master = master
        self.log_file = station.log_file
        self.rpc = rpc
        self.main = main
        self.demux = rpc_module.FrameDemux()
        self.marker = rpc_module.MarkerWatch()
        self.resync = None
        self.reading = False

    # ---- on the pty thread -------------------------------------------------

    def attach(self):
        asyncio.get_running_loop().add_reader(self.master, self.readable)
        self.reading = True

    def readable(self):
        try:
            data = os.read(self.master, 65536)
        except BlockingIOError:
            return
        except OSError:
            data = b""          # the station let go of the far end
        if not data:
            self.stop_reading()
            self.main.call_soon_threadsafe(self.station.pty_closed, self)
            return
        loop = asyncio.get_running_loop()
        text, frames = self.demux.feed(data, loop.time())
        self.deliver(text, frames)
        if self.demux.pending and self.resync is None:
            self.resync = loop.call_later(rpc_module.RESYNC_S, self.resync_due)

    def resync_due(self):
        self.resync = None
        if not self.reading:
            return
        loop = asyncio.get_running_loop()
        text, frames = self.demux.expire(loop.time())
        self.deliver(text, frames)
        if self.demux.pending:
            self.resync = loop.call_later(rpc_module.RESYNC_S, self.resync_due)

    def deliver(self, text, frames):
        if text:
            self.log_file.write(text)
            if self.marker.feed(text):
                self.main.call_soon_threadsafe(self.rpc.on_marker)
            if self.station.watchers:
                self.main.call_soon_threadsafe(self.station.console_out, text)
        for frame_id, payload in frames:
            self.main.call_soon_threadsafe(self.rpc.on_frame, frame_id, payload)

    def stop_reading(self):
        if self.resync is not None:
            self.resync.cancel()
            self.resync = None
        if self.reading:
            self.reading = False
            asyncio.get_running_loop().remove_reader(self.master)

    def close(self):
        """What the station wrote before it went is still read, then the pty
        is let go."""
        while self.reading:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                data = b""
            if not data:
                break
            text, frames = self.demux.feed(data, asyncio.get_running_loop().time())
            self.deliver(text, frames)
        text, frames = self.demux.expire(float("inf"))
        self.deliver(text, frames)
        self.stop_reading()
        try:
            os.close(self.master)
        except OSError:
            pass


class Station:
    """One firmware process, its pty and its log."""

    def __init__(self, name, node_id, directory, kind, ether_addr,
                 on_status=None, on_output=None, clock=None):
        self.name = name
        self.node_id = node_id
        self.dir = directory
        self.kind = kind                # a kinds.Kind: the binary and how to talk to it
        self.ether_addr = ether_addr
        # The ether, in a virtual-time run: it waits for a station it is told
        # is starting, and stops waiting for one that has gone.
        self.clock = clock
        self.on_status = on_status      # (station, status)
        self.on_output = on_output      # (station, bytes)
        self.master = None
        self.proc = None
        self.log_file = None
        self.status = STOPPED
        self.transport = None           # whether it forwards, as last read; None unknown
        # Whether this station had been through a first boot when it was last
        # started. Sampled at the fork, because the station writes `state/boot`
        # moments later and the answer the setup step needs is the one from
        # before it ran.
        self.was_configured = False
        self.stopping = False
        self.supervisor = None
        self.watchers = 0               # console windows open on it; read on the pty thread
        self.drain = None               # this start's reader, on the pty thread
        self.rpc = None                 # framed RPC on the pty, one per start
        self.outbox = bytearray()       # console bytes the pty has not taken yet

    # ---- identity --------------------------------------------------------

    @property
    def addr(self):
        return bind_addr(self.node_id)

    @property
    def log_path(self):
        return os.path.join(self.dir, "log")

    @property
    def state_dir(self):
        return os.path.join(self.dir, "state")

    @property
    def configured(self):
        """True when this station has been through its first boot already.

        What marks that is the kind's business — a file the firmware writes
        once it has run — and a directory without it is a station that has
        never been set up, which is what decides whether the setup lines go in.
        """
        return self.kind.configured(self)

    def set_status(self, status):
        if status == self.status:
            return
        self.status = status
        if self.on_status is not None:
            self.on_status(self, status)

    # ---- the process -----------------------------------------------------

    def env(self):
        env = dict(os.environ)
        env.update(self.kind.env(self))
        return env

    async def start(self):
        self.was_configured = self.configured
        os.makedirs(self.state_dir, exist_ok=True)
        if self.log_file is None:
            self.log_file = open(self.log_path, "ab", buffering=0)
        ptys().call(self.log_file.write, b"\n--- station %s starting ---\n"
                    % self.name.encode("utf-8"))

        master, slave = pty.openpty()
        tty.setraw(slave)       # a serial line has no echo and no translation
        self.master = master
        if self.rpc is not None:
            self.rpc.close()
        self.rpc = rpc_module.RpcClient(self.write)
        self.outbox.clear()
        self.set_status(STARTING)
        if self.clock is not None:
            self.clock.expect(self.node_id)
        try:
            self.proc = await asyncio.create_subprocess_exec(
                self.kind.elf, cwd=self.dir, env=self.env(),
                stdin=slave, stdout=slave, stderr=slave)
        except OSError:
            if self.clock is not None:
                self.clock.leave(self.node_id)
            raise
        os.close(slave)
        os.set_blocking(master, False)
        self.drain = Drain(self, master, self.rpc, asyncio.get_running_loop())
        ptys().call(self.drain.attach)
        log("station %s (%d, %s) up as pid %d on %s" % (
            self.name, self.node_id, self.kind.name, self.proc.pid, self.addr))

    def console_out(self, text):
        """Console bytes the pty thread read, for whoever has the window open."""
        if self.on_output is not None:
            self.on_output(self, text)

    def pty_closed(self, drain):
        """The pty thread read the end of this start's stream."""
        if drain is self.drain:
            self.detach_reader()

    def detach_reader(self):
        if self.rpc is not None:
            self.rpc.close()
        if self.master is None:
            return
        try:
            asyncio.get_running_loop().remove_writer(self.master)
        except (OSError, ValueError):
            pass
        # The pty thread reads what is left, then closes the pty: from here on
        # this loop neither reads nor writes it.
        if self.drain is not None:
            ptys().call(self.drain.close)
        else:
            os.close(self.master)
        self.drain = None
        self.master = None
        self.outbox.clear()

    def write(self, data):
        """Type into the station's console.

        What the pty will not take now waits for it, in order: a frame cut
        short would read as a corrupt one.
        """
        if self.master is None:
            return
        if self.outbox:
            self.outbox += data
            return
        try:
            sent = os.write(self.master, data)
        except BlockingIOError:
            sent = 0
        except OSError:
            return
        if sent < len(data):
            self.outbox += data[sent:]
            asyncio.get_running_loop().add_writer(self.master, self.writable)

    def writable(self):
        if self.master is None:
            return
        try:
            sent = os.write(self.master, bytes(self.outbox))
        except BlockingIOError:
            return
        except OSError:
            sent = len(self.outbox)
        del self.outbox[:sent]
        if not self.outbox:
            asyncio.get_running_loop().remove_writer(self.master)

    def resize(self, cols, rows):
        """Tell the station's console how wide its terminal is."""
        if self.master is None:
            return
        import fcntl
        import struct
        import termios
        try:
            fcntl.ioctl(self.master, termios.TIOCSWINSZ,
                        struct.pack("HHHH", rows, cols, 0, 0))
        except OSError:
            pass

    # ---- the supervisor --------------------------------------------------

    async def supervise(self, after_start=None):
        """Keep the station running until it is told to stop.

        `after_start` is awaited once per start, with the station, and is
        where the caller waits for the CLI and sends the setup lines. It is
        cancelled when the process exits under it.
        """
        while not self.stopping:
            await self.start()
            watcher = None
            if after_start is not None:
                watcher = asyncio.ensure_future(after_start(self))
            try:
                code = await self.proc.wait()
            finally:
                # Stopped or exited, this start is over, and so is its setup:
                # a watcher left running would set up whatever comes next.
                if watcher is not None:
                    watcher.cancel()
                if self.clock is not None:
                    self.clock.leave(self.node_id)
            self.detach_reader()
            if self.stopping:
                return
            self.set_status(RESTARTING)
            log("station %s exited (%s), restarting" % (self.name, code))
            if self.clock is not None:
                await self.clock.sleep(RESTART_DELAY)
            else:
                await asyncio.sleep(RESTART_DELAY)

    def run(self, after_start=None):
        """Start the supervisor as a task and keep hold of it."""
        self.stopping = False
        if self.clock is not None:
            # Now, not when the supervisor gets to it: T must not move on
            # without a station that is about to exist.
            self.clock.expect(self.node_id)
        self.supervisor = asyncio.ensure_future(self.supervise(after_start))
        return self.supervisor

    async def stop(self):
        """Stop the process and the supervisor, and wait for both to go."""
        self.stopping = True
        if self.clock is not None:
            self.clock.leave(self.node_id)
        if self.supervisor is not None:
            self.supervisor.cancel()
            try:
                await self.supervisor
            except asyncio.CancelledError:
                pass
            self.supervisor = None
        self.detach_reader()
        if self.proc is not None and self.proc.returncode is None:
            try:
                self.proc.terminate()
            except ProcessLookupError:
                pass
            try:
                await asyncio.wait_for(self.proc.wait(), 5)
            except asyncio.TimeoutError:
                log("station %s would not stop; killing it" % self.name)
                self.proc.kill()
                await self.proc.wait()
        self.proc = None
        if self.log_file is not None:
            ptys().call(self.log_file.close)
            self.log_file = None
        self.set_status(STOPPED)

    async def restart(self):
        """Stop the process and let the supervisor bring it back."""
        if self.proc is not None and self.proc.returncode is None:
            try:
                self.proc.terminate()
            except ProcessLookupError:
                pass
