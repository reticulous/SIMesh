#!/usr/bin/env python3
"""Framed RPC over a station's console pty.

    testbed → station   F5 53 47 01 <id> <len:2> <command>
    station → testbed   F5 53 47 01 <id> <len:2> <what the command printed>

The station's console is its serial port, and the firmware multiplexes a
framed side channel onto it (spangap-core/docs/framed-rpc.md): a frame is
never echoed, never enters the line editor and never turns the log into a CLI
session, so the testbed can ask a station things while a person types at the
same console. The reply is length-counted and interleaves with the log; the
pty drain takes it out of the stream here and passes every other byte on
unchanged.

Three pieces:

- `FrameDemux`, a state machine over the byte stream. It survives a magic
  split across reads, gives the bytes of a false start back to the text, and
  resyncs on the magic when a frame turns out not to be one — an id outside
  the range a query uses, a magic inside a payload, or a remainder that never
  arrives.
- `MarkerWatch`, the capability marker found in text that arrives in pieces.
  It matches `serial] framed rpc v1` as well as `serial: framed rpc v1`: the
  firmware's logger prints the line under a `[serial]` tag.
- `RpcClient`, one per station start: one frame in flight, the id derived from
  the command so a retry reuses it and a late reply answers the retry, and the
  capability marker the firmware prints early in boot, else one recoverable
  probe.

The device bounds a command's execution at five seconds and cuts a reply at
its last complete line, silently. So a caller asks for one key at a time, and
a line whose effect matters is confirmed by asking for that effect.
"""

import asyncio
import re

MAGIC = b"\xf5\x53\x47\x01"
HEADER = len(MAGIC) + 3             # the magic, the id, two bytes of length
ID_FIRST, ID_LAST = 0x20, 0xBF      # a query's id never leaves this range
MARKER = re.compile(rb"serial(?:\]|:) framed rpc v1")
MARKER_TAIL = 64                    # bytes of text kept to match a marker split across reads

RESYNC_S = 1.0          # a frame with no progress for this long was not a frame
EXEC_BOUND_S = 5.0      # the device's own bound on one command
QUERY_TIMEOUT_S = EXEC_BOUND_S + 3.0
PROBE = "show s.net.hostname"
PROBE_PAUSE_S = 0.1     # between probes of a station whose command line is not up yet
CTRL_C = b"\x03"


def query_id(command):
    """The id for a command: what was asked, not when, in 0x20..0xBF.

    The same hash flashmon uses, so a station answers both the same way. The
    range keeps a probe typed at firmware that does not speak frames from
    carrying a CR, an LF, a Ctrl-C or a 0xC0 into its console.
    """
    h = 0
    for ch in command:
        h = (h * 31 + ord(ch)) & 0xFF
    return ID_FIRST + h % (ID_LAST - ID_FIRST + 1)


def frame(frame_id, payload):
    """One frame, either direction."""
    if len(payload) > 0xFFFF:
        raise ValueError("a frame carries at most 65535 bytes")
    return MAGIC + bytes((frame_id, len(payload) >> 8, len(payload) & 0xFF)) + payload


