"""Fake stations on real UDP sockets, against the ether as a child process,
and the reception model's arithmetic against an ether held in-process.

Stations are linked rather than placed: every test states the path loss of
each pair it needs, in dB, into a loss table written to the test's directory
(slt.py), and the levels asserted below are what those losses give at 14 dBm.
A pair a test does not link is never heard, which is how a hidden terminal is
made here.
"""

import asyncio
import base64
import json
import math
import os
import queue
import re
import socket
import subprocess
import sys
import threading
import time

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
ETHER = os.path.join(HERE, "ether.py")
sys.path.insert(0, HERE)

import ether as ether_module     # noqa: E402 - the path is set just above
import slt                       # noqa: E402

FREQ = 868_100_000
BW = 125_000
SF = 9
SYNC = 0x34
POWER_DBM = 14

# A frame long enough that begin and end are plainly separate events, short
# enough that the tests stay quick.
FRAME_US = 300_000

# Losses that keep the arithmetic legible: at 14 dBm a 100 dB pair arrives at
# −86 dBm and a 127 dB one at −113.
NEAR_DB = 100.0
FAR_DB = 127.0

# The receiver's floor at 125 kHz and the default 6 dB noise figure.
NOISE_DBM = -174.0 + 10.0 * math.log10(BW) + 6.0       # −117.03


def level_for(loss_db, gain_db=0.0, power_dbm=POWER_DBM, freq=FREQ, f0=868_000_000):
    """The level the ether will compute for this loss, in dBm."""
    return power_dbm + gain_db - loss_db - 20.0 * math.log10(freq / f0)


def expected_snr(level):
    """What the ether reports as SNR: the level above the receiver's noise."""
    return round(level - NOISE_DBM)


def radio(mode="RX", **over):
    """The radio fields shared by a state and a transmission."""
    fields = {"mode": mode, "ready_at": 0, "freq": FREQ, "bw": BW, "sf": SF,
              "cr": 5, "sync": SYNC, "hdr": "explicit", "crc": True, "pre": 8}
    fields.update(over)
    return fields


def name(sid):
    return "n%d" % sid


def make_table(band, sids, losses):
    """A loss table for one band over these stations; `losses` maps an ordered
    pair of station ids to dB, and every other pair is never heard."""
    header = {"geodata": "test", "band": band, "f0_hz": slt.f0_of(band),
              "model": "log-distance",
              "nodes": [{"name": name(s), "id": s} for s in sorted(sids)]}
    table = slt.Table(header)
    for (a, b), db in losses.items():
        table.put(name(a), name(b), db)
    return table


class FakeStation:
    """One station's UDP socket, its clock and its inbox.

    The ether's address is resolved on the first datagram rather than at
    construction, because the ether does not exist until every link in the
    test has been stated — see `Bench`.
    """

    def __init__(self, sid, bench):
        self.sid = sid
        self.bench = bench
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.t = sid * 1_000_000      # each station's own clock, its own origin

    @property
    def ether(self):
        return ("127.0.0.1", self.bench.ensure_started())

    def close(self):
        self.sock.close()

    def send(self, msg):
        msg = dict(msg, sid=self.sid, t=self.t)
        self.sock.sendto(json.dumps(msg).encode(), self.ether)

    def hello(self):
        self.send({"type": "hello", "slots": [0]})
        return self.expect("welcome")

    def state(self, mode="RX", **over):
        self.send(dict({"type": "state", "slot": 0}, **radio(mode, **over)))

    def tx(self, fid, payload=b"hello", span_us=FRAME_US, **over):
        t0 = self.t
        self.send(dict({"type": "tx", "slot": 0, "id": fid, "t0": t0,
                        "t_pre": t0 + span_us // 10,
                        "t_hdr": t0 + span_us // 5,
                        "t_end": t0 + span_us, "power_dbm": POWER_DBM,
                        "payload": base64.b64encode(payload).decode()},
                       **radio("TX", **over)))

    def recv(self, timeout=2.0):
        self.sock.settimeout(timeout)
        try:
            data, _ = self.sock.recvfrom(65535)
        except socket.timeout:
            return None
        return json.loads(data.decode())

    def expect(self, kind, timeout=2.0):
        """The next message of this type, ignoring anything else."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            msg = self.recv(max(0.05, deadline - time.monotonic()))
            if msg is None:
                break
            if msg.get("type") == kind:
                return msg
        raise AssertionError("station %d never saw %s" % (self.sid, kind))

    def expect_nothing(self, timeout=0.5):
        assert self.recv(timeout) is None

    def ends(self, count, timeout=2.0):
        """The next `count` receptions to close, keyed by the bytes they carry.

        A frame's id belongs to the ether, not to the station that sent it, so
        a test that has two frames in the air tells them apart by what is in
        them.
        """
        closed = {}
        for _ in range(count):
            end = self.expect("rx_end", timeout=timeout)
            closed[base64.b64decode(end["payload"])] = end
        return closed


class Bench:
    """The ether as a child process, its nodeset and loss table, and the
    stations that talk to it.

    Calling it makes a station: `ether(1)` is station 1, node `n1`, in the
    nodeset. `link(a, b, db)` states a pair's loss, both ways. The ether starts
    on the first datagram any station sends, so every link a test wants is
    stated before then — the table is written once, as a run's is.
    """

    def __init__(self, tmp_path, time_mode="real", pairwise=False):
        self.tmp_path = tmp_path
        self.time_mode = time_mode
        self.pairwise = pairwise
        self.record = tmp_path / "record.tsv"
        self.proc = None
        self.port = None
        self.stations = []
        self.gains = {}             # sid -> dBi, for every node in the nodeset
        self.losses = {}            # (sid, sid) -> dB at the 868 band's centre

    def link(self, a, b, db, back=None):
        assert self.proc is None, "the ether is already running"
        self.gains.setdefault(a, 0.0)
        self.gains.setdefault(b, 0.0)
        self.losses[(a, b)] = float(db)
        self.losses[(b, a)] = float(db if back is None else back)

    def write_files(self):
        """The nodeset file and the 868 MHz table, which is how the ether reads them."""
        nodes = ["nodes:"]
        for sid, gain in sorted(self.gains.items()):
            nodes.append("  %s: { id: %d, antenna: { gain_dbi: %g } }" % (name(sid), sid, gain))
        nodeset_path = self.tmp_path / "nodeset.yaml"
        nodeset_path.write_text("\n".join(nodes) + "\n")
        losses = self.tmp_path / "losses"
        losses.mkdir(exist_ok=True)
        make_table("868", self.gains, self.losses).write(str(losses / "868.bin"))
        return nodeset_path, losses

    def ensure_started(self):
        """The ether's port, starting it on the first station to speak."""
        if self.proc is None:
            self.start()
        return self.port

    def start(self):
        argv = [sys.executable, ETHER, "--bind", "127.0.0.1:0",
                "--record", str(self.record), "--time", self.time_mode]
        if self.gains:
            nodes, losses = self.write_files()
            argv += ["--nodeset", str(nodes), "--losses", str(losses)]
        if self.pairwise:
            argv.append("--pairwise")
        self.proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.PIPE, text=True)
        lines = queue.Queue()

        def drain():
            for line in self.proc.stderr:
                lines.put(line)
            lines.put(None)

        threading.Thread(target=drain, daemon=True).start()

        deadline = time.monotonic() + 10
        while self.port is None and time.monotonic() < deadline:
            line = lines.get(timeout=10)
            if line is None:
                break
            found = re.search(r"listening on 127\.0\.0\.1:(\d+)", line)
            if found:
                self.port = int(found.group(1))
        assert self.port, "the ether never reported a listening port"

    def __call__(self, sid, gain_db=0.0):
        assert self.proc is None or sid in self.gains
        self.gains[sid] = float(gain_db)
        station = FakeStation(sid, self)
        self.stations.append(station)
        return station

    def close(self):
        for station in self.stations:
            station.close()
        if self.proc is not None:
            self.proc.terminate()
            self.proc.wait(timeout=5)


