"""The framed-RPC demultiplexer and client against synthetic byte streams."""

import asyncio
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import rpc  # noqa: E402


def frame(fid, payload):
    return rpc.frame(fid, payload)


def run_chunks(chunks, demux=None, now=0.0):
    demux = demux or rpc.FrameDemux()
    text = bytearray()
    frames = []
    for chunk in chunks:
        t, f = demux.feed(chunk, now)
        text += t
        frames += f
    return bytes(text), frames, demux


def split_every(data, n):
    return [data[i:i + n] for i in range(0, len(data), n)]


LOG1 = b"I (12) main: booting\n"
LOG2 = "\x1b[0;32mI [serial] framed rpc v1\x1b[0m\n".encode()
LOG3 = b"W (40) net: something \xe2\x9c\x93 unicode\n"


def test_frames_between_log_text_come_out_and_the_text_is_untouched():
    stream = LOG1 + frame(0x41, b"s.net.hostname = alpha\n") + LOG2 + frame(0x42, b"") + LOG3
    text, frames, demux = run_chunks([stream])
    assert text == LOG1 + LOG2 + LOG3
    assert frames == [(0x41, b"s.net.hostname = alpha\n"), (0x42, b"")]
    assert not demux.pending


def test_every_split_of_the_stream_gives_the_same_result():
    stream = (LOG1 + frame(0x41, b"one\ntwo\n") + LOG2
              + frame(0x7f, bytes(range(0x20, 0x80)) * 3) + LOG3)
    whole_text, whole_frames, _ = run_chunks([stream])
    for size in (1, 2, 3, 4, 5, 7, 11, 64):
        text, frames, demux = run_chunks(split_every(stream, size))
        assert text == whole_text, size
        assert frames == whole_frames, size
        assert not demux.pending


def test_a_magic_split_across_reads_waits_for_the_rest():
    whole = frame(0x50, b"answer\n")
    demux = rpc.FrameDemux()
    text, frames = demux.feed(LOG1 + whole[:2], 0.0)
    assert text == LOG1 and frames == [] and demux.pending
    text, frames = demux.feed(whole[2:], 0.1)
    assert text == b"" and frames == [(0x50, b"answer\n")]


def test_a_false_start_is_text():
    # 0xF5 followed by anything but the rest of the magic.
    for junk in (b"\xf5x", b"\xf5S", b"\xf5SG", b"\xf5SG\x02", b"\xf5\xf5SGx"):
        stream = b"before " + junk + b" after\n"
        for size in (1, 2, 100):
            text, frames, demux = run_chunks(split_every(stream, size))
            assert text == stream, (junk, size)
            assert frames == []
            assert not demux.pending


def test_a_magic_that_starts_inside_a_false_one_is_found():
    stream = b"\xf5" + frame(0x60, b"x\n") + b"tail"
    for size in (1, 3, 100):
        text, frames, _ = run_chunks(split_every(stream, size))
        assert text == b"\xf5tail"
        assert frames == [(0x60, b"x\n")]


def test_an_id_outside_the_query_range_is_not_a_frame():
    bogus = rpc.MAGIC + bytes((0x0a, 0x00, 0x02)) + b"hi"
    stream = LOG1 + bogus + frame(0x61, b"real\n")
    text, frames, _ = run_chunks([stream])
    assert text == LOG1 + bogus
    assert frames == [(0x61, b"real\n")]


def test_a_corrupt_length_resyncs_on_the_next_magic():
    # The length claims far more than follows; the next frame's magic ends it.
    broken = rpc.MAGIC + bytes((0x41, 0x7f, 0xff)) + b"partial reply\n"
    stream = broken + LOG1 + frame(0x42, b"good\n") + LOG3
    for size in (1, 2, 5, 1000):
        text, frames, demux = run_chunks(split_every(stream, size))
        assert frames == [(0x42, b"good\n")], size
        assert text == broken + LOG1 + LOG3, size
        assert not demux.pending


def test_a_frame_whose_remainder_never_arrives_is_given_back_after_the_resync_time():
    broken = rpc.MAGIC + bytes((0x41, 0x01, 0x00)) + b"never finished"
    demux = rpc.FrameDemux()
    text, frames = demux.feed(LOG1 + broken, 10.0)
    assert text == LOG1 and demux.pending
    assert demux.expire(10.0 + rpc.RESYNC_S / 2) == (b"", [])
    text, frames = demux.expire(10.0 + rpc.RESYNC_S)
    assert text == broken and frames == []
    assert not demux.pending
    # and the stream carries on as normal afterwards
    text, frames = demux.feed(frame(0x43, b"ok\n") + LOG3, 12.0)
    assert text == LOG3 and frames == [(0x43, b"ok\n")]


def test_a_held_partial_magic_expires_as_text():
    demux = rpc.FrameDemux()
    demux.feed(b"abc\xf5S", 0.0)
    text, _ = demux.expire(rpc.RESYNC_S)
    assert text == b"\xf5S"


