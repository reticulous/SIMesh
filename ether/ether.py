#!/usr/bin/env python3
"""The virtual ether: one UDP endpoint that carries frames between stations.

Stations announce themselves with `hello`, describe their radio with `state`
and hand over a transmission with `tx`; the ether answers `welcome`,
`rx_begin` and `rx_end`.

The level a frame arrives at comes from the loss table: every ordered pair of
nodes has a path loss per band (slt.py), and

    L = P_tx + G_tx + G_rx − loss(tx → rx) − 20·log10(f / f0)

with P_tx the power the frame itself went out at, the gains the nodes'
antennas and the last term the within-band correction from the table's centre
to the frame's carrier. A pair the table does not have, or has as +inf, is
never heard. The table arrives by direct call — `set_losses()` and its
relatives — from whatever is driving the run; the UDP wire to the stations
never mentions it, and a station never learns where it is.

Reception is decided at each receiver, from everything arriving there
(README.md, "What it models"; INTERNALS.md, "Reception"). Every transmission
whose carrier overlaps a receiver's bandwidth is interference there, whatever
its spreading factor or sync word; only a frame whose bandwidth, spreading
factor and sync word match the receiver's state, on its carrier, can be
decoded. The receiver locks on to a decodable frame at its preamble unless it
is already demodulating another that the new one does not lead by the
same-SF figure, and a frame it is locked on survives when, over every
stretch of its air, it clears thermal noise by its spreading factor's
demodulation threshold and each class of interference, summed within the
class, by that class's rejection figure. A station in `CAD` is told a frame is
arriving, and nothing more, when that frame is decodable there or the energy
in its band crosses the sense threshold.

With `pairwise` set the pairwise rule decides instead, on the same levels: a
frame is delivered where it is audible, the receiver takes a later frame only
when it leads the one in progress by the capture margin, and a frame survives
only by leading each audible same-carrier interferer by that margin, one at a
time and without summing.

Clocks. In a real-time run a station's `t` fields are its own clock and are
meaningful only relative to each other inside one message; the ether rebases
every frame onto its own monotonic clock and schedules from that, so the `t`
fields it sends back are ether microseconds. The offsets inside a frame
(`t_pre - t0`, `t_hdr - t0`, `t_end - t0`) are the transmitter's and are
carried through unchanged, which is what a receiver actually needs. In a
virtual-time run (`--time max` or `<k>x`) the ether is the conductor: its
clock is T, which it moves only when every station has answered its last
message with `idle`, and every message it sends carries T and the station's
`seq` (README.md, "Virtual time").

Every datagram in and out, except `idle` and `run`, is one tab-separated
line of the record: stamp (the wall clock in a real-time run, T in seconds in
a virtual one), direction, station id, JSON.
"""

import argparse
import asyncio
import contextlib
import heapq
import json
import math
import os
import random
import socket
import sys
import time
from array import array
from datetime import datetime, timezone

import slt

# What a receiver's state must share with a frame for the frame to be decoded.
MATCH_KEYS = ("bw", "sf", "sync")

DEFAULT_POWER_DBM = 14      # a `tx` that did not say what it was sent at

# ---- The figures ----------------------------------------------------------
#
# Every number the reception model decides with, and where it comes from.
#
# Thermal noise: kTB at 290 K is −174 dBm in one hertz; a receiver's floor is
# that plus 10·log10(bandwidth) plus its noise figure. 6 dB is the SX1262's
# order of magnitude and a setting of the medium (`Physics`).
THERMAL_DBM_PER_HZ = -174.0
DEFAULT_NOISE_FIGURE_DB = 6

# Demodulation threshold: the SNR over thermal noise a spreading factor needs,
# from the SX1261/2 datasheet: −7.5 dB at SF7 and 2.5 dB lower per step. A
# frame under its own modem's threshold is never decoded, and a receiver does
# not lock on to it. SF12 buys 17.5 dB of reach over SF7 and pays for it in
# air time; a flat threshold would make the two identical to the medium.
SENSITIVITY_DB = {5: -2.5, 6: -5.0, 7: -7.5, 8: -10.0,
                  9: -12.5, 10: -15.0, 11: -17.5, 12: -20.0}
SLOWEST_SENSITIVITY_DB = -20.0      # a frame that named no spreading factor

# Same-SF rejection: how far a frame must lead the summed power of every
# same-SF transmission in its band to be demodulated through it. The 6 dB of
# Semtech's specification, the figure Croce et al. (below) quote as "6 dB
# specified" against the 0–1 dB they measure; the specification's is used
# because it is the chip's own and because the medium takes a receiver's
# worst stretch, not its average. The same figure is the lead a later frame
# needs to take a receiver off the one it is demodulating.
SAME_SF_REJECTION_DB = 6.0

# Inter-SF rejection: the signal-to-interference ratio, in dB, a frame at
# spreading factor `ref` needs over the summed power of every transmission in
# its band at spreading factor `int`. Measured on the SX1272: D. Croce,
# M. Gucciardo, S. Mangione, G. Santaromita, I. Tinnirello, "Impact of LoRa
# Imperfect Orthogonality: Analysis of Link-Level Performance", IEEE
# Communications Letters 22(4), 2018, Table II ("SIR thresholds with SX1272
# transceiver"), rows SF_ref, columns SF_int. Its diagonal (1 dB) is replaced
# by the same-SF figure above. SF5 and SF6, which the SX1262 has and the
# SX1272 measurement does not, take the SF7 row and column.
INTER_SF_REJECTION_DB = {
    #      int:  7      8      9     10     11     12
    7:  {7: 1, 8: -8, 9: -9, 10: -9, 11: -9, 12: -9},
    8:  {7: -11, 8: 1, 9: -11, 10: -12, 11: -13, 12: -13},
    9:  {7: -15, 8: -13, 9: 1, 10: -13, 11: -14, 12: -15},
    10: {7: -19, 8: -18, 9: -17, 10: 1, 11: -17, 12: -18},
    11: {7: -22, 8: -22, 9: -21, 10: -20, 11: 1, 12: -20},
    12: {7: -25, 8: -25, 9: -25, 10: -24, 11: -23, 12: 1},
}

# Sense threshold: the summed in-band level at which carrier sense calls the
# channel busy although nothing on it is decodable. ETSI EN 300 220-1
# V3.1.1 (2017-02) clause 5.21.2, Table 45: the clear-channel-assessment
# threshold for a device under 100 mW e.r.p. is 15 dB above the receiver
# sensitivity limit of Table 32, 10·log10(bandwidth in kHz) − 117 dBm, so
# −81 dBm at 125 kHz. An SX1262 CAD correlates for chirps of its own
# spreading factor and bandwidth, which is the "decodable" half of the busy
# test; this is the other half, the energy an RSSI-based listen-before-talk
# acts on.
SENSE_ABOVE_SENSITIVITY_DB = 15.0
SENSITIVITY_LIMIT_DBM_AT_1KHZ = -117.0

# Pairwise rule only: how far a frame must lead each audible interferer.
PAIRWISE_CAPTURE_DB = 6.0

# How far apart two carriers may be and still be one carrier, as a fraction
# of the bandwidth. The synthesizer steps in 32 MHz / 2^25, so two drivers
# asked for the same frequency round it to register values tens of hertz
# apart; an exact match would make them deaf to each other, which no receiver
# is. A LoRa demodulator tolerates an offset of a quarter of its bandwidth.
CARRIER_TOLERANCE = 0.25

# ---------------------------------------------------------------------------

SPEED_OF_LIGHT = 299_792_458.0

MAX_FRAME_US = 60 * 1000 * 1000   # a stated timeline longer than this is junk

# A station that asks, this many times running, to run at the T it already
# has is given this much T instead: one FreeRTOS tick.
STANDING_LIMIT = 64
STANDING_QUANTUM_US = 10_000

