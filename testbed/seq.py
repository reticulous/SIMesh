#!/usr/bin/env python3
"""Draw a run's ether record as a sequence diagram: one lifeline per station.

Every frame in `record.tsv` becomes one row — an arrow from the station that
transmitted it to each station the ether told about it, with the verdict at
each arrow head and what the frame was on the right.

  seq.py RUN                      the whole record of the run
  seq.py RUN --tail 40            the last forty frames
  seq.py RUN --only announce,path rows whose label matches any of these
  seq.py --record PATH            a record on its own: lifelines by id,
                                  or by --names 1=alpha,2=bravo

Lifelines are named from the run's nodeset. The record is the ether's own
account: it knows a frame's carrier, its air time and its bytes, and nothing
about what the bytes mean. The reading on the right is the protocol's: in a
run with Reticulous stations (or a record read on its own) it is done the way
the firmware's receive path does it (`simesh.reticulum.frames.read_frame`);
in a run with none, a frame is its length and carrier.
"""

import argparse
import base64
import sys

from simesh import record as record_module
from simesh import reticulum
from simesh.reticulum import frames as rframes
from simesh.view import RunView

COL_W = 9               # characters between two lifelines
GUTTER = 3              # room to the left of the first lifeline for its name

# The verdict at an arrow head.
HEAD = {"clean": ("▶", "◀"), "crc": ("✗", "✗"),
        "hdr": ("✗", "✗"), "lost": ("·", "·"), "cad": ("~", "~")}


def read_bytes(payload, part, freq):
    """A frame as bytes on a carrier, for a protocol this tool cannot read."""
    return "%d B  %.3f MHz" % (len(payload), (freq or 0) / 1e6)


class Frame:
    """One transmission, and what each station made of it."""

    reader = None               # (payload, part, freq) -> a line; None reads Reticulum

    def __init__(self, at, sender, msg):
        self.at = at
        self.sender = sender
        self.fid = msg.get("id")
        self.freq = msg.get("freq")
        self.sf = msg.get("sf")
        self.payload = base64.b64decode(msg.get("payload") or "")
        self.part = 0           # 1 or 2 when the packet was split over two frames
        self.heard = {}         # receiver sid -> verdict, "lost" until its rx_end

    @property
    def label(self):
        if self.reader is not None:
            return self.reader(self.payload, self.part, self.freq)
        return rframes.read_frame(self.payload, self.part)


def read_record(path, reader=None):
    """The record as a list of frames, in the order they went on the air."""
    frames = []
    sent = None                 # the transmission being told to its receivers
    arriving = {}               # the ether's frame id -> that transmission
    halves = rframes.Halves()
    for stamp, direction, sid, msg in record_module.lines(path):
        kind = msg.get("type")
        if direction == "in" and kind == "tx":
            frame = Frame(record_module.parse_time(stamp), sid, msg)
            frame.reader = reader
            frame.part = halves.part(sid, frame.payload)
            frames.append(frame)
            sent = frame
            continue
        if direction != "out" or kind not in ("rx_begin", "rx_end"):
            continue
        # The ether tells a frame's receivers about it as it reads the
        # transmission, so the frame a reception belongs to is the one
        # just sent; its id, the ether's own, is what the end comes back
        # under however many frames are in the air.
        if kind == "rx_begin":
            if sent is None:
                continue        # a record that begins mid-frame
            arriving[msg.get("id")] = sent
            sent.heard[sid] = "cad" if msg.get("cad") else "lost"
        else:
            frame = arriving.get(msg.get("id"))
            if frame is not None:
                frame.heard[sid] = msg.get("verdict", "?")
    return frames


def lifelines(columns):
    """A blank row: every station's line and nothing on it."""
    row = [" "] * ((len(columns) - 1) * COL_W + 1)
    for index in range(len(columns)):
        row[index * COL_W] = "│"
    return row


def draw(frame, columns):
    """The rows for one frame: one arrow per station that heard it.

    One transmission heard by two stations is two arrows, not one line drawn
    through the transmitter — a line across three lifelines reads as a frame
    passing from the first station to the last, which is the one thing the
    diagram must never say.
    """
    here = columns[frame.sender]
    rows = []
    for sid in sorted(frame.heard, key=lambda s: columns.get(s, -1)):
        if sid not in columns:
            continue
        there = columns[sid]
        row = lifelines(columns)
        lo, hi = min(here, there), max(here, there)
        for x in range(lo * COL_W, hi * COL_W + 1):
            if row[x] == " ":
                row[x] = "─"
        for index in range(lo + 1, hi):
            row[index * COL_W] = "┼"    # a lifeline the arrow passes
        row[here * COL_W] = "├" if there > here else "┤"
        head = HEAD.get(frame.heard[sid], HEAD["lost"])
        row[there * COL_W] = head[0 if there > here else 1]
        rows.append("".join(row))
    return rows or ["".join(lifelines(columns))]


def header(columns, names):
    """The station heading above the lifelines, each name over its own line."""
    row = [" "] * ((len(columns) - 1) * COL_W + 1 + 2 * GUTTER)
    for sid, index in columns.items():
        label = names.get(sid, str(sid))[:COL_W - 1]
        start = max(0, GUTTER + index * COL_W - len(label) // 2)
        row[start:start + len(label)] = list(label)
    return "".join(row).rstrip()


def parse_names(text):
    names = {}
    for item in (text or "").split(","):
        if not item.strip():
            continue
        sid, _, name = item.partition("=")
        names[int(sid)] = name or sid
    return names


def main(argv=None):
    ap = argparse.ArgumentParser(description="a run's ether record as a sequence diagram")
    ap.add_argument("run", nargs="?", help="the run directory: its record and station names")
    ap.add_argument("--record", help="the record to read (default: the run's record.tsv)")
    ap.add_argument("--names", help="lifeline names, e.g. 1=alpha,2=bravo,3=charlie")
    ap.add_argument("--tail", type=int, metavar="N", help="only the last N frames")
    ap.add_argument("--only", metavar="WORDS",
                    help="only frames whose reading contains one of these,"
                         " comma separated (case-insensitive)")
    args = ap.parse_args(argv)
    if not args.run and not args.record:
        ap.error("give a run directory, or --record")

    names, reader, path = {}, None, args.record
    if args.run:
        view = RunView(args.run)
        names = dict(view.names)
        path = path or view.record_path
        if reticulum not in view.protocols():
            reader = read_bytes
    names.update(parse_names(args.names))

    frames = read_record(path, reader)
    if args.only:
        wanted = [w.strip().lower() for w in args.only.split(",") if w.strip()]
        frames = [f for f in frames if any(w in f.label.lower() for w in wanted)]
    if args.tail:
        frames = frames[-args.tail:]
    if not frames:
        sys.stderr.write("no frames in %s\n" % path)
        return 1

    seen = sorted({f.sender for f in frames} | {r for f in frames for r in f.heard})
    columns = {sid: index for index, sid in enumerate(seen)}
    origin = frames[0].at

    print("%8s  %s" % ("t (s)", header(columns, names)))
    for frame in frames:
        rows = draw(frame, columns)
        print("%8.3f  %s%s  %s" % (
            frame.at - origin, " " * GUTTER, rows[0],
            frame.label + ("" if frame.heard else "   → nobody")))
        for row in rows[1:]:
            # The same frame, at another station: no stamp, no second reading.
            print("%8s  %s%s" % ("", " " * GUTTER, row))
    print("\n▶ received  ✗ CRC failure  · reception never closed out  "
          "~ sensed by a channel activity detection")
    return 0


if __name__ == "__main__":
    sys.exit(main())
