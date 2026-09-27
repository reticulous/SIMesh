"""The ether's record, `record.tsv`: one line per message, in the order the
ether took or sent them.

    <stamp> TAB <in|out> TAB <station id> TAB <the message as JSON>

The stamp is T in seconds in a virtual-time run and the wall clock's ISO
time in a real one. A line starting `#` is a comment (the ether writes one
each time it opens the file). `in` is what a station told the ether (`hello`,
`state`, `tx`), `out` what the ether told a station (`welcome`, `rx_begin`,
`rx_end`). The record knows a frame's carrier, air time and bytes, and
nothing about what the bytes mean.
"""

import json
import os

RECORD_FILE = "record.tsv"


def path_in(run_dir):
    return os.path.join(run_dir, RECORD_FILE)


def parse_time(stamp):
    """Seconds out of a record stamp: T itself in a virtual-time run, the time
    of day in a real one (wrapping at midnight is the caller's business)."""
    if ":" not in stamp:
        return float(stamp)
    clock = stamp.split("T")[-1]
    hour, minute, second = clock.split("+")[0].split("Z")[0].split(":")
    return int(hour) * 3600 + int(minute) * 60 + float(second)


def lines(path):
    """(stamp, direction, station id, message) for every well-formed line."""
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith("#"):
                continue
            fields = line.rstrip("\n").split("\t")
            if len(fields) != 4:
                continue
            stamp, direction, sid, blob = fields
            try:
                yield stamp, direction, int(sid), json.loads(blob)
            except ValueError:
                continue


def epoch_of(path):
    """The wall-clock seconds T = 0 stands for in a virtual-time run, from the
    ether's first `welcome`; None when the record has none."""
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            if '"welcome"' in line:
                return json.loads(line.split("\t", 3)[3])["epoch"] / 1e6
    return None