# An idle that arrives this long, in wall time, after the message it answers
# came from a station that was busy: the station's own watchdog reports after
# 20 ms. Counted per station, so a run that crawls can say who holds it.
SLOW_IDLE_S = 0.018

# The receive buffer asked of the kernel for the ether's socket, in bytes
# (it gives no more than net.core.rmem_max). The default, some 200 KB, is less
# than a hundred stations' radio settings arriving at one instant, and a
# datagram the kernel drops is a message the ether never hears.
RECV_BUFFER_BYTES = 4 * 1024 * 1024

# The least wall time between two resends to one station of what it missed.
RESEND_GAP_S = 0.1

# Bytes written into a channel of the run (a station's console, a TCP
# connection between two stations) hold T until their reader has taken them.
# A reader that has not, this long in wall time after they were written, is
# not waiting for them; the channel lets go of T and says so.
UNREAD_GRACE_S = 1.0

# How many turns of the event loop `settle()` waits, at most, for the loop to
# have nothing else ready.
SETTLE_TURNS = 1000


def same_carrier(freq_a, freq_b, bw_hz):
    """True when two stated frequencies are one carrier at this bandwidth."""
    if freq_a is None or freq_b is None:
        return freq_a == freq_b
    return abs(freq_a - freq_b) <= CARRIER_TOLERANCE * float(bw_hz or 125_000)


def in_band(freq_a, bw_a, freq_b, bw_b):
    """True when two transmissions' channels overlap in frequency at all."""
    if freq_a is None or freq_b is None:
        return freq_a == freq_b
    half = (float(bw_a or 125_000) + float(bw_b or 125_000)) / 2.0
    return abs(freq_a - freq_b) < half


def rejection_db(sf_signal, sf_interferer):
    """The lead a frame at `sf_signal` needs over the summed interference at
    `sf_interferer`: the same-SF figure, or the inter-SF matrix's entry."""
    if sf_signal is None or sf_interferer is None or sf_signal == sf_interferer:
        return SAME_SF_REJECTION_DB
    ref = min(12, max(7, int(sf_signal)))
    other = min(12, max(7, int(sf_interferer)))
    if ref == other:
        return SAME_SF_REJECTION_DB
    return float(INTER_SF_REJECTION_DB[ref][other])


def sense_threshold_dbm(bw_hz):
    """The summed in-band level above which carrier sense calls the channel busy."""
    return (10.0 * math.log10(max(float(bw_hz or 125_000), 1000.0) / 1000.0)
            + SENSITIVITY_LIMIT_DBM_AT_1KHZ + SENSE_ABOVE_SENSITIVITY_DB)


def dbm_to_mw(dbm):
    return 10.0 ** (dbm / 10.0)


def mw_to_dbm(mw):
    return 10.0 * math.log10(mw) if mw > 0 else -math.inf


def wall_stamp():
    """The wall-clock timestamp that heads a record line."""
    return datetime.now(timezone.utc).isoformat(timespec="microseconds")


class RealClock:
    """The event loop's own clock, in microseconds."""

    virtual = False

    def __init__(self, loop):
        self.loop = loop

    def now(self):
        return int(self.loop.time() * 1_000_000)

    def call_at(self, t_us, callback, *args, key=0):
        return self.loop.call_at(t_us / 1_000_000.0, callback, *args)


class VirtualClock:
    """Conductor time T, in microseconds: moved by the barrier, not the wall.

    Events are kept in one heap and run when the barrier reaches their
    instant, in order of instant, then key — a station's id, so events at one
    instant run in station order — then the order they were scheduled in.
    """

    virtual = True

    def __init__(self):
        self.t = 0
        self.heap = []
        self.count = 0

    def now(self):
        return self.t

    def call_at(self, t_us, callback, *args, key=0):
        self.count += 1
        heapq.heappush(self.heap, (int(t_us), key, self.count, callback, args))

    def next_due(self):
        return self.heap[0][0] if self.heap else None

    def pop_due(self):
        """The next event at or before T, or None."""
        if self.heap and self.heap[0][0] <= self.t:
            _, _, _, callback, args = heapq.heappop(self.heap)
            return callback, args
        return None


def describe_time(mode, rate):
    if mode == "real":
        return "real"
    return "virtual, as fast as it goes" if rate is None else "virtual, paced at %gx" % rate


def parse_time_mode(text):
    """`real`, `max` or `<k>x` into (mode, rate): rate None is as fast as it goes."""
    text = (text or "real").strip().lower()
    if text == "real":
        return "real", 1.0
    if text == "max":
        return "virtual", None
    if text.endswith("x"):
        try:
            rate = float(text[:-1])
        except ValueError:
            rate = 0.0
        if rate > 0:
            return "virtual", rate
    raise ValueError("--time takes real, max or <k>x, not %r" % text)


def log(msg):
    """One line of human-readable running commentary."""
    sys.stderr.write("%s  %s\n" % (datetime.now().strftime("%H:%M:%S.%f")[:-3], msg))
    sys.stderr.flush()


def fspl_1m_db(freq_hz):
    """Free-space path loss over the first metre at this carrier, in dB: the
    log-distance model's anchor, for whatever computes synthetic ground's table."""
    return 20.0 * math.log10(4.0 * math.pi * max(freq_hz, 1.0) / SPEED_OF_LIGHT)


class Physics:
    """The medium's own setting beyond path loss: the receivers' noise figure."""

    def __init__(self, noise_figure_db=DEFAULT_NOISE_FIGURE_DB):
        self.noise_figure_db = float(noise_figure_db)

    def describe(self):
        return "noise figure %.1f dB" % self.noise_figure_db

    @classmethod
    def from_dict(cls, data):
        data = data or {}
        return cls(data.get("noise_figure_db", DEFAULT_NOISE_FIGURE_DB))

    def as_dict(self):
        return {"noise_figure_db": self.noise_figure_db}


class Station:
    """A station the ether has heard from, and the radio state it last stated."""

    def __init__(self, sid, addr, slots):
        self.sid = sid
        self.addr = addr
        self.slots = slots
        self.states = {}        # slot -> the last `state` message for it
        self.locks = {}         # slot -> the Reception its demodulator follows
        self.tx_until = 0       # while its own frame is on the air, it is deaf
        # The conductor's view of it, in a virtual-time run: the last sequence
        # number sent to it, whether it has said it is idle since, and the
        # conductor time it next needs to run at (None: never, on its own).
        self.seq = 0
        self.idle = False
        self.until = None
        self.told = 0           # the T it was last sent, which is the T it has
        self.on_idle = []       # callbacks for its next idle
        self.asking = 0         # TCP writes it is waiting to be let make
        self.asked_at = 0       # its seq when it last asked
        self.standing = 0       # idles in a row that asked for T itself
        self.granted_at = 0.0   # the wall clock when it was last told anything
        self.idle_said = None   # (seq, until) of the idle last taken from it
        self.unanswered = []    # (seq, datagram) sent since its last idle, for a resend
        self.resent_at = 0.0    # the wall clock of the last resend to it
        self.stale_said = None  # the number of the last idle it said for an older message
        self.slow_idles = 0     # idles that took the busy watchdog's time or more
        # The power its last frame went out at. A station states this per
        # transmission rather than in its `state`, so it is learned by
        # listening — and it is what `levels()` must answer with, or the map
        # would draw a reach the station does not have.
        self.power_dbm = DEFAULT_POWER_DBM

    def state(self, slot):
        return self.states.get(slot)

    def listening(self, slot):
        """True when this station's slot last said it was receiving or sensing."""
        st = self.states.get(slot)
        return bool(st) and st.get("mode") in ("RX", "CAD")

    def sensing(self, slot):
        """True when this station's slot last said it was in CAD."""
        st = self.states.get(slot)
        return bool(st) and st.get("mode") == "CAD"


