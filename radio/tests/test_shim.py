"""The time shim, in a stand-in station, against a fake conductor.

standin.c links the chip library and runs a thread that sleeps in 25 ms steps,
one in 40 ms timed condition waits, and an interval timer at 10 ms; it is started with the shim preloaded, in a
virtual-time run with the thread census on. The conductor here grants T only
up to what the station last asked for, as the ether does, and every event the
station prints must land at its own instant in node time.
"""

import json
import os
from errno import ETIMEDOUT
import select
import shutil
import socket
import subprocess
import time

import pytest

from test_model import BUILD, RADIO, load_library

SHIM = os.path.join(BUILD, "libsimclock.so")
STANDIN = os.path.join(BUILD, "standin")
EPOCH = 1_790_000_000_000_000


def build_standin():
    load_library()      # builds the library, and the shim with it, if they are missing
    if not os.path.exists(SHIM):
        subprocess.run(["cmake", "--build", BUILD], check=True, stdout=subprocess.DEVNULL)
    src = os.path.join(RADIO, "tests", "standin.c")
    if (not os.path.exists(STANDIN)
            or os.path.getmtime(STANDIN) < os.path.getmtime(src)
            or os.path.getmtime(STANDIN) < os.path.getmtime(os.path.join(BUILD, "libsimradio.a"))):
        cc = shutil.which("gcc") or pytest.fail("no gcc to build the stand-in")
        subprocess.run([cc, "-O1", "-I", os.path.join(RADIO, "include"), src,
                        os.path.join(BUILD, "libsimradio.a"), "-lstdc++", "-lm", "-lpthread",
                        "-o", STANDIN], check=True)


class Station:
    """The stand-in, started with the shim; `args` follow the ether's address
    on its command line, `extra` adds to its environment, and a None there
    takes a variable out."""

    def __init__(self, port, *args, **extra):
        env = dict(os.environ, SIMESH_TIME="virtual", SIMESH_IDLE="threads",
                   SIMESH_EPOCH_US=str(EPOCH), LD_PRELOAD=SHIM)
        for key in ("SIMESH_SEED", "SIMESH_NODE_ID"):
            env.pop(key, None)
        for key, value in extra.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        self.proc = subprocess.Popen([STANDIN, "127.0.0.1:%d" % port, *args], env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL)
        self.lines = []
        self.buf = b""

    def pump(self, wait=0.0):
        fd = self.proc.stdout.fileno()
        while True:
            r, _, _ = select.select([fd], [], [], wait)
            if not r:
                break
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            self.buf += chunk
            wait = 0.05
        *whole, self.buf = self.buf.split(b"\n")
        self.lines += [line.decode().split() for line in whole if line]

    def close(self):
        self.proc.kill()
        self.proc.wait()


@pytest.fixture
def conductor():
    build_standin()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(3.0)
    station = Station(sock.getsockname()[1])
    yield sock, station
    station.close()
    sock.close()


def test_the_shim_keeps_the_station_on_node_time(conductor):
    sock, station = conductor
    data, addr = sock.recvfrom(65535)
    assert json.loads(data)["type"] == "hello"
    # T at join is 0 here, so what the station read before the welcome
    # reached it and what it reads after agree.
    t, seq = 0, 1
    sock.sendto(json.dumps({"type": "welcome", "t": t, "mode": "virtual", "rate": None,
                            "epoch": EPOCH, "seq": seq}).encode(), addr)
    grants = 0
    untils = []
    started = time.monotonic()
    while t < 200_000:
        data, _ = sock.recvfrom(65535)
        msg = json.loads(data)
        if msg.get("type") != "idle" or msg.get("seq") != seq:
            continue
        assert msg["until"] is not None and msg["until"] > t
        untils.append(msg["until"])
        t = msg["until"]
        seq += 1
        grants += 1
        sock.sendto(json.dumps({"type": "run", "t": t, "seq": seq}).encode(), addr)
    took = time.monotonic() - started
    station.pump(0.2)

    clock = next(l for l in station.lines if l[0] == "clock")
    assert int(clock[1]) == 0                           # node time is T
    assert int(clock[2]) == EPOCH // 1_000_000          # time() is the epoch plus it

    # The sleeper's 25 ms and the timer's 10 ms, each at its own instant.
    sleeps = [int(l[1]) for l in station.lines if l[0] == "sleeper"]
    assert sleeps[:7] == [25_000, 50_000, 75_000, 100_000, 125_000, 150_000, 175_000]
    alarms = [l for l in station.lines if l[0] == "alarm"]
    assert len(alarms) >= 19
    # A timed wait ends at its deadline in node time, and says it timed out.
    waits = [(int(l[1]), int(l[2])) for l in station.lines if l[0] == "waiter"]
    assert waits[:4] == [(40_000, ETIMEDOUT), (80_000, ETIMEDOUT), (120_000, ETIMEDOUT),
                         (160_000, ETIMEDOUT)]
    # Every instant the station asked for is a tick or a sleep ending.
    ticks = {10_000 * i for i in range(1, 25)}
    assert set(untils) <= ticks | set(sleeps) | {s + 25_000 for s in sleeps}
    # Twenty grants of T went by without the busy watchdog: well under its
    # 20 ms of wall each.
    assert took < grants * 0.02


