"""What a frame on the air is, read the way a Reticulous station reads it.

An air frame is the RNode header byte (a sequence number and the split flag)
and then a Reticulum packet, except for the frames the interface speaks for
itself: SUPE's (first byte 0xC2..0xC8) and the four-byte power request. A
Reticulum packet is the firmware's `rnsParse` layout: a flags byte, a hop
count, one or two addresses, a context byte, then the data. A packet too
long for one frame goes as two, both with the split flag; which half a frame
is follows from the order its sender sent them in (`Halves`).

Three readings are here: `read_frame`, one line for a person; `kind_of`, one
short class word for counting airtime; `announce`, the destination and hop
count of an announce, for following paths as they spread.
"""

import hashlib

RNODE_FLAG_SPLIT = 0x01     # the air frame is half of a packet
MAGIC_PWRREQ = 0x04         # the four-byte power request that prefixes a frame
PWRREQ_LEN = 4
SUPE_TYPE = {0xC2: "HAIL", 0xC3: "ANNOUNCE", 0xC4: "GOT", 0xC5: "READY",
             0xC6: "END", 0xC7: "BYE", 0xC8: "RESEND"}

BENCH_MAX = 48              # printable frames this short are somebody's `lora tx`
TRUNC_HASH = 16             # a Reticulum address is a truncated hash, 16 bytes

PACKET_TYPE = {0: "DATA", 1: "ANNOUNCE", 2: "LINKREQUEST", 3: "PROOF"}
DEST_TYPE = {0: "single", 1: "group", 2: "plain", 3: "link"}
CONTEXT = {
    0x00: "", 0x01: "resource", 0x02: "resource-adv", 0x03: "resource-req",
    0x04: "resource-hmu", 0x05: "resource-prf", 0x06: "resource-icl",
    0x07: "resource-rcl", 0x08: "cache-request", 0x09: "request",
    0x0A: "response", 0x0B: "path-response", 0x0C: "command",
    0x0D: "command-status", 0x0E: "channel", 0xFA: "keepalive",
    0xFB: "link-identify", 0xFC: "link-close", 0xFD: "link-proof",
    0xFE: "link-rtt", 0xFF: "link-proof",
}

# The destinations this mesh names out loud. An announce carries the name hash
# of its aspect; a plain destination is addressed by the hash of that hash.
ASPECTS = [
    "lxmf.delivery", "lxmf.propagation", "lxmproxy.server",
    "nomadnetwork.node", "netgraph.discovery",
    "rnstransport.probe", "rnstransport.remote.management",
    "rnstransport.path.request", "rnstransport.tunnel.synthesize",
]


def name_hash(name):
    return hashlib.sha256(name.encode()).digest()[:10]


NAME_HASHES = {name_hash(n): n for n in ASPECTS}
PLAIN_DESTS = {hashlib.sha256(name_hash(n)).digest()[:TRUNC_HASH]: n
               for n in ASPECTS}


class Halves:
    """Which half of a split packet each sender's next split frame is.

    A sender's split frames come in pairs, first half then second, so the
    parity of what it has sent is the answer: `part(sender, payload)` is 0
    for a frame that is not split, else 1 or 2, and must see every frame the
    sender transmits, in order.
    """

    def __init__(self):
        self.open = set()           # senders with the first half of a packet out

    def part(self, sender, payload):
        if not payload or not payload[0] & RNODE_FLAG_SPLIT or payload[0] in SUPE_TYPE:
            return 0
        part = 2 if sender in self.open else 1
        self.open.symmetric_difference_update({sender})
        return part


def hexid(raw):
    """A destination as a person reads it: the first four bytes."""
    return raw[:4].hex()


def read_frame(frame, part=0):
    """What one frame on the air is, in a line."""
    if not frame:
        return "empty frame"
    if len(frame) == PWRREQ_LEN and frame[0] == MAGIC_PWRREQ:
        return "power request  suggest %d dBm" % ((frame[1] ^ 0x80) - 0x80)
    if frame[0] in SUPE_TYPE:
        return "SUPE %-6s %dB" % (SUPE_TYPE[frame[0]], len(frame))
    if len(frame) <= BENCH_MAX and all(0x20 <= b < 0x7F for b in frame):
        return "bench frame  \"%s\"  %dB" % (frame.decode(), len(frame))
    split = " split %s" % ("1/2" if part == 1 else "2/2") if part else ""
    if part == 2:
        return "…continuation  %dB%s" % (len(frame), split)
    return read_packet(frame[1:]) + split