@pytest.fixture
def ether(tmp_path):
    """The ether on an ephemeral port, its record in the test's directory."""
    bed = Bench(tmp_path)
    try:
        yield bed
    finally:
        bed.close()


@pytest.fixture
def pairwise(tmp_path):
    """The ether under the pairwise rule."""
    bed = Bench(tmp_path, pairwise=True)
    try:
        yield bed
    finally:
        bed.close()


@pytest.fixture
def conductor(tmp_path):
    """The ether in virtual time, as fast as its stations let it go."""
    bed = Bench(tmp_path, "max")
    try:
        yield bed
    finally:
        bed.close()


def listen(*stations, **over):
    for station in stations:
        station.hello()
    for station in stations:
        station.state("RX", **over)
    time.sleep(0.1)


def test_hello_gets_a_welcome(ether):
    welcome = ether(1).hello()
    assert welcome["mode"] == "real"
    assert welcome["rate"] == 1
    assert isinstance(welcome["t"], int)
    assert isinstance(welcome["epoch"], int)
    assert isinstance(welcome["seed"], int)
    assert "seq" not in welcome


def test_frame_reaches_a_listening_station(ether):
    ether.link(1, 2, NEAR_DB)
    sender, receiver = ether(1), ether(2)
    sender.hello()
    listen(receiver)

    level = level_for(NEAR_DB)
    sent_at = time.monotonic()
    sender.tx(7, payload=b"over the air")

    begin = receiver.expect("rx_begin")
    assert time.monotonic() - sent_at < 0.2, "rx_begin must arrive at once"
    assert begin["slot"] == 0
    assert "cad" not in begin
    assert begin["level"] == round(level)
    assert begin["t_end"] - begin["t0"] == FRAME_US
    assert begin["t_pre"] - begin["t0"] == FRAME_US // 10
    assert begin["t_hdr"] - begin["t0"] == FRAME_US // 5

    end = receiver.expect("rx_end")
    elapsed = time.monotonic() - sent_at
    assert FRAME_US / 1e6 - 0.05 < elapsed < FRAME_US / 1e6 + 0.3, elapsed
    assert end["id"] == begin["id"], "one reception, one id from beginning to end"
    assert end["verdict"] == "clean"
    assert base64.b64decode(end["payload"]) == b"over the air"
    assert end["rssi"] == round(level)
    assert end["snr"] == expected_snr(level)

    sender.expect_nothing()      # a transmitter never hears itself


def test_the_level_is_the_tables_loss(ether):
    ether.link(1, 2, NEAR_DB)
    ether.link(1, 3, FAR_DB)
    sender, near, far = ether(1), ether(2), ether(3)
    sender.hello()
    listen(near, far)

    sender.tx(1)
    assert near.expect("rx_begin")["level"] == round(level_for(NEAR_DB))
    assert far.expect("rx_begin")["level"] == round(level_for(FAR_DB))


def test_a_table_is_read_per_direction(ether):
    """A measured table need not be symmetric, and each frame reads its own
    direction."""
    ether.link(1, 2, NEAR_DB, back=NEAR_DB + 10)
    a, b = ether(1), ether(2)
    listen(a, b)
    a.tx(1)
    assert b.expect("rx_begin")["level"] == round(level_for(NEAR_DB))
    b.expect("rx_end")
    b.tx(2)
    assert a.expect("rx_begin")["level"] == round(level_for(NEAR_DB + 10))


def test_antenna_gain_lifts_both_ends(ether):
    """Gain is the receiver's as much as the transmitter's: the link has both."""
    ether.link(1, 2, FAR_DB)
    ether.link(1, 3, FAR_DB)
    sender, plain, tall = ether(1, gain_db=2), ether(2), ether(3, gain_db=9)
    sender.hello()
    listen(plain, tall)

    sender.tx(1)
    assert plain.expect("rx_begin")["level"] == round(level_for(FAR_DB, gain_db=2))
    assert tall.expect("rx_begin")["level"] == round(level_for(FAR_DB, gain_db=11))


def test_a_pair_the_table_does_not_have_is_never_heard(ether):
    ether.link(1, 2, NEAR_DB)
    ether.link(1, 3, math.inf)
    sender, heard, never, absent = ether(1), ether(2), ether(3), ether(4)
    sender.hello()
    listen(heard, never, absent)

    sender.tx(1)
    heard.expect("rx_begin")
    never.expect_nothing()
    absent.expect_nothing()