class FrameDemux:
    """Reply frames out of a console byte stream; everything else is text.

    `feed(data, now)` returns `(text, frames)`: the bytes that were not frame,
    in order, and each complete frame as `(id, payload)`. Bytes that might
    still be the start of a frame are held until the next read settles them,
    and `expire(now)` gives them back once they have waited `RESYNC_S`.

    Giving bytes back is a replay: the first held byte is text, and the rest
    go through the machine again, because a magic can start inside a false
    one (`F5 F5 53 47 01 …`).
    """

    SCAN, MATCH, HEAD, BODY = range(4)

    def __init__(self):
        self.state = self.SCAN
        self.held = bytearray()     # everything of the frame in progress
        self.want = 0               # payload bytes the frame in progress still needs
        self.since = None           # when the frame in progress last grew

    @property
    def pending(self):
        """True while bytes are held for a frame that has not completed."""
        return bool(self.held)

    def feed(self, data, now=0.0):
        text = bytearray()
        frames = []
        if data:
            self.since = now
        self._run(bytes(data), text, frames)
        if not self.held:
            self.since = None
        return bytes(text), frames

    def expire(self, now):
        """Give up on a frame that stopped arriving. `(text, frames)` as feed."""
        text = bytearray()
        frames = []
        if self.held and self.since is not None and now - self.since >= RESYNC_S:
            self._give_back(text, frames)
            if self.held:
                self.since = now
        if not self.held:
            self.since = None
        return bytes(text), frames

    # ---- the machine -----------------------------------------------------

    def _give_back(self, text, frames):
        """The frame in progress was not one: its first byte is text, and the
        rest is read again from the top."""
        held = bytes(self.held)
        self.held.clear()
        self.state = self.SCAN
        self.want = 0
        text += held[:1]
        self._run(held[1:], text, frames)

    def _run(self, data, text, frames):
        i = 0
        n = len(data)
        while i < n:
            if self.state == self.SCAN:
                j = data.find(MAGIC[0], i)
                if j < 0:
                    text += data[i:]
                    return
                text += data[i:j]
                self.held = bytearray(data[j:j + 1])
                self.state = self.MATCH
                i = j + 1
            elif self.state == self.MATCH:
                b = data[i]
                if b == MAGIC[len(self.held)]:
                    self.held.append(b)
                    i += 1
                    if len(self.held) == len(MAGIC):
                        self.state = self.HEAD
                    continue
                # A false start: what was held is text, and the disagreeing
                # byte is read again — it may open a magic of its own.
                self._give_back(text, frames)
            elif self.state == self.HEAD:
                self.held.append(data[i])
                i += 1
                if len(self.held) == len(MAGIC) + 1:
                    if not ID_FIRST <= self.held[-1] <= ID_LAST:
                        self._give_back(text, frames)
                    continue
                if len(self.held) == HEADER:
                    self.want = (self.held[5] << 8) | self.held[6]
                    self.state = self.BODY
                    if self.want == 0:
                        self._deliver(frames)
            else:  # BODY
                take = data[i:i + self.want]
                # A magic inside a payload is the next frame starting: the
                # length was wrong, or something that is not a reply landed
                # inside this one. What was held is text from the top.
                prefix = bytes(self.held[max(HEADER, len(self.held) - (len(MAGIC) - 1)):])
                window = prefix + take
                k = window.find(MAGIC)
                if k >= 0:
                    cut = k - len(prefix)       # where the magic starts in take
                    if cut < 0:
                        keep = len(self.held) + cut
                        rest = bytes(self.held[keep:]) + data[i:]
                        del self.held[keep:]
                    else:
                        self.held += take[:cut]
                        rest = data[i + cut:]
                    text += self.held
                    self.held.clear()
                    self.state = self.SCAN
                    self.want = 0
                    self._run(rest, text, frames)
                    return
                self.held += take
                self.want -= len(take)
                i += len(take)
                if self.want == 0:
                    self._deliver(frames)

    def _deliver(self, frames):
        frames.append((self.held[4], bytes(self.held[HEADER:])))
        self.held.clear()
        self.state = self.SCAN


class MarkerWatch:
    """The capability marker, found in text that arrives in pieces."""

    def __init__(self):
        self.tail = b""
        self.seen = False

    def feed(self, data):
        """True the first time the marker has gone by."""
        if self.seen or not data:
            return False
        window = self.tail + data
        if MARKER.search(window):
            self.seen = True
            self.tail = b""
            return True
        self.tail = window[-MARKER_TAIL:]
        return False


class RpcError(Exception):
    """The station does not speak frames, or did not answer."""