def read_packet(payload):
    """A Reticulum packet's header in one line, or what it is instead."""
    if not payload:
        return "empty packet"
    flags = payload[0]
    if flags & 0x80:
        return "%d B, IFAC-authenticated" % len(payload)
    hdr2 = bool(flags & 0x40)
    need = 2 + (32 if hdr2 else 16) + 1
    if len(payload) < need:
        return "%d B, not a Reticulum packet" % len(payload)
    ptype = PACKET_TYPE.get(flags & 0x03, "?")
    dtype = DEST_TYPE.get((flags >> 2) & 0x03, "?")
    ctxflag = bool(flags & 0x20)
    hops = payload[1]
    via = payload[2:2 + TRUNC_HASH] if hdr2 else None
    dest = payload[2 + (TRUNC_HASH if hdr2 else 0):][:TRUNC_HASH]
    ctx = payload[need - 1]
    data = payload[need:]

    where = PLAIN_DESTS.get(dest) or "%s/%s" % (dtype, hexid(dest))
    parts = ["%-11s %s" % (ptype, where)]
    if ptype == "ANNOUNCE" and len(data) >= 74:
        aspect = NAME_HASHES.get(data[64:74])
        parts.append("of %s" % (aspect or "?" + data[64:74][:4].hex()))
        if ctxflag:
            parts.append("+ratchet")
    elif CONTEXT.get(ctx):
        parts.append(CONTEXT[ctx])
    if via is not None:
        parts.append("via %s" % hexid(via))
    parts.append("hops=%d" % hops)
    parts.append("%dB" % len(payload))
    return "  ".join(parts)


def kind_of(payload, part):
    """What a frame is, as one short class word for airtime; None for the
    second half of a split packet, which counts under its first half's."""
    if not payload:
        return "empty"
    if payload[0] in SUPE_TYPE:
        return "SUPE " + SUPE_TYPE[payload[0]]
    if len(payload) == PWRREQ_LEN and payload[0] == MAGIC_PWRREQ:
        return "power request"
    if part == 2:
        return None
    p = payload[1:]
    if len(p) < 19 or p[0] & 0x80:
        return "other"
    flags = p[0]
    hdr2 = bool(flags & 0x40)
    need = 2 + (32 if hdr2 else 16) + 1
    if len(p) < need:
        return "other"
    ptype = flags & 0x03
    dest = p[2 + (16 if hdr2 else 0):][:16]
    ctx = p[need - 1]
    data = p[need:]
    if ptype == 1:
        if ctx == 0x0B:
            return "path response"
        aspect = NAME_HASHES.get(data[64:74]) if len(data) >= 74 else None
        return "announce " + (aspect or "other")
    if ptype == 2:
        return "link request"
    if ptype == 3:
        return "link proof" if ctx == 0xFF else "proof"
    name = PLAIN_DESTS.get(dest)
    if name == "rnstransport.path.request":
        return "path request"
    if ctx in (0xFB, 0xFC, 0xFE, 0xFA):
        return "link control"
    if 0x01 <= ctx <= 0x07:
        return "resource"
    return "data"


def is_announce_kind(kind):
    """True for the classes that are announces, Reticulum's or SUPE's."""
    return kind.startswith("announce") or kind == "SUPE ANNOUNCE"


def calling_class(kind):
    """The class a calling-channel frame counts under in the role split:
    announces and path traffic, SUPE HAIL, the rest of SUPE, unicast."""
    if is_announce_kind(kind) or kind in ("path request", "path response"):
        return "announces_and_path"
    if kind == "SUPE HAIL":
        return "hails"
    if kind.startswith("SUPE "):
        return "supe_other"
    return "unicast"


CALLING_CLASSES = ("announces_and_path", "hails", "supe_other", "unicast")


def announce(payload, part):
    """(destination hex, hops) of an announce frame, else None."""
    if part == 2 or not payload or payload[0] in SUPE_TYPE:
        return None
    body = payload[1:]
    if len(body) < 19 or body[0] & 0x80:
        return None
    flags = body[0]
    if flags & 0x03 != 1:
        return None
    hdr2 = bool(flags & 0x40)
    dest = body[2 + (16 if hdr2 else 0):][:16]
    return dest.hex(), body[1]