class Channel:
    """Bytes on their way from one part of a virtual run to another.

    What the testbed types at a station's console (`tty/<sid>`), and a TCP
    connection between two stations, one way (`tcp/<a>><b>`, the writer's
    address and port, then the reader's). `writer` and `reader` are station
    ids, None for the testbed. `written` and `taken` count the bytes each end
    has said it put in and took out; while `written` is ahead, the reader has
    input it has not seen, and T waits.
    """

    def __init__(self, key, writer, reader):
        self.key = key
        self.writer = writer
        self.reader = reader
        self.written = 0
        self.taken = 0
        self.timer = None       # the unread grace, while it holds T

    def holding(self):
        return self.written > self.taken


class Frame:
    """A transmission in flight, on the ether's clock.

    A frame carries every other transmission it shared air and band with,
    whatever their spreading factors, and the level it arrives at each
    station, computed once on first asking so a table replaced mid-frame
    does not change a frame already on the air. The verdict is not one of
    its properties: each receiver rules at its own rx_end.
    """

    def __init__(self, eid, fid, sid, msg, start_us, end_us, pre_us, hdr_us):
        self.eid = eid          # the ether's own number, unique across stations
        self.fid = fid          # the transmitter's, unique only to it
        self.sid = sid
        self.freq = msg.get("freq")
        self.bw = msg.get("bw")
        self.sf = msg.get("sf")
        self.sync = msg.get("sync")
        self.power_dbm = msg.get("power_dbm", DEFAULT_POWER_DBM)
        self.payload = msg.get("payload", "")
        self.start_us = start_us
        self.end_us = end_us
        self.pre_us = pre_us
        self.hdr_us = hdr_us
        self.interferers = []   # frames that shared this one's band and air
        self.receivers = []     # (sid, slot, level) for each decoding receiver
        self.levels = {}        # rsid -> dBm there, or None: never heard

    def shares_air(self, other):
        """True when the two frames overlap in band and in time."""
        return (in_band(self.freq, self.bw, other.freq, other.bw)
                and self.start_us < other.end_us
                and other.start_us < self.end_us)


class Reception:
    """One receiver slot decoding one frame, from rx_begin to rx_end.

    `lost` is set when a louder frame took the receiver off this one at its
    own preamble, or when this one arrived while the receiver was following
    another it did not lead: the rx_end still goes out, as `crc`, and the
    chip, which follows only the last frame it was begun on, drops it.
    """

    def __init__(self, frame, rsid, slot, level, lost=False):
        self.frame = frame
        self.rsid = rsid
        self.slot = slot
        self.level = level
        self.lost = lost