class RpcClient:
    """Framed RPC with one station, for the life of one start.

    `write` puts bytes on the station's console. The station's drain hands
    this `on_text` and `on_frame` for what it read. One frame is in flight at
    a time: the device runs frames one after another, and two queries whose
    ids collided would each take the other's answer.
    """

    def __init__(self, write):
        self.write = write
        self.lock = asyncio.Lock()
        self.waiters = {}           # id -> the future of the query waiting for it
        self.late = {}              # id -> a reply that arrived with nobody waiting
        self.marker = asyncio.Event()
        self.available = False      # the marker was seen, or a probe answered
        self.probed = False
        self.closed = False
        self.watch = MarkerWatch()

    # ---- what the drain hands in -----------------------------------------

    def on_text(self, data):
        if self.watch.feed(data):
            self.on_marker()

    def on_marker(self):
        self.available = True
        self.marker.set()

    def on_frame(self, frame_id, payload):
        text = payload.decode("utf-8", "replace")
        waiter = self.waiters.pop(frame_id, None)
        if waiter is not None and not waiter.done():
            waiter.set_result(text)
            return
        # Nobody asked, or the asker gave up. A retry carries the same id, so
        # this answers it without a second execution.
        self.late[frame_id] = text
        while len(self.late) > 16:
            self.late.pop(next(iter(self.late)))

    def close(self):
        """The station is gone: nothing more will be answered."""
        self.closed = True
        for waiter in self.waiters.values():
            if not waiter.done():
                waiter.set_exception(RpcError("the station went away"))
        self.waiters.clear()

    # ---- asking ----------------------------------------------------------

    async def query(self, command, timeout=None, tries=2):
        """Run one command; what it printed. RpcError when it was not answered."""
        if timeout is None:
            timeout = QUERY_TIMEOUT_S
        if not self.available:
            raise RpcError("the station has not said it speaks framed RPC")
        payload = command.encode("utf-8")
        frame_id = query_id(command)
        loop = asyncio.get_running_loop()
        async with self.lock:
            for _ in range(tries):
                if self.closed:
                    raise RpcError("the station went away")
                self.late.pop(frame_id, None)
                waiter = loop.create_future()
                self.waiters[frame_id] = waiter
                self.write(frame(frame_id, payload))
                try:
                    return await asyncio.wait_for(waiter, timeout)
                except asyncio.TimeoutError:
                    self.waiters.pop(frame_id, None)
                    late = self.late.pop(frame_id, None)
                    if late is not None:
                        return late
        raise RpcError("no answer to %r in %.0fs" % (command, timeout * tries))

    async def wait_ready(self, timeout, marker_wait, pause=asyncio.sleep):
        """True once the station answers a frame.

        The marker says the sniffer is armed, which is very early in boot; the
        CLI a frame runs on comes up a little later and answers nothing until
        then, so the probe is asked until it answers. A station whose marker
        was never seen gets one probe, sent blind: on firmware that does not
        speak frames it is typed at the console, carries no line end, and is
        undone with a Ctrl-C.

        `pause` waits between probes, on the run's clock: in a virtual-time run
        half a second of wall can be many seconds of the station's time.
        """
        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout
        try:
            await asyncio.wait_for(self.marker.wait(), min(marker_wait, timeout))
        except asyncio.TimeoutError:
            return await self.probe_once()
        while loop.time() < deadline and not self.closed:
            try:
                reply = await self.query(PROBE, timeout=min(QUERY_TIMEOUT_S,
                                                            max(0.1, deadline - loop.time())),
                                         tries=1)
                if reply.strip():
                    return True
            except RpcError:
                pass
            await pause(PROBE_PAUSE_S)
        return False

    async def probe_once(self):
        if self.probed or self.closed:
            return self.available
        self.probed = True
        self.available = True
        try:
            reply = await self.query(PROBE, tries=1)
        except RpcError:
            reply = None
        if reply is not None and reply.strip():
            return True
        if self.marker.is_set():
            return True     # it spoke up while the probe was in flight
        self.available = False
        self.write(CTRL_C)
        return False


def parse_setting(text, key):
    """The value out of a `show <key>` reply, which is `<key> = <value>`."""
    for line in text.splitlines():
        name, sep, value = line.partition("=")
        if sep and name.strip() == key:
            return value.strip()
    return None