def test_a_log_line_inside_a_frame_does_not_lose_the_next_frame():
    reply = b"line one\nline two\n"
    good = frame(0x44, reply)
    # A log line lands after the header: the length now counts log bytes as
    # payload and the frame's own tail spills into the text.
    torn = good[:rpc.HEADER] + LOG1 + good[rpc.HEADER:]
    stream = torn + LOG3 * 3 + frame(0x45, b"after\n")
    text, frames, demux = run_chunks(split_every(stream, 7))
    assert (0x45, b"after\n") in frames
    assert not demux.pending
    assert text.endswith(LOG3)


def test_random_streams_round_trip():
    rng = random.Random(7)
    for _ in range(200):
        parts, want_text, want_frames = [], b"", []
        for _ in range(rng.randrange(1, 12)):
            if rng.random() < 0.4:
                fid = rng.randrange(rpc.ID_FIRST, rpc.ID_LAST + 1)
                payload = bytes(rng.randrange(0x20, 0x7f) for _ in range(rng.randrange(0, 300)))
                parts.append(frame(fid, payload))
                want_frames.append((fid, payload))
            else:
                t = bytes(rng.choice(b"abc \n\xf5\x1b[mSG\x01") for _ in range(rng.randrange(0, 80)))
                # a text run that happens to spell the magic is not text
                t = t.replace(rpc.MAGIC, b"magic")
                parts.append(t)
                want_text += t
        stream = b"".join(parts)
        cuts = sorted(rng.sample(range(len(stream) + 1), min(len(stream), 6)))
        chunks = [stream[a:b] for a, b in zip([0] + cuts, cuts + [len(stream)])]
        demux = rpc.FrameDemux()
        text, frames, demux = run_chunks(chunks, demux)
        text += demux.expire(1e9)[0]
        assert frames == want_frames
        assert text == want_text


def test_query_ids_stay_in_range_and_match_flashmons_hash():
    for cmd in ("", "show s.net.hostname", "auth -O", "lora up", "save", "x" * 500):
        assert rpc.ID_FIRST <= rpc.query_id(cmd) <= rpc.ID_LAST
    h = 0
    for ch in "auth -O":
        h = (h * 31 + ord(ch)) & 0xff
    assert rpc.query_id("auth -O") == 0x20 + h % 0xa0


def test_marker_split_across_reads_is_seen():
    sent = []
    client = rpc.RpcClient(sent.append)
    client.on_text(b"\x1b[0;32mI [ser")
    assert not client.available
    client.on_text(b"ial] framed rpc v1\x1b[0m\n")
    assert client.available and client.marker.is_set()


def test_client_one_in_flight_late_reply_answers_the_retry():
    async def go():
        sent = []
        client = rpc.RpcClient(sent.append)
        client.available = True

        async def answer_late():
            # Nothing for the first attempt; the reply to it lands during the
            # retry and is taken, since the retry carries the same id.
            while len(sent) < 2:
                await asyncio.sleep(0.01)
            fid = sent[0][4]
            assert sent[1][4] == fid
            client.on_frame(fid, b"s.net.hostname = alpha\n")

        task = asyncio.ensure_future(answer_late())
        reply = await client.query("show s.net.hostname", timeout=0.05, tries=3)
        await task
        assert reply == "s.net.hostname = alpha\n"

        # A reply for an id nobody is waiting on is kept for its retry only.
        client.on_frame(0x30, b"stray")
        assert client.late == {0x30: "stray"}
    asyncio.run(go())


def test_client_queries_are_serialised():
    async def go():
        sent = []
        client = rpc.RpcClient(sent.append)
        client.available = True
        order = []

        async def ask(cmd):
            reply = await client.query(cmd, timeout=1.0)
            order.append(reply)

        a = asyncio.ensure_future(ask("show a"))
        b = asyncio.ensure_future(ask("show b"))
        await asyncio.sleep(0.02)
        assert len(sent) == 1           # b waits for a
        client.on_frame(sent[0][4], b"A")
        await asyncio.sleep(0.02)
        assert len(sent) == 2
        client.on_frame(sent[1][4], b"B")
        await asyncio.gather(a, b)
        assert order == ["A", "B"]
    asyncio.run(go())


def test_no_marker_means_one_probe_then_ctrl_c():
    async def go():
        sent = []
        client = rpc.RpcClient(sent.append)
        orig = rpc.QUERY_TIMEOUT_S
        rpc.QUERY_TIMEOUT_S = 0.05
        try:
            up = await client.wait_ready(timeout=5.0, marker_wait=0.01)
        finally:
            rpc.QUERY_TIMEOUT_S = orig
        assert up is False
        assert sent[-1] == rpc.CTRL_C
        assert len([s for s in sent if s.startswith(rpc.MAGIC)]) == 1
        assert not client.available
        # never a second probe
        assert await client.probe_once() is False
    asyncio.run(go())


def test_marker_then_probe_answered_is_ready():
    async def go():
        sent = []
        client = rpc.RpcClient(sent.append)

        async def station():
            client.on_text(b"I [serial] framed rpc v1\n")
            while not sent:
                await asyncio.sleep(0.01)
            client.on_frame(sent[0][4], b"s.net.hostname = alpha\n")

        task = asyncio.ensure_future(station())
        assert await client.wait_ready(timeout=5.0, marker_wait=5.0)
        await task
    asyncio.run(go())