def entropy_of(**extra):
    """The stand-in's `entropy` line, its first, from a station started with
    `extra`. A real-time stand-in prints without end after it, so only that
    line is read."""
    build_standin()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    station = Station(sock.getsockname()[1], **extra)
    try:
        r, _, _ = select.select([station.proc.stdout], [], [], 5.0)
        assert r, "the stand-in printed nothing"
        line = station.proc.stdout.readline().decode().split()
        assert line and line[0] == "entropy"
        return tuple(line[1:])
    finally:
        station.close()
        sock.close()


def test_a_seeded_virtual_run_gives_each_station_its_own_repeatable_bytes():
    a = entropy_of(SIMESH_SEED="17", SIMESH_NODE_ID="3")
    assert entropy_of(SIMESH_SEED="17", SIMESH_NODE_ID="3") == a
    assert len(a) == 3 and len(set(a)) == 3        # three calls, three draws
    assert entropy_of(SIMESH_SEED="17", SIMESH_NODE_ID="4") != a
    assert entropy_of(SIMESH_SEED="18", SIMESH_NODE_ID="3") != a


def test_without_a_seed_or_virtual_time_the_bytes_are_the_hosts():
    unseeded = entropy_of(SIMESH_NODE_ID="3")
    assert entropy_of(SIMESH_NODE_ID="3") != unseeded
    real = entropy_of(SIMESH_TIME=None, SIMESH_SEED="17", SIMESH_NODE_ID="3")
    assert entropy_of(SIMESH_TIME=None, SIMESH_SEED="17", SIMESH_NODE_ID="3") != real


def test_the_shim_counts_console_and_tcp_bytes_and_waits_to_write():
    """A console line read a byte at a time is reported once, as a running
    total; a TCP write to another address of the run is asked for, waited on,
    and leaves from the station's own address; what it reads back is
    reported."""
    build_standin()
    ether = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    ether.bind(("127.0.0.1", 0))
    ether.settimeout(3.0)
    peer = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    peer.bind(("127.0.0.2", 0))
    peer.listen(1)
    host, port = peer.getsockname()
    station = Station(ether.getsockname()[1], "%s:%d" % (host, port),
                      SIMESH_NODE_ID="3", SIMESH_BIND_ADDR="127.0.0.3",
                      SIMESH_ETHER="127.0.0.1:%d" % ether.getsockname()[1])
    conn = None
    try:
        data, addr = ether.recvfrom(65535)
        assert json.loads(data)["type"] == "hello"
        ether.sendto(json.dumps({"type": "welcome", "t": 0, "mode": "virtual", "rate": None,
                                 "epoch": EPOCH, "seq": 1}).encode(), addr)
        peer.settimeout(3.0)
        conn, (from_host, _) = peer.accept()
        assert from_host == "127.0.0.3"         # its own address, not 127.0.0.1
        station.proc.stdin.write(b"ping\n")
        station.proc.stdin.flush()
        reports = []
        asked = None
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and not asked:
            data, sender = ether.recvfrom(65535)
            msg = json.loads(data)
            if msg["type"] in ("read", "wrote"):
                reports.append(msg)
            if msg["type"] == "wrote" and "go" in msg:
                asked = msg
                # Nothing has been written until it may go.
                conn.settimeout(0.2)
                with pytest.raises(socket.timeout):
                    conn.recv(64)
                ether.sendto(json.dumps({"type": "go", "go": msg["go"]},
                                        separators=(",", ":")).encode(), sender)
        assert {"type": "read", "sid": 3, "ch": "tty", "total": 5} in reports
        assert asked["ch"].startswith("tcp/127.0.0.3:") and asked["ch"].endswith(">%s:%d" % (host, port))
        assert asked["n"] == 5
        conn.settimeout(3.0)
        assert conn.recv(64) == b"ping\n"
        conn.sendall(b"pong\n")
        while True:
            msg = json.loads(ether.recvfrom(65535)[0])
            if msg["type"] == "read" and msg["ch"].startswith("tcp/"):
                break
        a, b = asked["ch"][4:].split(">")
        assert msg == {"type": "read", "sid": 3, "ch": "tcp/%s>%s" % (b, a), "n": 5}
        station.pump(1.0)
        assert ["tcp", "pong"] in station.lines
    finally:
        if conn is not None:
            conn.close()
        station.close()
        peer.close()
        ether.close()