class Ether(asyncio.DatagramProtocol):
    """The UDP endpoint: parses station messages and delivers frames.

    Whatever drives a run gives it the loss tables with `set_losses()` and
    subscribes to `on_tx`, `on_rx` and `on_station` to watch the air. Each
    callback takes the arguments named beside it below and returns nothing;
    exceptions raised in one are logged and swallowed, because a page that has
    gone away must not be able to stop the medium.
    """

    def __init__(self, record_path, physics=None, seed=None, time_mode="real",
                 pairwise=False, epoch=None):
        self.transport = None
        self.loop = asyncio.get_event_loop()
        self.mode, self.rate = parse_time_mode(time_mode)
        self.clock = VirtualClock() if self.mode == "virtual" else RealClock(self.loop)
        # The wall-clock microseconds T = 0 stands for, so every station's
        # time() agrees with every other's. A virtual run may be given one,
        # so that two runs of the same network put the same wall clock, and
        # so the same timestamps, into their stations.
        self.epoch = int(time.time() * 1_000_000) - self.clock.now()
        if epoch is not None and self.mode == "virtual":
            self.epoch = int(epoch)
        self.expected = set()       # stations started and not yet heard from
        self.busy_count = 0         # stations not idle since they were last told anything
        self.pending = []           # (sid, arrival, addr, msg) held for the barrier
        self.holds = 0              # the testbed's own work in hand at the T it has
        self.settling = []          # callbacks for when the loop is next quiet
        self.settle_turns = 0
        self.channels = {}          # key -> Channel with bytes in it, or owed some
        self.unread = 0             # channels holding T
        self.endpoints = {}         # "addr:port" -> the station a TCP endpoint is
        self.asks = []              # (sid, arrival, channel, n, reply, addr): writes waiting
        self.dirty = set()          # stations that have run since their output was read
        self.on_drain = None        # (sids, done): read what they printed, then done()
        self.arrivals = 0
        self.advancing = False
        self.pace_timer = None
        self.pace_origin = None     # (wall seconds, T) a paced run is measured from
        self.barriers = 0           # times T has moved
        self.resends = 0            # messages sent again to a station that missed them
        self.standing = 0           # steps in a row that left T where it was
        self.physics = physics or Physics()
        self.pairwise = bool(pairwise)
        self.stations = {}          # sid -> Station
        self.tables = {}            # band name -> slt.Table
        self.names = {}             # sid -> node name, the table's index
        self.gains = {}             # sid -> antenna gain in dBi
        self.frames = []            # frames still in flight or just ended
        self.next_eid = 0           # the ether's own frame numbering
        self.seed = seed if seed is not None else random.randrange(1 << 31)
        self.on_tx = None           # (sid, eid, freq, t_start, t_end)
        self.on_rx = None           # (sid, rsid, eid, verdict, level)
        self.on_station = None      # (sid, state)
        self.record = None
        if record_path is not None:
            self.record = open(record_path, "a", encoding="utf-8", buffering=1)
            self.record.write("# %s\tether record: stamp\tdir\tsid\tjson\n"
                              % wall_stamp())

    # ---- clock ---------------------------------------------------------

    def now(self):
        """The ether's clock in microseconds: the loop's in a real-time run,
        conductor time T in a virtual one."""
        return self.clock.now()

    def call_at(self, t_us, callback, *args, key=0):
        """Run `callback(*args)` at `t_us` on the ether's clock."""
        return self.clock.call_at(t_us, callback, *args, key=key)

    async def sleep(self, seconds):
        """Wait `seconds` on the ether's clock: conductor time in a virtual run.

        In a virtual run the wait ends at its instant of T and T stays there
        until the caller has done what it woke up to do: the instant holds T
        from when it falls due until the loop has run everything the caller
        and whatever it started made ready (`settle()`). So a station started,
        or a line typed, after a sleep is started or typed at the sleep's T,
        whatever the pace.
        """
        if not self.clock.virtual:
            await asyncio.sleep(seconds)
            return
        done = self.loop.create_future()
        held = []

        def due():
            if not done.done():
                self.holds += 1
                held.append(True)
                done.set_result(None)

        self.clock.call_at(self.clock.t + int(seconds * 1_000_000), due, key=-1)
        self.kick()
        try:
            await done
        finally:
            if held:
                self.settle(self.release)

    def release(self):
        """One hold on T is let go."""
        self.holds -= 1
        self.kick()

    def settle(self, callback):
        """Run `callback` once the loop has nothing else ready to run.

        A coroutine woken by a future runs a loop turn later, and what it
        starts (a task, a gather) a turn after that; checking the loop's ready
        queue each turn follows the whole chain, for up to SETTLE_TURNS turns.
        One check serves every callback waiting, so they do not keep each
        other's checks from ever finding the loop quiet.
        """
        self.settling.append(callback)
        if len(self.settling) == 1:
            self.settle_turns = 0
            self.loop.call_soon(self.settle_check)

    def settle_check(self):
        ready = getattr(self.loop, "_ready", None)
        if ready and self.settle_turns < SETTLE_TURNS:
            self.settle_turns += 1
            self.loop.call_soon(self.settle_check)
            return
        batch, self.settling = self.settling, []
        for callback in batch:
            callback()

    # ---- the conductor --------------------------------------------------

    def expect(self, sid):
        """A station is starting: T waits for it to say hello and then idle."""
        if self.clock.virtual:
            self.expected.add(sid)

    def leave(self, sid):
        """A station has stopped: T no longer waits for it."""
        self.expected.discard(sid)
        if self.forget(sid):
            log("station %d left" % sid)
        self.kick()

    def forget(self, sid):
        """Drop a station and everything it said; True if there was one."""
        station = self.stations.pop(sid, None)
        if station is None:
            return False
        if not station.idle:
            self.busy_count -= 1
        self.pending = [p for p in self.pending if p[0] != sid]
        self.asks = [a for a in self.asks if a[0] != sid]
        for key, channel in list(self.channels.items()):
            if sid in (channel.writer, channel.reader):
                self.drop_channel(channel)
        self.endpoints = {e: s for e, s in self.endpoints.items() if s != sid}
        return True

    def mark(self, station, idle):
        """Whether a station has said it is idle since it was last told anything."""
        if station.idle != idle:
            station.idle = idle
            self.busy_count += -1 if idle else 1

    def busy(self):
        """True while T must wait: a station started and not yet idle, input
        a reader has not taken, or the testbed's own work at this instant."""
        return (bool(self.expected) or self.busy_count > 0 or self.unread > 0
                or self.holds > 0)

    def waiting_on(self):
        """The stations T is waiting for: started and unheard, not idle, or
        with input they have not read (the testbed, when it is the reader, is
        not named)."""
        unread = {c.reader for c in self.channels.values()
                  if c.holding() and c.reader is not None}
        return sorted(self.expected) + sorted(
            sid for sid, st in self.stations.items() if not st.idle or sid in unread)

    def next_instant(self):
        """Where T goes next: the earliest a station or the ether needs to run."""
        candidates = [st.until for st in self.stations.values() if st.until is not None]
        due = self.clock.next_due()
        if due is not None:
            candidates.append(due)
        if not candidates:
            return None
        return max(self.clock.t, min(candidates))

    def kick(self):
        """Move T as far as the barrier lets it, now."""
        if not self.clock.virtual or self.advancing:
            return
        self.advancing = True
        try:
            while True:
                if self.asks and self.quiet():
                    self.answer_ask()
                    continue
                if self.busy():
                    return
                if self.pending:
                    self.flush_pending()
                    continue
                if self.dirty and self.on_drain is not None:
                    # What the stations printed at this T is read before T
                    # moves: a reply the testbed acts on is acted on here.
                    sids, self.dirty = sorted(self.dirty), set()
                    self.holds += 1
                    self.on_drain(sids, self.drained)
                    continue
                t = self.next_instant()
                if t is None:
                    return
                if not self.paced(t):
                    return
                self.step_to(t)
        finally:
            self.advancing = False

    def paced(self, t):
        """True when a paced run may move to `t` now; else a timer comes back."""
        if self.rate is None:
            return True
        wall = self.loop.time()
        if self.pace_origin is None:
            self.pace_origin = (wall, self.clock.t)
        origin_wall, origin_t = self.pace_origin
        target = origin_wall + (t - origin_t) / 1_000_000.0 / self.rate
        if target <= wall:
            if wall - target > 0.25:
                # Behind by more than a beat: carry on from here rather than
                # racing to catch up.
                self.pace_origin = (wall, self.clock.t)
            return True
        if self.pace_timer is None:
            def resume():
                self.pace_timer = None
                self.kick()
            self.pace_timer = self.loop.call_at(target, resume)
        return False

    def step_to(self, t):
        """T moves to `t`: what is due runs, and every station due is told."""
        if t == self.clock.t:
            self.standing += 1
            if self.standing % 10000 == 0:
                log("T has stood at %d for %d steps; due: %s" % (
                    t, self.standing,
                    ", ".join("%d" % sid for sid, st in sorted(self.stations.items())
                              if st.until is not None and st.until <= t)))
        else:
            self.standing = 0
        self.clock.t = t
        self.barriers += 1
        while True:
            due = self.clock.pop_due()
            if due is None:
                break
            callback, args = due
            callback(*args)
        for sid in sorted(self.stations):
            st = self.stations[sid]
            if st.idle and st.until is not None and st.until <= t:
                self.send(sid, {"type": "run"})

    def flush_pending(self):
        """What stations said while T stood still, in station order."""
        batch = sorted(self.pending, key=lambda p: (p[0], p[1]))
        self.pending = []
        for sid, _, addr, msg in batch:
            if sid not in self.stations:
                continue
            kind = msg.get("type")
            if kind == "state":
                self.recv_state(sid, addr, msg)
            elif kind == "tx":
                self.recv_tx(sid, addr, msg)

    def recv_idle(self, sid, msg):
        """A station idle after the message numbered `seq`.

        UDP may lose a datagram either way, so a station says its idle again
        while it hears nothing back. An idle for a number older than the last
        one sent is usually only crossing that message on the wire; said a
        second time, it says the station missed what came after it, which is
        sent again as it was (`unanswered`). A second idle for what is
        already known changes nothing."""
        station = self.stations.get(sid)
        seq = msg.get("seq")
        if station is None or not isinstance(seq, int):
            return
        if seq < station.seq:
            # Once is the ordinary race, an idle crossing the next message on
            # the wire; the same number again means that message never came.
            if station.stale_said == seq:
                self.resend(station, seq)
            station.stale_said = seq
            return
        station.stale_said = None
        if seq != station.seq:
            return
        station.unanswered = []
        until = msg.get("until")
        until = int(until) if isinstance(until, (int, float)) else None
        if station.idle and (seq, until) == station.idle_said:
            return
        station.idle_said = (seq, until)
        if self.loop.time() - station.granted_at >= SLOW_IDLE_S:
            station.slow_idles += 1
        self.mark(station, True)
        self.dirty.add(sid)
        station.until = until
        if station.until is not None and station.until <= self.clock.t:
            # A station that keeps asking for the instant it already has is
            # working in no time at all; work takes time, so after a while it
            # is given the next tick's worth instead.
            station.standing += 1
            if station.standing > STANDING_LIMIT:
                if station.standing == STANDING_LIMIT + 1:
                    log("station %d asks for T %d again and again; giving it %d us"
                        % (sid, self.clock.t, STANDING_QUANTUM_US))
                station.until = self.clock.t + STANDING_QUANTUM_US
        else:
            station.standing = 0
        if station.on_idle:
            waiting, station.on_idle = station.on_idle, []
            for callback in waiting:
                callback()
        self.kick()

    def resend(self, station, answered):
        """What a station was sent after message `answered`, sent again as it
        was, at most every RESEND_GAP_S of wall time."""
        now = self.loop.time()
        if now - station.resent_at < RESEND_GAP_S:
            return
        station.resent_at = now
        missed = [data for seq, data in station.unanswered if seq > answered]
        if missed:
            self.resends += len(missed)
            log("station %d missed %d message(s) after %d; sending them again"
                % (station.sid, len(missed), answered))
        for data in missed:
            self.transport.sendto(data, station.addr)

    # ---- channels: input that does not come over the air -----------------

    def console(self, sid):
        """What the testbed types at station `sid`'s console."""
        key = "tty/%d" % sid
        channel = self.channels.get(key)
        if channel is None:
            channel = self.channels[key] = Channel(key, None, sid)
        return channel

    def typed(self, sid, total):
        """The testbed is writing station `sid`'s console: `total` bytes, in
        all, since it last started it. T waits until the station has read
        them."""
        if self.clock.virtual:
            self.account(self.console(sid), wrote=total, totals=True)

    def sync(self, sid, done):
        """Run `done` once station `sid` has the T the run has and nothing in
        hand.

        A station is only told T when something happens to it, so one that
        has been idle while T moved still has the T it was last told; and one
        that has been told something has not necessarily taken it yet. A line
        typed at its console goes out once `done` runs, so the station reads
        it at this T, with no message of the ether's landing halfway through.
        """
        station = self.stations.get(sid)
        if not self.clock.virtual or station is None or (
                station.idle and station.told >= self.clock.t):
            done()
            return
        station.on_idle.append(done)
        if station.told < self.clock.t:
            self.send(sid, {"type": "run"})

    def drained(self):
        """The testbed has read what the stations printed (`on_drain`): T may
        move once what that set going has run."""
        self.settle(self.release)

    def endpoint_sid(self, endpoint):
        """The station a TCP endpoint ("addr:port") belongs to, or None: one it
        has reported from, else the station at that address."""
        sid = self.endpoints.get(endpoint)
        if sid is not None:
            return sid
        addr = endpoint.rsplit(":", 1)[0]
        for sid, station in self.stations.items():
            if station.addr and station.addr[0] == addr:
                return sid
        return None

    def recv_listen(self, sid, msg):
        """A station listens at a TCP endpoint: a connection to it is that
        station's, though another station of the run shares its address."""
        at = msg.get("at")
        if sid in self.stations and isinstance(at, str):
            self.endpoints[at] = sid

    def recv_io(self, sid, addr, msg):
        """A station's count of bytes it put into or took out of a channel.

        A write that asks (`go`) is answered, at the address it asked from,
        once its reader is in step (`sync()`); every such ask is answered,
        counted or not, since the writer waits for it.
        """
        go = msg.get("go")
        channel = self.io(sid, msg)
        if go is None:
            return
        reply = json.dumps({"type": "go", "go": go}, separators=(",", ":")).encode("utf-8")
        if channel is None:
            if self.transport is not None:
                self.transport.sendto(reply, addr)
            return
        station = self.stations[sid]
        station.asking += 1
        station.asked_at = station.seq
        self.arrivals += 1
        self.asks.append((sid, self.arrivals, channel, msg["n"], reply, addr))
        self.kick()

    def quiet(self):
        """True when nothing in the run is at work: every station idle, or
        waiting for a go-ahead it has asked for since it was last told
        anything, and no input unread but by a station that is waiting so (a
        station's thread held up in a write can keep its others from
        reading)."""
        if self.expected or self.holds:
            return False

        def waiting(st):
            return bool(st.asking) and st.seq == st.asked_at

        if not all(st.idle or waiting(st) for st in self.stations.values()):
            return False
        for channel in self.channels.values():
            if channel.holding():
                reader = self.stations.get(channel.reader)
                if reader is None or not waiting(reader):
                    return False
        return True

    def answer_ask(self):
        """Let the first write asked for, in station order, go ahead.

        Asks are answered only when the run is quiet, one at a time, and in
        station order, as the barrier takes what stations say: two stations
        writing to each other at one instant then go in the same order every
        run. A reader that has not been told the run's T is told it first.
        """
        ask = min(self.asks, key=lambda a: (a[0], a[1]))
        sid, _, channel, n, reply, addr = ask
        reader = self.stations.get(channel.reader)
        if reader is not None and reader.idle and reader.told < self.clock.t:
            self.send(channel.reader, {"type": "run"})
            if not reader.idle:
                return
        self.asks.remove(ask)
        writer = self.stations.get(sid)
        if writer is not None:
            writer.asking -= 1
        if self.channels.get(channel.key) is not channel:
            self.channels[channel.key] = channel
        self.account(channel, wrote=n)
        if self.transport is not None:
            self.transport.sendto(reply, addr)

    def io(self, sid, msg):
        """Count what a station reports; the reader of a TCP write that
        counts, else None.

        The console's counts are running totals since the process started;
        a TCP connection's are the bytes of one call. A TCP connection counts
        only when both of its ends are stations of the run; the testbed's own
        connections to a station (its web proxy) are outside it.
        """
        if sid not in self.stations:
            return None
        took = msg.get("type") == "read"
        ch = msg.get("ch")
        if ch == "tty":
            total = msg.get("total")
            if took and isinstance(total, int):
                self.account(self.console(sid), took=total, totals=True)
            return
        n = msg.get("n")
        if not isinstance(ch, str) or not ch.startswith("tcp/") or not isinstance(n, int):
            return
        src, _, dst = ch[4:].partition(">")
        if not src or not dst:
            return
        if took:
            self.endpoints[dst] = sid
            writer, reader = self.endpoint_sid(src), sid
        else:
            self.endpoints[src] = sid
            writer, reader = sid, self.endpoint_sid(dst)
        if writer is None or reader is None:
            return
        channel = self.channels.get(ch)
        if channel is None:
            channel = self.channels[ch] = Channel(ch, writer, reader)
        if took:
            self.account(channel, took=n)
            return None
        if msg.get("go") is None:
            self.account(channel, wrote=n)
        return channel

    def account(self, channel, wrote=0, took=0, totals=False):
        """Bytes into or out of a channel; T waits while its reader is behind.

        With `totals` the counts are running totals, and only ever move
        forward; otherwise they are what one call moved (a writer's may be
        negative: what it announced and the call did not take). A station
        that has just caught up with its input is at work on it, so it is
        sent the T it already has (its clock does not move under that work)
        and owes an idle for it, as for any message.
        """
        was = channel.holding()
        if totals:
            channel.written = max(channel.written, wrote)
            channel.taken = max(channel.taken, took)
        else:
            channel.written += wrote
            channel.taken += took
        now = channel.holding()
        if now and not was:
            self.unread += 1
            channel.timer = self.loop.call_later(UNREAD_GRACE_S, self.unread_grace, channel)
        elif was and not now:
            self.unread -= 1
            if channel.timer is not None:
                channel.timer.cancel()
                channel.timer = None
        if not totals and channel.written == channel.taken:
            self.channels.pop(channel.key, None)
        if took and was and not now:
            reader = self.stations.get(channel.reader)
            if reader is not None:
                self.send(channel.reader, {"type": "run", "t": reader.told})
        self.kick()

    def unread_grace(self, channel):
        """A reader that has not taken its input in UNREAD_GRACE_S of wall is
        not waiting for it: T goes on without it."""
        channel.timer = None
        if self.channels.get(channel.key) is not channel or not channel.holding():
            return
        log("%s: %d bytes unread after %.1f s; T goes on" % (
            channel.key, channel.written - channel.taken, UNREAD_GRACE_S))
        channel.taken = channel.written
        self.unread -= 1
        if not channel.key.startswith("tty/"):
            self.channels.pop(channel.key, None)
        self.kick()

    def drop_channel(self, channel):
        if channel.holding():
            self.unread -= 1
        if channel.timer is not None:
            channel.timer.cancel()
            channel.timer = None
        self.channels.pop(channel.key, None)

    # ---- the loss table -------------------------------------------------

    def set_losses(self, tables_by_band, sid_by_name, gains_db_by_sid=None):
        """Replace every loss, name and gain at once.

        `tables_by_band` maps a band name ("433", "868", "915") to its
        slt.Table; `sid_by_name` maps each node name in the tables to the
        station id it runs as; `gains_db_by_sid` maps a station id to its
        antenna gain in dBi, 0 where absent. Everything is swapped in one
        assignment each, between two events, so no frame is ruled on half an
        old table and half a new one. A station id with no name is not in
        the medium: it hears nothing and nothing hears it.
        """
        tables = dict(tables_by_band or {})
        names = {int(sid): name for name, sid in (sid_by_name or {}).items()}
        gains = {int(sid): float(g) for sid, g in (gains_db_by_sid or {}).items()}
        self.tables, self.names, self.gains = tables, names, gains

    def replace_losses(self, band, table):
        """Put in one band's whole table, as it stands, in place of the old one."""
        tables = dict(self.tables)
        if table is None:
            tables.pop(band, None)
        else:
            tables[band] = table
        self.tables = tables

    def update_node(self, name, rows_by_band):
        """One node's row and column recomputed: the old ones stand until now.

        `rows_by_band` maps a band to `(from_db, to_db)`, each a dict of other
        node name to loss in dB at the band's centre: from this node to
        that one, and from that one to this. The band's table is copied, the
        cells written into the copy and the copy put in place, so a reader
        never sees the row half written. A band not named keeps its table.
        """
        tables = dict(self.tables)
        for band, (from_db, to_db) in (rows_by_band or {}).items():
            old = tables.get(band)
            if old is None or name not in old.index:
                continue
            new = slt.Table(old.header, array("f", old.loss), array("B", old.flags),
                            array("H", old.samples))
            for other, loss in (from_db or {}).items():
                if other in new.index and other != name:
                    new.loss[new.cell(name, other)] = loss
            for other, loss in (to_db or {}).items():
                if other in new.index and other != name:
                    new.loss[new.cell(other, name)] = loss
            tables[band] = new
        self.tables = tables

    def set_gain(self, sid, gain_db):
        """One station's antenna gain, in dBi."""
        gains = dict(self.gains)
        gains[int(sid)] = float(gain_db)
        self.gains = gains

    def clear(self):
        """Forget every table, name and gain: nothing hears anything."""
        self.set_losses({}, {}, {})

    def path_loss(self, a, b, freq_hz):
        """The dB from station a to station b at this carrier, or None: never heard."""
        name_a, name_b = self.names.get(a), self.names.get(b)
        if name_a is None or name_b is None or a == b:
            return None
        table = self.tables.get(slt.band_for(freq_hz))
        if table is None or name_a not in table.index or name_b not in table.index:
            return None
        loss = table.at(name_a, name_b, freq_hz)
        return loss if math.isfinite(loss) else None

    def level(self, tx_sid, rx_sid, freq_hz, power_dbm=DEFAULT_POWER_DBM):
        """The level in dBm a frame from `tx_sid` arrives at `rx_sid`, or None."""
        loss = self.path_loss(tx_sid, rx_sid, freq_hz)
        if loss is None:
            return None
        return (float(power_dbm) + self.gains.get(tx_sid, 0.0)
                + self.gains.get(rx_sid, 0.0) - loss)

    def level_of(self, frame, rsid):
        """The level `frame` arrives at `rsid`, fixed on first asking."""
        if rsid not in frame.levels:
            frame.levels[rsid] = self.level(frame.sid, rsid, frame.freq, frame.power_dbm)
        return frame.levels[rsid]

    def noise(self, bw_hz):
        """A receiver's noise floor in dBm for this bandwidth: kTB plus the figure."""
        return (THERMAL_DBM_PER_HZ + 10.0 * math.log10(max(float(bw_hz or 125_000), 1.0))
                + self.physics.noise_figure_db)

    def sensitivity(self, sf):
        """The SNR this spreading factor needs to demodulate, in dB."""
        return SENSITIVITY_DB.get(sf, SLOWEST_SENSITIVITY_DB)

    def audible(self, level, bw_hz, sf=None):
        """True when a frame at this level clears its modem's threshold over noise."""
        if level is None:
            return False
        return level >= self.noise(bw_hz) + self.sensitivity(sf)

    def levels(self, sid, freq_hz, bw_hz=125_000, power_dbm=None, sf=None):
        """What every other station in the table would hear from `sid` on this
        carrier, as sid -> dBm: only those that could decode it.

        The power is the one that station last transmitted at, so the answer
        describes the station as it is rather than as a constant assumed it
        would be. A station that has never transmitted has no such figure and
        is asked about at the default.
        """
        station = self.stations.get(sid)
        if power_dbm is None:
            power_dbm = station.power_dbm if station else DEFAULT_POWER_DBM
        if sf is None and station is not None:
            state = station.state(0)
            sf = state.get("sf") if state else None
        heard = {}
        for other in self.names:
            if other == sid:
                continue
            level = self.level(sid, other, freq_hz, power_dbm)
            if self.audible(level, bw_hz, sf):
                heard[other] = level
        return heard

    # ---- events ---------------------------------------------------------

    def raise_event(self, callback, *args):
        """Hand one event to a subscriber; a broken subscriber is not the medium's
        problem, so it is logged and the frame carries on."""
        if callback is None:
            return
        try:
            callback(*args)
        except Exception as err:                # noqa: BLE001 - see docstring
            log("subscriber raised %r" % (err,))

    # ---- plumbing ------------------------------------------------------

    def connection_made(self, transport):
        self.transport = transport
        sock = transport.get_extra_info("socket")
        if sock is not None:
            with contextlib.suppress(OSError):
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, RECV_BUFFER_BYTES)

    def datagram_received(self, data, addr):
        try:
            msg = json.loads(data.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            self.write_record("in", None, {"raw": repr(data[:200])})
            return
        if not isinstance(msg, dict):
            return
        sid = msg.get("sid")
        kind = msg.get("type")
        if kind not in ("idle", "wrote", "read"):
            self.write_record("in", sid, msg)
        if not isinstance(sid, int):
            return
        if kind == "hello":
            self.recv_hello(sid, addr, msg)
        elif kind == "listen":
            self.recv_listen(sid, msg)
        elif not self.clock.virtual:
            if kind == "state":
                self.recv_state(sid, addr, msg)
            elif kind == "tx":
                self.recv_tx(sid, addr, msg)
        elif kind == "idle":
            self.recv_idle(sid, msg)
        elif kind in ("wrote", "read"):
            self.recv_io(sid, addr, msg)
        elif kind in ("state", "tx"):
            # Held for the barrier, and taken in station order there, so two
            # stations acting at one instant are ruled on the same way every
            # time. Anything a station says means it is not idle.
            station = self.stations.get(sid)
            if station is None:
                return
            station.addr = addr
            self.mark(station, False)
            self.arrivals += 1
            self.pending.append((sid, self.arrivals, addr, msg))
        # Anything else is not ours to understand.

    def send(self, sid, msg):
        """Send one message to a station, at the address it last wrote from.

        In a virtual-time run every message is an instant: it carries T and
        the station's next sequence number, and the station owes an idle for
        that number before T can move again.
        """
        station = self.stations.get(sid)
        if station is None or self.transport is None:
            return
        data = None
        if self.clock.virtual:
            station.seq += 1
            station.granted_at = self.loop.time()
            self.mark(station, False)
            msg = dict(msg, seq=station.seq)
            msg.setdefault("t", self.clock.t)
            station.told = max(station.told, msg["t"])
            data = json.dumps(msg).encode("utf-8")
            station.unanswered.append((station.seq, data))
        if msg.get("type") != "run":
            self.write_record("out", sid, msg)
        self.transport.sendto(data or json.dumps(msg).encode("utf-8"), station.addr)

    def stamp(self):
        """What heads a record line: the wall clock, or T in a virtual run."""
        if self.clock.virtual:
            return "%.6f" % (self.clock.t / 1_000_000.0)
        return wall_stamp()

    def write_record(self, direction, sid, msg):
        if self.record is None:
            return
        self.record.write("%s\t%s\t%s\t%s\n" % (
            self.stamp(), direction, "-" if sid is None else sid,
            json.dumps(msg, separators=(",", ":"), sort_keys=True)))

    # ---- station messages ----------------------------------------------

    def station_for(self, sid, addr, slots=None):
        """The station record for `sid`, created on first sight."""
        station = self.stations.get(sid)
        if station is None:
            station = Station(sid, addr, slots or [0])
            self.stations[sid] = station
            self.busy_count += 1            # not idle until it says so
            log("station %d joined from %s:%d" % (sid, addr[0], addr[1]))
        else:
            station.addr = addr
            if slots:
                station.slots = slots
        return station

    def recv_hello(self, sid, addr, msg):
        slots = msg.get("slots") or [0]
        if self.clock.virtual:
            # A station saying hello again has restarted: whatever it had
            # scheduled and said went with its old process.
            self.forget(sid)
        station = self.station_for(sid, addr, slots)
        self.expected.discard(sid)
        station.until = None
        self.send(sid, {"type": "welcome", "t": self.now(), "mode": self.mode,
                        "rate": self.rate, "epoch": self.epoch, "seed": self.seed})

    def recv_state(self, sid, addr, msg):
        station = self.station_for(sid, addr)
        slot = msg.get("slot", 0)
        station.states[slot] = msg
        if msg.get("mode") != "RX":
            # The chip lets go of what it was demodulating on leaving RX.
            station.locks.pop(slot, None)
        log("station %d slot %s %s freq=%s bw=%s sf=%s sync=%s" % (
            sid, slot, msg.get("mode"), msg.get("freq"), msg.get("bw"),
            msg.get("sf"), msg.get("sync")))
        self.raise_event(self.on_station, sid, msg)

    def recv_tx(self, sid, addr, msg):
        station = self.station_for(sid, addr)
        start = self.now()
        t0 = msg.get("t0", 0)
        if self.clock.virtual and int(t0) < start:
            # The station acted on something from outside the run after it
            # last said it was idle, at the T it had then: the frame goes on
            # the air now.
            log("frame from %d stated t0 %d, %d us before T; sent at T" % (
                sid, int(t0), start - int(t0)))
        span = max(0, min(int(msg.get("t_end", t0)) - int(t0), MAX_FRAME_US))
        pre = max(0, min(int(msg.get("t_pre", t0)) - int(t0), span))
        hdr = max(pre, min(int(msg.get("t_hdr", t0)) - int(t0), span))
        self.next_eid += 1
        frame = Frame(self.next_eid, msg.get("id"), sid, msg, start, start + span,
                      start + pre, start + hdr)

        self.prune(start)
        for other in [f for f in self.frames if f.shares_air(frame)]:
            frame.interferers.append(other)
            other.interferers.append(frame)
            log("frame %d from %d shares air with frame %d from %d (%s/%s Hz)" % (
                frame.eid, sid, other.eid, other.sid, frame.freq, other.freq))
        self.frames.append(frame)
        station.tx_until = frame.end_us
        station.power_dbm = frame.power_dbm
        station.locks.clear()           # half duplex: whatever it followed is gone
        self.raise_event(self.on_tx, sid, frame.eid, frame.freq,
                         frame.start_us, frame.end_us)

        for rsid, rstation in self.stations.items():
            if rsid == sid:
                continue
            if rstation.tx_until > start:
                continue        # half duplex: its own frame is still going out
            level = self.level_of(frame, rsid)
            if level is None:
                continue        # the table says this pair never hears
            for slot in rstation.slots:
                if not rstation.listening(slot):
                    continue
                if self.pairwise:
                    self.arrive_pairwise(frame, msg, rstation, slot, level)
                else:
                    self.arrive(frame, msg, rstation, slot, level)

        log("frame %d (station's %s) from %d: %d us on %s Hz sf%s -> %s" % (
            frame.eid, frame.fid, sid, span, frame.freq, frame.sf,
            ["%d@%.1fdBm" % (r[0], r[2]) for r in frame.receivers] or "nobody"))

    @staticmethod
    def matches(state, tx):
        """True when a receiver's stated radio can decode this transmission."""
        return (all(state.get(k) == tx.get(k) for k in MATCH_KEYS)
                and same_carrier(state.get("freq"), tx.get("freq"), state.get("bw")))

    # ---- arrival: the lock ----------------------------------------------

    def begin_message(self, frame, slot, level, energy=False):
        begin = {"type": "rx_begin", "slot": slot,
                 "id": frame.eid, "t0": frame.start_us,
                 "t_pre": frame.pre_us, "t_hdr": frame.hdr_us,
                 "t_end": frame.end_us, "level": round(level)}
        if energy:
            begin["cad"] = True
        return begin

    def current_lock(self, rstation, slot, now):
        """The reception this slot's demodulator is following at `now`, or None."""
        held = rstation.locks.get(slot)
        if held is not None and held.frame.end_us <= now:
            rstation.locks.pop(slot, None)
            held = None
        return held

    def open_reception(self, frame, rstation, slot, level, lost=False):
        """Schedule the rx_end for one receiver of a decodable frame."""
        reception = Reception(frame, rstation.sid, slot, level, lost)
        frame.receivers.append((rstation.sid, slot, level))
        self.call_at(frame.end_us, self.deliver_end, reception, key=rstation.sid)
        return reception

    def in_band_energy(self, rsid, state, now):
        """The summed level, in dBm, of every transmission on the air at `now`
        whose channel overlaps this receiver's."""
        total = 0.0
        for other in self.frames:
            if not other.start_us <= now < other.end_us or other.sid == rsid:
                continue
            if not in_band(other.freq, other.bw, state.get("freq"), state.get("bw")):
                continue
            level = self.level_of(other, rsid)
            if level is not None:
                total += dbm_to_mw(level)
        return mw_to_dbm(total)

    def arrive(self, frame, msg, rstation, slot, level):
        """A frame reaches one listening slot: the receiver-centred rule.

        Off its band, nothing. Decodable — matching its state and over its
        modem's threshold — a CAD slot is told it is there, and an RX slot
        locks on to it unless it is following a frame this one does not lead
        by the same-SF figure; a frame that takes the lock loses the earlier
        one. Anything else in band is energy: an rx_begin marked `cad`, and no
        end, when the summed energy there crosses the sense threshold.
        """
        state = rstation.state(slot)
        if not in_band(frame.freq, frame.bw, state.get("freq"), state.get("bw")):
            return
        decodable = (self.matches(state, msg)
                     and self.audible(level, frame.bw, frame.sf))
        now = frame.start_us
        if not decodable:
            if self.in_band_energy(rstation.sid, state, now) >= \
                    sense_threshold_dbm(state.get("bw")):
                self.send(rstation.sid, self.begin_message(frame, slot, level, energy=True))
            return
        if rstation.sensing(slot):
            self.send(rstation.sid, self.begin_message(frame, slot, level, energy=True))
            return
        held = self.current_lock(rstation, slot, now)
        if held is not None and level - held.level < SAME_SF_REJECTION_DB:
            # The demodulator is busy with a frame this one does not lead:
            # it is energy to this receiver, and lost to it.
            self.send(rstation.sid, self.begin_message(frame, slot, level, energy=True))
            self.open_reception(frame, rstation, slot, level, lost=True)
            return
        if held is not None:
            held.lost = True
            log("frame %d takes station %d off frame %d" % (
                frame.eid, rstation.sid, held.frame.eid))
        rstation.locks[slot] = self.open_reception(frame, rstation, slot, level)
        self.send(rstation.sid, self.begin_message(frame, slot, level))

    def arrive_pairwise(self, frame, msg, rstation, slot, level):
        """A frame reaches one listening slot: the pairwise rule.

        Delivered where it matches and is audible. A later frame takes the
        receiver only when it leads, in the whole dB the station is shown, the
        one in progress by the capture margin; otherwise it is energy and lost
        to this receiver. A CAD slot is told of every frame it could decode.
        """
        state = rstation.state(slot)
        if not self.matches(state, msg) or not self.audible(level, frame.bw, frame.sf):
            return
        if rstation.sensing(slot):
            self.send(rstation.sid, self.begin_message(frame, slot, level, energy=True))
            return
        held = self.current_lock(rstation, slot, frame.start_us)
        if held is not None and round(level) < round(held.level) + PAIRWISE_CAPTURE_DB:
            self.send(rstation.sid, self.begin_message(frame, slot, level, energy=True))
            self.open_reception(frame, rstation, slot, level, lost=True)
            return
        if held is not None:
            held.lost = True
        rstation.locks[slot] = self.open_reception(frame, rstation, slot, level)
        self.send(rstation.sid, self.begin_message(frame, slot, level))

    # ---- delivery: the verdict ------------------------------------------

    def segments(self, frame, others):
        """The frame's air cut where the set of overlapping transmissions
        changes: a list of the lists of transmissions on the air in each piece."""
        cuts = {frame.start_us, frame.end_us}
        for other in others:
            cuts.update(t for t in (other.start_us, other.end_us)
                        if frame.start_us < t < frame.end_us)
        edges = sorted(cuts)
        pieces = []
        for lo, hi in zip(edges, edges[1:]):
            pieces.append([o for o in others if o.start_us < hi and o.end_us > lo])
        return pieces

    @staticmethod
    def talked_over(reception):
        """True when the receiver itself transmitted during the frame: half
        duplex, so the frame is lost to it whatever else was on the air."""
        return any(other.sid == reception.rsid for other in reception.frame.interferers)

    def verdict_for(self, reception):
        """How this frame ends at one receiver: the receiver-centred rule.

        Over every piece of the frame's air, the worst deciding: its level
        over thermal noise at or above its spreading factor's demodulation
        threshold, and over each class of interference — the transmissions in
        its band at one spreading factor, their powers summed — at or above
        that class's rejection figure.
        """
        frame, rsid, level = reception.frame, reception.rsid, reception.level
        if reception.lost or self.talked_over(reception):
            return "crc"
        if level - self.noise(frame.bw) < self.sensitivity(frame.sf):
            return "crc"
        others = []
        for other in frame.interferers:
            against = self.level_of(other, rsid)
            if against is not None:
                others.append((other, against))
        if not others:
            return "clean"
        by_frame = {id(o): a for o, a in others}
        for piece in self.segments(frame, [o for o, _ in others]):
            classes = {}
            for other in piece:
                key = other.sf
                classes[key] = classes.get(key, 0.0) + dbm_to_mw(by_frame[id(other)])
            for sf, mw in classes.items():
                if level - mw_to_dbm(mw) < rejection_db(frame.sf, sf):
                    return "crc"
        return "clean"

    def verdict_pairwise(self, reception):
        """How this frame ends at one receiver: the pairwise rule.

        Each same-carrier interferer this station could hear is taken on its
        own, and the frame survives only by leading every one of them by the
        capture margin.
        """
        frame, rsid, level = reception.frame, reception.rsid, reception.level
        if reception.lost or self.talked_over(reception):
            return "crc"
        for other in frame.interferers:
            if not same_carrier(frame.freq, other.freq, max(frame.bw or 0, other.bw or 0)):
                continue
            against = self.level_of(other, rsid)
            if not self.audible(against, other.bw, other.sf):
                continue        # this receiver never heard the other frame
            if level - against < PAIRWISE_CAPTURE_DB:
                return "crc"
        return "clean"

    def deliver_end(self, reception):
        """Close out one receiver's reception of a frame, at its stated end."""
        frame, rsid, slot, level = (reception.frame, reception.rsid,
                                    reception.slot, reception.level)
        station = self.stations.get(rsid)
        if station is not None and station.locks.get(slot) is reception:
            station.locks.pop(slot, None)
        verdict = (self.verdict_pairwise(reception) if self.pairwise
                   else self.verdict_for(reception))
        self.send(rsid, {"type": "rx_end", "slot": slot, "id": frame.eid,
                         "t": self.now(), "verdict": verdict,
                         "payload": frame.payload, "rssi": round(level),
                         "snr": round(level - self.noise(frame.bw))})
        log("frame %d from %d -> station %d slot %s: %s at %.1f dBm%s" % (
            frame.eid, frame.sid, rsid, slot, verdict, level,
            " (lost the lock)" if reception.lost else ""))
        self.raise_event(self.on_rx, rsid, frame.sid, frame.eid, verdict, level)

    def prune(self, now):
        """Drop frames whose air is long gone; collisions can no longer touch them."""
        self.frames = [f for f in self.frames if f.end_us > now]

    def close(self):
        if self.pace_timer is not None:
            self.pace_timer.cancel()
        if self.transport is not None:
            self.transport.close()
        if self.record is not None:
            self.record.close()


def parse_bind(text):
    """`host:port` into a tuple, with a bare port allowed."""
    if ":" in text:
        host, _, port = text.rpartition(":")
        return (host or "127.0.0.1", int(port))
    return ("127.0.0.1", int(text))


def read_nodeset(path):
    """A nodeset file's node names, station ids and antenna gains, for a run alone.

    Only what the medium needs: `nodes: {name: {id, antenna: {gain_dbi}}}`.
    Returns (sid_by_name, gains_db_by_sid).
    """
    import yaml                 # only a standalone run reads a file

    with open(path, encoding="utf-8") as handle:
        data = yaml.safe_load(handle) or {}
    sids, gains = {}, {}
    for name, node in (data.get("nodes") or {}).items():
        sid = int(node["id"])
        sids[str(name)] = sid
        gains[sid] = float((node.get("antenna") or {}).get("gain_dbi", 0.0))
    return sids, gains


def read_losses(directory):
    """Every band's table found in `directory`, as `<band>.bin`."""
    tables = {}
    for band in slt.BANDS:
        path = os.path.join(directory, "%s.bin" % band)
        if os.path.exists(path):
            tables[band] = slt.Table.read(path)
    return tables


async def serve(bind, record_path, physics, losses, time_mode="real", pairwise=False):
    loop = asyncio.get_running_loop()
    transport, ether = await loop.create_datagram_endpoint(
        lambda: Ether(record_path, physics, time_mode=time_mode, pairwise=pairwise),
        local_addr=bind)
    tables, sids, gains = losses
    ether.set_losses(tables, sids, gains)
    host, port = transport.get_extra_info("sockname")[:2]
    log("ether listening on %s:%d" % (host, port))
    log("recording to %s" % record_path)
    log("time: %s" % describe_time(ether.mode, ether.rate))
    log("physics: %s; %s rule" % (physics.describe(),
                                  "pairwise" if pairwise else "receiver-centred"))
    log("loss tables: %s; nodes: %s" % (
        ", ".join("%s MHz (%d)" % (b, t.n) for b, t in sorted(tables.items())) or "none",
        ", ".join("%s=%d" % (n, s) for n, s in sorted(sids.items(), key=lambda i: i[1]))
        or "none — nothing can hear anything"))
    stop = loop.create_future()
    try:
        await stop
    finally:
        ether.close()


def main(argv=None):
    ap = argparse.ArgumentParser(description="the virtual ether")
    ap.add_argument("--bind", default="127.0.0.1:7000",
                    help="host:port to listen on (default 127.0.0.1:7000)")
    ap.add_argument("--record", default="record.tsv",
                    help="record file (default record.tsv in the current directory)")
    ap.add_argument("--geodata", metavar="PATH",
                    help="the geodata file the nodeset and tables belong to (named in the log)")
    ap.add_argument("--nodeset", metavar="PATH",
                    help="a nodeset file: node names, station ids and antenna gains")
    ap.add_argument("--losses", metavar="DIR",
                    help="the directory holding the nodeset's <band>.bin loss tables "
                         "(default: none, so nothing is heard)")
    ap.add_argument("--noise-figure", type=float, default=DEFAULT_NOISE_FIGURE_DB,
                    help="receiver noise figure in dB (default %g)" % DEFAULT_NOISE_FIGURE_DB)
    ap.add_argument("--pairwise", action="store_true",
                    help="rule on collisions pairwise, per interferer by the capture "
                         "margin, instead of on the summed interference")
    ap.add_argument("--time", default="real",
                    help="real (default), max, or <k>x: virtual time as fast as the "
                         "stations allow, or paced at k times the wall clock")
    args = ap.parse_args(argv)
    try:
        parse_time_mode(args.time)
    except ValueError as err:
        ap.error(str(err))
    if bool(args.nodeset) != bool(args.losses):
        ap.error("--nodeset and --losses go together")
    sids, gains, tables = {}, {}, {}
    if args.nodeset:
        sids, gains = read_nodeset(args.nodeset)
        tables = read_losses(args.losses)
        if args.geodata:
            log("geodata: %s" % args.geodata)
    try:
        asyncio.run(serve(parse_bind(args.bind), args.record,
                          Physics(args.noise_figure), (tables, sids, gains),
                          args.time, args.pairwise))
    except KeyboardInterrupt:
        log("ether stopping")
    return 0


if __name__ == "__main__":
    sys.exit(main())