def test_a_frame_under_its_spreading_factors_threshold_is_not_delivered(ether):
    """At SF9 a frame must clear the −117.03 dBm floor by −12.5 dB: −129.53
    dBm. At 14 dBm that is a loss of 143.53 dB; 144 is out of range and 143
    in it."""
    ether.link(1, 2, 143.0)
    ether.link(1, 3, 144.0)
    sender, inside, outside = ether(1), ether(2), ether(3)
    sender.hello()
    listen(inside, outside)

    sender.tx(1)
    assert inside.expect("rx_end")["verdict"] == "clean"
    outside.expect_nothing()


def test_a_station_the_nodeset_does_not_name_hears_nothing(ether):
    ether.link(1, 2, NEAR_DB)
    sender = ether(1)
    ether(2)
    stray = FakeStation(9, ether)        # an id no nodeset names
    ether.stations.append(stray)
    sender.hello()
    listen(stray)

    sender.tx(121)
    stray.expect_nothing()


def test_the_sender_is_not_a_receiver_and_others_must_match(ether):
    for sid in (2, 3, 4):
        ether.link(1, sid, NEAR_DB)
    sender, same, other_freq, sleeping = ether(1), ether(2), ether(3), ether(4)
    for station in (sender, same, other_freq, sleeping):
        station.hello()
    same.state("RX")
    other_freq.state("RX", freq=869_500_000)
    sleeping.state("SLEEP")
    time.sleep(0.1)

    sender.tx(11, payload=b"x")
    assert base64.b64decode(same.expect("rx_end")["payload"]) == b"x"
    other_freq.expect_nothing()
    sleeping.expect_nothing()


def test_wrong_sync_word_is_not_decoded(ether):
    """A weak frame at another sync word is neither decoded nor loud enough
    to be energy."""
    ether.link(1, 2, FAR_DB)
    sender, receiver = ether(1), ether(2)
    sender.hello()
    listen(receiver, sync=0x12)

    sender.tx(12)
    receiver.expect_nothing()


