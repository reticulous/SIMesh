"""The time shim, in a stand-in station, against a fake conductor.

standin.c links the chip library and runs a thread that sleeps in 25 ms steps
and an interval timer at 10 ms; it is started with the shim preloaded, in a
virtual-time run with the thread census on. The conductor here grants T only
up to what the station last asked for, as the ether does, and every event the
station prints must land at its own instant in node time.
"""

import json
import os
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
    def __init__(self, port):
        env = dict(os.environ, SIMESH_TIME="virtual", SIMESH_IDLE="threads",
                   SIMESH_EPOCH_US=str(EPOCH), LD_PRELOAD=SHIM)
        self.proc = subprocess.Popen([STANDIN, "127.0.0.1:%d" % port], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
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
    # Every instant the station asked for is a tick or a sleep ending.
    ticks = {10_000 * i for i in range(1, 25)}
    assert set(untils) <= ticks | set(sleeps) | {s + 25_000 for s in sleeps}
    # Twenty grants of T went by without the busy watchdog: well under its
    # 20 ms of wall each.
    assert took < grants * 0.02