def test_a_carrier_a_few_register_steps_off_is_the_same_carrier(ether):
    """Two drivers round one frequency to registers tens of hertz apart; a
    receiver decodes within a quarter of its bandwidth and not beyond."""
    ether.link(1, 2, FAR_DB)
    ether.link(1, 3, FAR_DB)
    sender, near_miss, other = ether(1), ether(2), ether(3)
    sender.hello()
    near_miss.hello()
    other.hello()
    near_miss.state("RX", freq=FREQ + 36)
    other.state("RX", freq=FREQ + BW // 4 + 1000)
    time.sleep(0.1)

    sender.tx(13)
    near_miss.expect("rx_begin")
    assert near_miss.expect("rx_end")["verdict"] == "clean"
    other.expect_nothing()


def test_overlapping_frames_of_equal_level_both_end_as_crc(ether):
    ether.link(1, 3, FAR_DB - 10)
    ether.link(2, 3, FAR_DB - 10)
    first, second, receiver = ether(1), ether(2), ether(3)
    first.hello()
    second.hello()
    listen(receiver)

    first.tx(21, payload=b"first")
    assert "cad" not in receiver.expect("rx_begin")
    time.sleep(FRAME_US / 2e6)          # squarely inside the first frame
    second.tx(22, payload=b"second")
    assert receiver.expect("rx_begin")["cad"] is True, "the receiver is busy"

    ends = receiver.ends(2)
    assert set(ends) == {b"first", b"second"}
    assert ends[b"first"]["verdict"] == "crc"
    assert ends[b"second"]["verdict"] == "crc"


def test_frames_that_do_not_overlap_stay_clean(ether):
    ether.link(1, 2, NEAR_DB)
    sender, receiver = ether(1), ether(2)
    sender.hello()
    listen(receiver)

    sender.tx(31, span_us=100_000)
    receiver.expect("rx_begin")
    assert receiver.expect("rx_end")["verdict"] == "clean"
    sender.tx(32, span_us=100_000)
    receiver.expect("rx_begin")
    assert receiver.expect("rx_end")["verdict"] == "clean"


def test_leaving_rx_before_a_frame_means_it_is_not_heard(ether):
    ether.link(1, 2, NEAR_DB)
    sender, receiver = ether(1), ether(2)
    sender.hello()
    receiver.hello()
    receiver.state("RX")
    time.sleep(0.05)
    receiver.state("STDBY_RC")
    time.sleep(0.05)

    sender.tx(41)
    receiver.expect_nothing()


def test_a_station_in_cad_senses_a_frame_but_is_never_told_how_it_ended(ether):
    """CAD is energy, not reception: an rx_begin marked `cad`, and no rx_end,
    so nothing is ruled on and nothing is recorded as received."""
    ether.link(1, 2, FAR_DB)
    sender, sensing = ether(1), ether(2)
    sender.hello()
    sensing.hello()
    sensing.state("CAD")
    time.sleep(0.05)

    sender.tx(43, payload=b"is anyone there")
    begin = sensing.expect("rx_begin")
    assert begin["cad"] is True
    assert begin["level"] == round(level_for(FAR_DB))
    assert begin["t_end"] - begin["t0"] == FRAME_US
    sensing.expect_nothing(timeout=FRAME_US / 1e6 + 0.3)

    time.sleep(0.2)             # the record is written as the ether goes
    ends = [line for line in ether.record.read_text().splitlines()
            if '"type":"rx_end"' in line]
    assert ends == []


def test_carrier_sense_hears_energy_over_the_threshold_at_any_sf(ether):
    """At 125 kHz the sense threshold is −81 dBm. An SF7 frame at −76 dBm is
    energy to a CAD and to a receiver at SF9; one at −96 dBm is nothing to
    either, and neither is ever decoded."""
    ether.link(1, 3, 90.0)
    ether.link(1, 4, 90.0)
    ether.link(2, 3, 110.0)
    ether.link(2, 4, 110.0)
    loud, quiet, sensing, receiving = ether(1), ether(2), ether(3), ether(4)
    loud.hello()
    quiet.hello()
    sensing.hello()
    receiving.hello()
    sensing.state("CAD")
    receiving.state("RX")
    time.sleep(0.1)

    loud.tx(1, sf=7)
    for station in (sensing, receiving):
        begin = station.expect("rx_begin")
        assert begin["cad"] is True
        assert begin["level"] == round(level_for(90.0))
    time.sleep(FRAME_US / 1e6 + 0.1)
    quiet.tx(2, sf=7)
    sensing.expect_nothing()
    receiving.expect_nothing()


def test_a_station_is_deaf_while_its_own_frame_is_going_out(ether):
    """Half duplex: a radio transmitting hears nothing, however loud."""
    ether.link(1, 2, NEAR_DB)
    a, b = ether(1), ether(2)
    listen(a, b)

    b.tx(61, payload=b"b is talking")
    a.expect("rx_begin")
    a.tx(62, payload=b"and so is a")
    b.expect_nothing(timeout=0.2)
    assert a.expect("rx_end")["verdict"] == "crc", "a talked over the frame it heard"


def test_frames_that_share_a_station_id_are_still_told_apart(ether):
    """Each station numbers its own frames, so a receiver cannot use that
    number: the id in a reception is the ether's, and no two frames share it."""
    ether.link(1, 2, NEAR_DB)
    ether.link(3, 2, FAR_DB)
    a, b, c = ether(1), ether(2), ether(3)
    listen(a, b, c)

    a.tx(1, payload=b"from a")
    c.tx(1, payload=b"from c")
    first, second = b.expect("rx_begin"), b.expect("rx_begin")
    assert first["id"] != second["id"]


def test_the_louder_of_two_hidden_frames_keeps_the_receiver(ether):
    """The hidden terminal: a and c cannot hear each other, so both transmit.
    a arrives 8 dB over c and first: b keeps a's frame, and c's is energy."""
    ether.link(1, 2, 110.0)
    ether.link(3, 2, 118.0)
    a, b, c = ether(1), ether(2), ether(3)
    listen(a, b, c)

    a.tx(91, payload=b"from a")
    assert "cad" not in b.expect("rx_begin")
    c.tx(92, payload=b"from c")
    assert b.expect("rx_begin")["cad"] is True

    ends = b.ends(2)
    assert ends[b"from a"]["verdict"] == "clean", "a leads c by more than 6 dB"
    assert ends[b"from c"]["verdict"] == "crc"
    a.expect_nothing()
    c.expect_nothing()


def test_a_louder_frame_takes_the_receiver_at_its_preamble(ether):
    """The quieter frame starts first and has the receiver; the louder one
    arrives 8 dB up, takes it, and the first is lost there."""
    ether.link(1, 2, 118.0)
    ether.link(3, 2, 110.0)
    quiet, b, loud = ether(1), ether(2), ether(3)
    listen(quiet, b, loud)

    quiet.tx(1, payload=b"quiet")
    assert "cad" not in b.expect("rx_begin")
    time.sleep(0.03)
    loud.tx(2, payload=b"loud")
    assert "cad" not in b.expect("rx_begin"), "the louder frame takes the lock"

    ends = b.ends(2)
    assert ends[b"quiet"]["verdict"] == "crc"
    assert ends[b"loud"]["verdict"] == "clean"


def test_two_frames_within_the_same_sf_figure_spoil_each_other(ether):
    ether.link(1, 2, 110.0)
    ether.link(3, 2, 114.0)
    a, b, c = ether(1), ether(2), ether(3)
    listen(a, b, c)

    a.tx(101, payload=b"from a")
    c.tx(102, payload=b"from c")
    ends = b.ends(2)
    assert ends[b"from a"]["verdict"] == "crc"
    assert ends[b"from c"]["verdict"] == "crc"


def test_a_receiver_that_hears_only_one_of_two_colliding_frames_keeps_it(ether):
    """The collision is at b; the table gives d no path from a."""
    ether.link(1, 2, 110.0)
    ether.link(3, 2, 110.0)
    ether.link(3, 4, 110.0)
    a, b, c, d = ether(1), ether(2), ether(3), ether(4)
    listen(a, b, c, d)

    a.tx(111, payload=b"from a")
    c.tx(112, payload=b"from c")

    ends = b.ends(2)             # equal from both, so it keeps neither
    assert ends[b"from a"]["verdict"] == "crc"
    assert ends[b"from c"]["verdict"] == "crc"

    end = d.expect("rx_end")     # d heard only c, and cleanly
    assert base64.b64decode(end["payload"]) == b"from c"
    assert end["verdict"] == "clean"


def summed_interference(bench):
    """A frame at −96 dBm and two same-SF interferers at −104 dBm each,
    hidden from one another. Each alone is 8 dB under it; summed they are
    −100.99 dBm, 4.99 dB under it, which is short of the 6 dB figure."""
    bench.link(1, 4, 110.0)
    bench.link(2, 4, 118.0)
    bench.link(3, 4, 118.0)
    s, i1, i2, rx = bench(1), bench(2), bench(3), bench(4)
    listen(s, i1, i2, rx)
    s.tx(1, payload=b"signal")
    time.sleep(0.02)
    i1.tx(2, payload=b"one")
    i2.tx(3, payload=b"two")
    return rx.ends(3)


def test_interference_is_summed_within_a_class(ether):
    ends = summed_interference(ether)
    assert ends[b"signal"]["verdict"] == "crc"


def test_the_pairwise_rule_takes_each_interferer_alone(pairwise):
    ends = summed_interference(pairwise)
    assert ends[b"signal"]["verdict"] == "clean"


# ---------------------------------------------------------------------------
# The hand-built case: two hidden senders, one receiver
# ---------------------------------------------------------------------------
#
# A (station 1) and C (station 3) cannot hear each other; B (station 2)
# listens at SF9, 125 kHz. Both transmit at 14 dBm; C goes first and A 30 ms
# later, so their air overlaps for all but the ends. With the figures table:
#
#   noise at B     N = −174 + 10·log10(125 000) + 6          = −117.03 dBm
#   SF9 threshold      SENSITIVITY_DB[9]                      = −12.5 dB
#   same-SF figure     SAME_SF_REJECTION_DB                   = 6 dB
#   SF9 over SF7       INTER_SF_REJECTION_DB[9][7]            = −15 dB
#   sense threshold    10·log10(125) − 117 + 15               = −81.03 dBm
#
# Same SF (both at SF9). Loss A→B 120 dB, C→B 127 dB:
#   S_A = 14 − 120 = −106 dBm, SNR 11.03 ≥ −12.5: decodable.
#   S_C = 14 − 127 = −113 dBm, SNR 4.03 ≥ −12.5: decodable, and B locks on.
#   A's preamble: S_A − S_C = 7 ≥ 6, so A takes B off C: C is crc.
#   A's air, first piece (C on the air): S_A − S_C = 7 ≥ 6; second piece
#   (C gone): no interference. A is clean.
#
# Different SF (A at SF9, C at SF7). Loss A→B 120 dB, C→B 108 dB:
#   S_C = −94 dBm: not decodable at B (SF7 ≠ SF9), and under −81.03 dBm, so
#   B is told nothing of it.
#   S_A = −106 dBm, SNR 11.03 ≥ −12.5; against the SF7 class
#   S_A − S_C = −12 ≥ −15. A is clean.
#   With C→B 100 dB instead, S_C = −86: −20 < −15, and A is crc.
#   Under the pairwise rule the 108 dB case is crc too: C is audible at its
#   own SF (−94 ≥ −117.03 − 7.5) and A leads it by −12 < 6.

def test_the_figures_the_hand_calculation_uses():
    assert ether_module.SENSITIVITY_DB[9] == -12.5
    assert ether_module.SAME_SF_REJECTION_DB == 6.0
    assert ether_module.INTER_SF_REJECTION_DB[9][7] == -15
    assert ether_module.rejection_db(9, 7) == -15.0
    assert ether_module.rejection_db(9, 9) == 6.0
    assert ether_module.sense_threshold_dbm(BW) == pytest.approx(-81.03, abs=0.01)
    assert NOISE_DBM == pytest.approx(-117.03, abs=0.01)


def hidden_pair(bench, loss_c, sf_c):
    bench.link(1, 2, 120.0)
    bench.link(3, 2, loss_c)
    a, b, c = bench(1), bench(2), bench(3)
    listen(a, b, c)
    c.tx(1, payload=b"C", sf=sf_c)
    time.sleep(0.03)
    a.tx(2, payload=b"A")
    return b


def test_hand_case_same_sf(ether):
    b = hidden_pair(ether, 127.0, SF)
    begins = [b.expect("rx_begin"), b.expect("rx_begin")]
    assert [m.get("cad") for m in begins] == [None, None]
    assert [m["level"] for m in begins] == [-113, -106]
    ends = b.ends(2)
    assert ends[b"C"]["verdict"] == "crc"
    assert ends[b"A"]["verdict"] == "clean"


@pytest.mark.parametrize("loss_c, verdict", [(108.0, "clean"), (100.0, "crc")])
def test_hand_case_different_sf(ether, loss_c, verdict):
    b = hidden_pair(ether, loss_c, 7)
    begin = b.expect("rx_begin")
    assert (begin["level"], begin.get("cad")) == (-106, None), "C is nothing to B"
    end = b.expect("rx_end")
    assert base64.b64decode(end["payload"]) == b"A"
    assert end["verdict"] == verdict
    b.expect_nothing(timeout=0.2)


def test_hand_case_different_sf_under_the_pairwise_rule(pairwise):
    b = hidden_pair(pairwise, 108.0, 7)
    end = b.expect("rx_end")
    assert base64.b64decode(end["payload"]) == b"A"
    assert end["verdict"] == "crc"


# ---------------------------------------------------------------------------
# The medium in-process: tables, and the verdict's pieces
# ---------------------------------------------------------------------------

@pytest.fixture
def medium():
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    try:
        yield ether_module.Ether(None)
    finally:
        asyncio.set_event_loop(None)
        loop.close()


def test_level_reads_the_band_table_with_the_within_band_correction(medium):
    t433 = make_table("433", [1, 2], {(1, 2): 100.0})
    t868 = make_table("868", [1, 2], {(1, 2): 110.0})
    medium.set_losses({"433": t433, "868": t868}, {"n1": 1, "n2": 2}, {1: 3.0})
    assert medium.level(1, 2, 433_920_000, 14) == pytest.approx(-83.0)
    assert medium.level(1, 2, 520_000_000, 14) == pytest.approx(
        -83.0 - 20 * math.log10(520e6 / 433.92e6))
    assert medium.level(1, 2, 868_000_000, 14) == pytest.approx(-93.0)
    assert medium.level(1, 2, 915_000_000, 14) is None, "no table for the band"
    assert medium.level(2, 1, 868_000_000, 14) is None, "the pair is one way"
    assert medium.level(1, 9, 868_000_000, 14) is None, "no such node"


def test_a_node_row_update_takes_effect_whole(medium):
    table = make_table("868", [1, 2, 3], {(1, 2): 100.0, (2, 1): 100.0, (1, 3): 120.0})
    medium.set_losses({"868": table}, {"n1": 1, "n2": 2, "n3": 3})
    medium.update_node("n1", {"868": ({"n2": 90.0, "n3": math.inf}, {"n2": 95.0})})
    assert medium.level(1, 2, 868e6, 14) == pytest.approx(-76.0)
    assert medium.level(2, 1, 868e6, 14) == pytest.approx(-81.0)
    assert medium.level(1, 3, 868e6, 14) is None
    assert table.get("n1", "n2") == 100.0, "the table handed in is never written"


def test_levels_lists_only_the_stations_that_could_decode(medium):
    table = make_table("868", [1, 2, 3], {(1, 2): 100.0, (1, 3): 150.0})
    medium.set_losses({"868": table}, {"n1": 1, "n2": 2, "n3": 3})
    heard = medium.levels(1, 868e6, sf=9)
    assert list(heard) == [2]
    assert heard[2] == pytest.approx(-86.0)


def frame(medium, sid, start, end, sf=SF, eid=[0]):
    eid[0] += 1
    msg = dict(radio("TX", sf=sf), power_dbm=POWER_DBM)
    return ether_module.Frame(eid[0], eid[0], sid, msg, start, end, start, start)


def verdict(medium, signal, others, rsid=9):
    for other in others:
        signal.interferers.append(other)
    level = medium.level_of(signal, rsid)
    return medium.verdict_for(ether_module.Reception(signal, rsid, 0, level))


def test_the_worst_piece_decides_and_only_overlap_is_summed(medium):
    """Two interferers 8 dB under the frame: one in each half of it, they are
    never on the air together and it survives; overlapping, their sum is
    4.99 dB under it and it does not."""
    table = make_table("868", [1, 2, 3, 9],
                       {(1, 9): 110.0, (2, 9): 118.0, (3, 9): 118.0})
    medium.set_losses({"868": table}, {"n1": 1, "n2": 2, "n3": 3, "n9": 9})
    apart = verdict(medium, frame(medium, 1, 0, 1000),
                    [frame(medium, 2, 100, 400), frame(medium, 3, 600, 900)])
    assert apart == "clean"
    together = verdict(medium, frame(medium, 1, 0, 1000),
                       [frame(medium, 2, 100, 600), frame(medium, 3, 500, 900)])
    assert together == "crc"


def test_each_sf_is_its_own_class(medium):
    """An SF7 and an SF8 interferer, each within its own figure of an SF9
    frame, do not add to each other."""
    table = make_table("868", [1, 2, 3, 9],
                       {(1, 9): 120.0, (2, 9): 107.0, (3, 9): 109.0})
    medium.set_losses({"868": table}, {"n1": 1, "n2": 2, "n3": 3, "n9": 9})
    # SF7 at −93: −106 − (−93) = −13 ≥ −15. SF8 at −95: −11 ≥ −13.
    got = verdict(medium, frame(medium, 1, 0, 1000),
                  [frame(medium, 2, 0, 1000, sf=7), frame(medium, 3, 0, 1000, sf=8)])
    assert got == "clean"


def test_an_off_band_transmission_is_no_interference(medium):
    table = make_table("868", [1, 2, 9], {(1, 9): 120.0, (2, 9): 80.0})
    medium.set_losses({"868": table}, {"n1": 1, "n2": 2, "n9": 9})
    signal = frame(medium, 1, 0, 1000)
    other = frame(medium, 2, 0, 1000)
    other.freq = FREQ + BW
    assert not signal.shares_air(other)
    assert verdict(medium, signal, []) == "clean"


def test_the_unknown_is_ignored_and_the_record_holds_every_message(ether):
    ether.link(1, 2, NEAR_DB)
    sender, receiver = ether(1), ether(2)
    sender.hello()
    listen(receiver)
    sender.send({"type": "wait", "until": 5})
    sender.sock.sendto(b"not json at all", sender.ether)
    time.sleep(0.1)
    sender.tx(61, payload=b"recorded")
    receiver.expect("rx_begin")
    receiver.expect("rx_end")
    time.sleep(0.2)

    lines = [l for l in ether.record.read_text().splitlines() if not l.startswith("#")]
    rows = [l.split("\t") for l in lines]
    assert all(len(r) == 4 for r in rows)
    kinds = [(r[1], r[2], json.loads(r[3]).get("type")) for r in rows]
    assert ("in", "-", None) in kinds, "what is not JSON is recorded raw"
    assert ("in", "1", "hello") in kinds
    assert ("out", "1", "welcome") in kinds
    assert ("in", "2", "state") in kinds
    assert ("in", "1", "tx") in kinds
    assert ("out", "2", "rx_begin") in kinds
    assert ("out", "2", "rx_end") in kinds


# ---------------------------------------------------------------------------
# Virtual time: the ether as the conductor
# ---------------------------------------------------------------------------

def idle(station, seq, until):
    station.send({"type": "idle", "seq": seq, "until": until})


def join_virtual(station):
    welcome = station.hello()
    assert welcome["mode"] == "virtual"
    assert welcome["rate"] is None
    return welcome


def test_virtual_welcome_states_the_mode_and_t(conductor):
    welcome = join_virtual(conductor(1))
    assert welcome["t"] == 0
    assert welcome["seq"] == 1
    assert isinstance(welcome["epoch"], int)


def test_the_barrier_holds_until_every_station_is_idle(conductor):
    a, b = conductor(1), conductor(2)
    join_virtual(a)
    join_virtual(b)
    idle(a, 1, 10_000)
    a.expect_nothing(0.3)                       # b has not said it is idle
    idle(b, 1, 20_000)
    run = a.expect("run")
    assert (run["t"], run["seq"]) == (10_000, 2)
    b.expect_nothing(0.2)                       # b has nothing before 20 000
    idle(a, 2, 20_000)
    assert a.expect("run")["t"] == 20_000
    assert b.expect("run")["t"] == 20_000


def test_a_stale_idle_does_not_count(conductor):
    a, b = conductor(1), conductor(2)
    join_virtual(a)
    join_virtual(b)
    idle(b, 1, None)
    idle(a, 1, 5_000)
    assert a.expect("run")["seq"] == 2
    # An answer to the last grant, not this one: it does not count. Once is
    # an idle crossing the run on the wire; said again, the station missed
    # what came after it, which is sent again as it was.
    idle(a, 1, 6_000)
    a.expect_nothing(0.3)
    idle(a, 1, 6_000)
    assert a.expect("run") == {"type": "run", "seq": 2, "t": 5_000}
    idle(a, 2, 6_000)
    assert a.expect("run")["t"] == 6_000


def test_an_idle_said_twice_counts_once(conductor):
    a, b = conductor(1), conductor(2)
    join_virtual(a)
    join_virtual(b)
    idle(b, 1, None)
    idle(a, 1, 5_000)
    assert a.expect("run")["seq"] == 2
    idle(a, 2, 6_000)
    run = a.expect("run")
    assert run == {"type": "run", "seq": 3, "t": 6_000}
    # Said once more after the run went out: that run crossed it, nothing
    # is sent; said yet again, the run was lost, and goes once more, and
    # never a new grant.
    idle(a, 2, 6_000)
    a.expect_nothing(0.2)
    idle(a, 2, 6_000)
    assert a.expect("run") == run
    idle(a, 2, 6_000)                           # within the resend gap: nothing
    a.expect_nothing(0.3)


def test_anything_a_station_says_retracts_its_idle(conductor):
    a, b = conductor(1), conductor(2)
    join_virtual(a)
    join_virtual(b)
    idle(b, 1, None)
    a.state("RX")
    idle(a, 1, 7_000)
    assert a.expect("run")["t"] == 7_000
    a.state("RX")                               # before it has answered the run
    idle(a, 2, 9_000)
    assert a.expect("run")["t"] == 9_000


def test_rx_begin_and_rx_end_arrive_at_their_instants_in_t(conductor):
    conductor.link(1, 2, NEAR_DB)
    tx, rx = conductor(1), conductor(2)
    join_virtual(tx)
    join_virtual(rx)
    rx.state("RX")
    idle(rx, 1, None)
    idle(tx, 1, 50_000)
    assert tx.expect("run")["t"] == 50_000
    tx.send(dict({"type": "tx", "slot": 0, "id": 1, "t0": 50_000,
                  "t_pre": 50_000 + FRAME_US // 10, "t_hdr": 50_000 + FRAME_US // 5,
                  "t_end": 50_000 + FRAME_US, "power_dbm": POWER_DBM,
                  "payload": base64.b64encode(b"on time").decode()}, **radio("TX")))
    idle(tx, 2, None)
    begin = rx.expect("rx_begin")
    assert (begin["t"], begin["t0"], begin["t_end"]) == (50_000, 50_000, 50_000 + FRAME_US)
    assert begin["t_pre"] == 50_000 + FRAME_US // 10
    rx.expect_nothing(0.2)                      # T waits on rx, which has not answered
    idle(rx, begin["seq"], begin["t_pre"])
    assert rx.expect("run")["t"] == begin["t_pre"]
    idle(rx, begin["seq"] + 1, None)
    end = rx.expect("rx_end")
    assert end["t"] == 50_000 + FRAME_US
    assert base64.b64decode(end["payload"]) == b"on time"
    lines = [l for l in conductor.record.read_text().splitlines() if not l.startswith("#")]
    stamps = {json.loads(l.split("\t")[3])["type"]: l.split("\t")[0] for l in lines}
    assert stamps["rx_end"] == "%.6f" % ((50_000 + FRAME_US) / 1e6)
    assert "idle" not in stamps and "run" not in stamps


def test_a_late_tx_is_clamped_to_t(conductor):
    conductor.link(1, 2, NEAR_DB)
    tx, rx = conductor(1), conductor(2)
    join_virtual(tx)
    join_virtual(rx)
    rx.state("RX")
    idle(rx, 1, None)
    idle(tx, 1, 80_000)
    tx.expect("run")
    idle(tx, 2, None)
    time.sleep(0.1)
    # Something from outside the run moves the station after it said it was
    # idle; the frame it stamps with the T it last had goes on the air at T.
    tx.send(dict({"type": "tx", "slot": 0, "id": 1, "t0": 30_000, "t_pre": 31_000,
                  "t_hdr": 32_000, "t_end": 40_000, "power_dbm": POWER_DBM,
                  "payload": base64.b64encode(b"late").decode()}, **radio("TX")))
    idle(tx, 2, None)
    begin = rx.expect("rx_begin")
    assert (begin["t0"], begin["t_pre"], begin["t_end"]) == (80_000, 81_000, 90_000)


def test_one_instant_is_ruled_in_station_order(conductor):
    conductor.link(1, 3, NEAR_DB)
    conductor.link(2, 3, NEAR_DB)
    early, late, rx = conductor(1), conductor(2), conductor(3)
    for st in (early, late, rx):
        join_virtual(st)
    rx.state("RX")
    idle(rx, 1, None)
    idle(early, 1, None)
    idle(late, 1, None)

    def send_tx(st, payload):
        st.send(dict({"type": "tx", "slot": 0, "id": 1, "t0": 0,
                      "t_pre": FRAME_US // 10, "t_hdr": FRAME_US // 5,
                      "t_end": FRAME_US, "power_dbm": POWER_DBM,
                      "payload": base64.b64encode(payload).decode()}, **radio("TX")))

    # Station 2 speaks first on the wire; station 1's frame still gets the
    # lower number, and the receiver, because one instant is taken in station
    # order.
    send_tx(late, b"two")
    time.sleep(0.1)
    send_tx(early, b"one")
    idle(late, 1, None)
    idle(early, 1, None)
    first = rx.expect("rx_begin")
    idle(rx, first["seq"], None)
    second = rx.expect("rx_begin")
    assert first["id"] < second["id"]
    assert "cad" not in first and second["cad"] is True
    idle(rx, second["seq"], None)
    ends = {}
    for _ in range(2):
        end = rx.expect("rx_end")
        ends[end["id"]] = end
        idle(rx, end["seq"], None)
    assert ends[first["id"]]["verdict"] == ends[second["id"]]["verdict"] == "crc"
    lines = [l for l in conductor.record.read_text().splitlines() if not l.startswith("#")]
    order = [json.loads(l.split("\t")[3]) for l in lines if "\tout\t3\t" in l]
    begins = [m for m in order if m["type"] == "rx_begin"]
    assert [m["id"] for m in begins] == sorted(m["id"] for m in begins)


def test_a_station_that_leaves_restarts_with_a_hello(conductor):
    a, b = conductor(1), conductor(2)
    join_virtual(a)
    join_virtual(b)
    idle(a, 1, 10_000)
    idle(b, 1, 10_000)
    assert a.expect("run")["t"] == 10_000
    b.expect("run")
    # b restarts: its hello replaces everything the ether knew of it.
    welcome = b.hello()
    assert (welcome["t"], welcome["seq"]) == (10_000, 1)
    idle(a, 2, 20_000)
    a.expect_nothing(0.2)
    idle(b, 1, 30_000)
    assert a.expect("run")["t"] == 20_000


def test_paced_time_follows_the_wall(tmp_path):
    bed = Bench(tmp_path, "10x")
    try:
        a = bed(1)
        a.hello()
        idle(a, 1, 500_000)                     # half a second of T: 50 ms of wall at 10x
        start = time.monotonic()
        a.expect("run")
        idle(a, 2, 1_500_000)                   # a whole second of T: 100 ms
        a.expect("run")
        took = time.monotonic() - start
        assert 0.08 <= took <= 0.4
    finally:
        bed.close()


# ---------------------------------------------------------------------------
# Virtual time: input that does not come over the air
# ---------------------------------------------------------------------------

def at_address(station, host):
    """Move a fake station's socket to its own loopback address, as a
    station of a run has, so the ends of a TCP connection name it."""
    station.sock.close()
    station.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    station.sock.bind((host, 0))
    return station


class Asker:
    """A station thread's own socket for asking to write over TCP."""

    def __init__(self, station):
        self.station = station
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.req = 0

    def ask(self, ch, n):
        self.req += 1
        self.sock.sendto(json.dumps({"type": "wrote", "sid": self.station.sid, "ch": ch,
                                     "n": n, "go": self.req}).encode(),
                         self.station.ether)
        return self.req

    def go(self, timeout=2.0):
        self.sock.settimeout(timeout)
        try:
            return json.loads(self.sock.recvfrom(1024)[0])["go"]
        except socket.timeout:
            return None

    def close(self):
        self.sock.close()


A_TO_B = "tcp/127.0.0.11:40000>127.0.0.12:4965"
B_TO_A = "tcp/127.0.0.12:4965>127.0.0.11:40000"


def test_a_tcp_write_waits_for_its_reader_and_holds_t_until_it_is_read(conductor):
    a = at_address(conductor(1), "127.0.0.11")
    b = at_address(conductor(2), "127.0.0.12")
    join_virtual(a)
    join_virtual(b)
    idle(b, 1, None)
    idle(a, 1, 10_000)
    assert a.expect("run")["t"] == 10_000
    asker = Asker(a)
    try:
        req = asker.ask(A_TO_B, 10)
        # b was last told T 0: it is told this T before a may write.
        run = b.expect("run")
        assert run["t"] == 10_000
        assert asker.go(0.3) is None
        idle(b, run["seq"], None)
        assert asker.go() == req
        # The bytes are on their way and b has not read them: T stays.
        idle(a, 2, 20_000)
        a.expect_nothing(0.3)
        b.send({"type": "read", "ch": A_TO_B, "n": 10})
        # b is at work on them, at the T it has.
        run = b.expect("run")
        assert run["t"] == 10_000
        idle(b, run["seq"], None)
        assert a.expect("run")["t"] == 20_000
    finally:
        asker.close()


def test_two_stations_writing_to_each_other_go_in_station_order(conductor):
    a = at_address(conductor(1), "127.0.0.11")
    b = at_address(conductor(2), "127.0.0.12")
    join_virtual(a)
    join_virtual(b)
    idle(a, 1, 10_000)
    idle(b, 1, 10_000)
    a.expect("run")
    b.expect("run")
    ask_a, ask_b = Asker(a), Asker(b)
    try:
        req_b = ask_b.ask(B_TO_A, 5)            # b asks first, while a is still at work
        assert ask_b.go(0.3) is None
        req_a = ask_a.ask(A_TO_B, 7)
        assert ask_a.go() == req_a              # the lower station id goes first
        assert ask_b.go(0.3) is None            # and b waits until a is done
        idle(a, 2, None)
        assert ask_b.go() == req_b
    finally:
        ask_a.close()
        ask_b.close()


def test_a_write_to_something_outside_the_run_is_let_go_at_once(conductor):
    a = at_address(conductor(1), "127.0.0.11")
    join_virtual(a)
    asker = Asker(a)
    try:
        req = asker.ask("tcp/127.0.0.11:40000>127.0.0.99:80", 3)
        assert asker.go() == req
    finally:
        asker.close()


class InProcess:
    """An ether in this process's loop, in virtual time, with one station
    on a UDP socket, so its Python side can be driven directly."""

    def __init__(self, loop):
        self.loop = loop
        self.ether = None
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.setblocking(False)

    async def start(self):
        _, self.ether = await self.loop.create_datagram_endpoint(
            lambda: ether_module.Ether(None, time_mode="max"),
            local_addr=("127.0.0.1", 0))
        self.addr = self.ether.transport.get_extra_info("sockname")

    def send(self, msg):
        self.sock.sendto(json.dumps(dict(msg, sid=1)).encode(), self.addr)

    async def recv(self, timeout=1.0):
        end = self.loop.time() + timeout
        while self.loop.time() < end:
            try:
                return json.loads(self.sock.recv(65535))
            except BlockingIOError:
                await asyncio.sleep(0.005)
        return None

    async def join(self):
        self.send({"type": "hello", "slots": [0], "t": 0})
        welcome = await self.recv()
        self.send({"type": "idle", "seq": welcome["seq"], "until": None})
        await asyncio.sleep(0.05)

    def close(self):
        self.sock.close()
        self.ether.close()


def in_process(test):
    loop = asyncio.new_event_loop()
    try:
        bed = InProcess(loop)
        loop.run_until_complete(bed.start())
        try:
            loop.run_until_complete(test(bed))
        finally:
            bed.close()
    finally:
        loop.close()


def test_t_stays_at_a_sleeps_end_until_what_it_woke_has_run():
    async def test(bed):
        ether = bed.ether
        await bed.join()
        # Something else would move T on at once: a heap entry at 2 s.
        ether.call_at(2_000_000, lambda: None)
        seen = []

        async def after():
            await ether.sleep(1.0)
            await asyncio.sleep(0)              # a turn of the loop later
            seen.append(ether.now())
            ether.expect(9)                     # a station starting at this T

        await asyncio.wait_for(after(), 2)
        await asyncio.sleep(0.05)
        assert seen == [1_000_000]
        assert ether.now() == 1_000_000         # waiting for station 9

    in_process(test)


def test_a_line_typed_at_a_station_waits_for_it_to_have_t_and_holds_t_until_read():
    async def test(bed):
        ether = bed.ether
        await bed.join()
        await ether.sleep(1.0)
        await asyncio.sleep(0.05)
        synced = []
        ether.typed(1, 12)
        ether.sync(1, lambda: synced.append(ether.now()))
        run = await bed.recv()
        assert run["type"] == "run" and run["t"] == 1_000_000
        assert synced == []
        bed.send({"type": "idle", "seq": run["seq"], "until": None})
        await asyncio.sleep(0.05)
        assert synced == [1_000_000]
        assert ether.busy()                     # twelve bytes it has not read
        bed.send({"type": "read", "ch": "tty", "total": 12})
        run = await bed.recv()
        assert run["type"] == "run" and run["t"] == 1_000_000
        bed.send({"type": "idle", "seq": run["seq"], "until": None})
        await asyncio.sleep(0.05)
        assert not ether.busy()

    in_process(test)


def test_what_stations_printed_is_read_before_t_moves():
    async def test(bed):
        ether = bed.ether
        drains = []
        ether.on_drain = lambda sids, done: drains.append((sids, done, ether.now()))
        await bed.join()
        ether.call_at(3_000_000, lambda: None)
        ether.kick()
        await asyncio.sleep(0.05)
        assert [(sids, t) for sids, _, t in drains] == [([1], 0)]
        assert ether.now() == 0                 # nothing moves until it is read
        drains[0][1]()
        await asyncio.sleep(0.05)
        assert ether.now() == 3_000_000

    in_process(test)
