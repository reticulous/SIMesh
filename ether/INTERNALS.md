# The ether — internals

How the medium decides who hears what, and the choices behind it.
[README.md](README.md) is what it does and the wire it speaks.

## Shape

One `asyncio.DatagramProtocol` on one UDP (User Datagram Protocol) socket,
single-threaded. Everything
is driven from arriving datagrams and from timers — the event loop's in a
real-time run, the barrier's heap in a virtual one (below):

- the **loss tables**, one per band, with the node name each station id
  runs as and each station's antenna gain — held whether or not that station
  has ever been heard from;
- a **station** is an address, per slot the last `state` message it sent and
  the reception its demodulator is locked on to, and the instant its own
  transmission stops occupying it;
- a **frame** is a transmission in flight, on the ether's clock, with the
  level it arrives at each station, the receivers decoding it and every other
  transmission it shared air and band with;
- a **reception** is one receiver slot decoding one frame, from its
  `rx_begin` to its `rx_end`, and whether it has lost the lock.

A station is created the first time it is heard from, whatever the message, and
its address is updated on every one — so a station that restarts on a new
ephemeral port is simply followed. Its name and gain are not created that way:
they come from whatever is driving the run, and a station with no name is not
in the medium at all.

The ether runs **inside** the testbed's process rather than beside it, so
losses arrive by method call. That is why there is no wire for them and
nothing to keep in step: dragging a station on the map is one
`update_node()` between two frames, once its row is recomputed.

## Clocks

Three clocks meet here and only one of them is authoritative.

A station stamps its messages with its own clock, which starts at zero when
its process first reads it. Those numbers mean nothing between stations. So on every
`tx` the ether reads only the **offsets** — `t_pre − t0`, `t_hdr − t0`,
`t_end − t0` — and rebases them onto its own monotonic clock at the instant the
datagram arrived. The `rx_begin` it sends carries ether microseconds; the
receiver, in turn, cares only about the gaps between them and schedules from its
own clock. Each hop keeps what it can trust and discards what it cannot.

The offsets are clamped: negative is zero, anything beyond the frame's own span
is the span, and a stated timeline longer than a minute is junk and is cut. A
station cannot make the ether schedule something absurd by stating it.

That is a real-time run. In a virtual one the ether's clock is **conductor
time T**, and it is the only clock: a station's timers, its sleeps and its
radio all run on T (or on node time, the station's own function of T), so a
`tx` states instants on T and the offsets are read the same way. The event
loop still carries the datagrams, but nothing the ether schedules is on the
loop's clock: `rx_end`s and the testbed's own waits (`Ether.sleep`) go into
one heap ordered by instant, then station id, then the order they were
scheduled in, and run when T reaches them.

## The barrier

In virtual time T moves only when nothing in the run could still act at the
T it has. A station is busy from the moment the ether sends it anything —
every message carries its `seq` — until it answers with an `idle` for that
`seq`; a station started and not yet heard from is busy too
(`expect()`, which the testbed calls before it starts the process). A count of
busy stations is kept, not a scan, because a barrier is taken hundreds of
times per second of T.

T also waits on two things that are not stations: **holds**, the testbed's
own work at the T it has, and **unread channels**, input a station has been
sent from outside the air and not yet read (below).

When nothing is busy, `kick()`:

1. takes whatever stations said while T stood — `state` and `tx`, held in
   arrival order — in station-id order, which is what makes two frames
   started at one instant collide the same way every run; taking them makes
   those stations busy again, so the loop ends there until they answer;
2. otherwise, when stations have run since their consoles were last read,
   has the testbed read them (`on_drain`) and holds T until it has, and until
   what that set going in the testbed has run;
3. otherwise moves T to the earliest of every station's `until` and the
   heap's first instant, runs what the heap has due, and sends `run` to every
   idle station whose `until` has been reached.

Holding a station's messages until the barrier rather than acting on them as
they arrive is the whole of determinism here. Two stations answering one
`run` race each other to the socket; taken as they arrived, the ruling on a
collision would depend on the host's scheduler.

A paced run (`--time <k>x`) measures T against the wall from where it
started, and a barrier that would get ahead waits on a loop timer. A run that
has fallen more than a quarter of a second behind carries on from where it is
rather than racing to catch up.

Two things keep a station from holding T for ever:

- **A station that asks for the T it has**, 64 times running, is given 10 ms
  instead. Work takes time; a station whose every idle says "run me now" is
  spinning, and a FreeRTOS tick of T lets it through.
- **A station that does not answer** answers anyway: its own side reports
  idle 20 ms of wall time after it was last told anything, unless a thread
  of it is still computing (the time shim holds the watchdog back then, so
  long work takes no T). An idle that
  arrives 18 ms or more after the message it answers was sent is counted per
  station (`slow_idles`), so a run that crawls can say which station is
  holding it.

A station that leaves (`leave()`) or says `hello` again is forgotten: its
held messages, its busy mark, its `until`, its channels and its asks go with
its old process.

## What does not come over the air

A station is only told T when something happens to it, and it acts on
whatever wakes it at the T it last had. Everything that can wake it other
than the ether's own messages is made into an instant of T, so that the same
run does the same thing whatever the host's scheduler does:

- **The testbed's waits.** `sleep()` ends at its instant of T with a hold,
  which is let go by `settle()`: once the event loop has nothing else ready,
  followed turn by turn, so the coroutine the wait woke and whatever it
  started (a station's start, a line typed) have run at that T. A station
  about to start is busy from `expect()`, before its process exists.
- **What the testbed types at a console** (`tty/<sid>`). `typed()` counts the
  bytes as written at once, so T waits; `sync()` holds them back until the
  station has the run's T and has taken every message sent to it, telling it
  T with a `run` first when it is behind; then the pty gets them. The
  station's time shim reports the running total it has read (`read`), and
  when that catches up the station is sent a `run` at the T it already has:
  it is at work on the input, and owes an idle for it like any message.
- **What a station prints**, which the testbed acts on (a framed-RPC reply,
  the capability marker). A station that has sent an idle has written
  everything it wrote before it; before T moves past it the testbed reads its
  pty until empty (a read of an empty pty master first waits for the kernel
  to carry the station's writes across) and T waits until what that set going
  has run, as for a sleep. The testbed's next line to a station is therefore
  typed at the T of the reply that prompted it.
- **TCP between two stations** (`tcp/<a>><b>`, one per direction). The
  writer's shim asks before the bytes exist and waits; the ether queues the
  ask and answers it only when the run is **quiet** — every station idle, or
  waiting on an ask made since it was last told anything, and no input unread
  but by such a waiting station — one ask at a time, lowest station first, and
  tells the reader T first if it is behind. Two stations that write to each
  other at one instant therefore go in the same order every run, and the
  reader reads at a T nothing else of the ether's can land in the middle of.
  The bytes count as written from the answer until the reader's `read`, and
  the reader then gets its `run`, as for the console. Both ends of a
  connection name a station, since the shim makes a station's connections to
  a loopback address leave from its own; a connection with an end that is not
  a station of the run (the testbed's web proxy) is not counted, and its asks
  are answered at once.

Counts are running totals for the console and per call for TCP (a write the
kernel took less of is followed by the difference, negative). A channel whose
reader has not caught up in `UNREAD_GRACE_S` (a second) of wall time is not
being waited for by anything in the station — input it reads some other way,
or not at all — and lets go of T, with a line in the log.

## The level, and why it is a table

```
L = P_tx + G_tx + G_rx − loss(tx → rx) − 20·log10(f / f0)
```

The loss is read, not computed: every ordered pair of nodes has one per
band, computed at one frequency `f0` in the band by whatever made the table
([`../LOSSTABLE.md`](../LOSSTABLE.md) is the format) — the
planner's P.1812 over real ground for a pack, log-distance for synthetic
ground, with the nodeset's offsets added by whoever hands the tables over —
and the medium has one code path for all of them.
Every pair is in it, not only the pairs strong enough to carry a frame,
because a pair far too weak to be decoded still adds to a receiver's
interference; a pair beyond the compute radius or off the pack is +inf, and
+inf is silence.

The last term moves the loss from `f0` to the **frame's own carrier**. Only
free-space loss scales as 20·log10(f), so the correction is good within a
band and nowhere else, which is why each band has its own table and a
carrier outside every band's table is heard by nobody.

The table is read in the frame's direction, because a measured loss is per
direction. Gains are the nodeset's and the power is the frame's own, so the
same table serves a nodeset whose antennas or transmit powers change.

A frame's level at each station is fixed the first time it is asked for —
at the frame's start for its receivers, at a verdict for an interferer — and
kept on the frame. A node moved mid-run has its row recomputed and put in
with `update_node()`, which copies the band's table, writes the row and
column into the copy and swaps it in: until then the old row stands, and a
frame already on the air keeps the levels it started with.

## The noise, and the demodulation threshold

`N = −174 + 10·log10(BW) + NF` (BW the bandwidth in Hz, NF the noise
figure), and a frame that does not clear the signal-to-noise ratio (SNR) its
spreading factor (SF) needs above it cannot be decoded: no lock, and for a frame
nothing else makes audible, no `rx_begin` at all.

A table says where a pair is silent, but a finite loss is not yet a link:
something has to say where decoding stops, and the only honest place is the
noise.

It is **per spreading factor** because that is the whole point of one: SF12 buys
17.5 dB of reach over SF7 and pays for it in air time, and a flat cut would make
the two identical to the medium. A flat cut low enough for SF12 delivers frames
no SF7 receiver could demodulate, and a nodeset laid out against it draws links
that do not exist and behaves far worse than it looks. The CRC band just above the
threshold, where a frame locks but fails its cyclic redundancy check (CRC) at a
probability, is the next thing this
could learn; `welcome` already carries a seed for it.

## Who is affected, and who can decode

These are two questions with two answers.

**Affected** is every transmission whose channel overlaps the receiver's —
their centres closer than half the sum of their bandwidths — whatever its
spreading factor or sync word. A LoRa demodulator hears every chirp in its
band; what it can reject depends on the chirp, which is the verdict's
business, not the delivery's. Off the band a transmission contributes
nothing.

**Can decode** is narrower. For each other station, for each of its slots: it
must have a finite loss in the table, it must not be transmitting itself, the
slot must have last said `RX` or `CAD` (channel activity detection), its stated `bw`, `sf` and `sync` must
equal the transmission's, its `freq` must be within a quarter of its
bandwidth of the transmission's, and the frame must clear the demodulation
threshold. The matching keys are one constant — the thing to extend when the
medium learns to care about coding rate, header type or preamble length.

The carrier is matched within a tolerance, not exactly, because the
synthesizer steps in 32 MHz / 2^25: two drivers asked for 869.525 MHz round it
to register values tens of hertz apart (RadioLib lands on 869 524 963 Hz, the
berlinmesh driver on 869 524 999), and an exact match makes two stations on
one channel deaf to each other. A quarter of the bandwidth is what a LoRa
demodulator tolerates.

A slot in `CAD` is sensing, not receiving. It must be told a frame is
arriving, or a channel activity detection is blind to every frame that starts
inside its window and carrier sense says the wrong thing exactly when two
stations contend. It must not be told how the frame ended: it demodulated
nothing, so there is no verdict to rule, and a reception recorded for it would
draw a green flash for a station that only listened for energy. So its
`rx_begin` carries `"cad": true` and no `rx_end` is scheduled; whether the
slot was sensing is decided at the frame's start, from its last `state`.

## Carrier sense

A CAD slot is told of a frame when that frame is decodable there — an SX1262
CAD correlates for chirps of its own spreading factor and bandwidth — or
when the summed level of everything on the air in its band at that instant
crosses the **sense threshold**. That threshold is the one the European
Telecommunications Standards Institute's (ETSI) EN 300 220-1 V3.1.1 sets for
clear-channel assessment (clause 5.21.2, Table 45): 15 dB above the receiver
sensitivity limit of Table 32, 10·log10(BW in kHz) − 117 dBm, which is −81
dBm at 125 kHz. It is what a listen-before-talk on the received signal
strength indication (RSSI) acts on, and it lets a strong frame at another spreading
factor make the channel busy, as it does on a bench.

An RX slot gets the same energy begins for the frames it is not decoding, so
the chip's instantaneous RSSI and a CAD it starts straight after RX read the
same air.

## Reception: two tests, the worst piece deciding

Per receiver, per frame it is locked on to, at that frame's `rx_end`. The
frame's air is cut at every instant an overlapping transmission starts or
ends, so the set of transmissions on the air is constant within each piece,
and in every piece:

1. **signal over thermal noise** at or above the spreading factor's
   demodulation threshold;
2. **signal over each class of interference** at or above that class's
   rejection figure. A class is every overlapping transmission at one
   spreading factor, and its powers are summed, in milliwatts, before the
   test. The frame's own spreading factor needs 6 dB; another needs the
   inter-SF figure, which is negative: a frame at SF12 survives an SF7
   interferer 25 dB louder than itself.

A single failing piece makes the frame `crc`. The two tests are not one
weighted sum: the demodulation threshold is against noise and is negative,
the same-SF figure is against a chirp and is positive, and adding noise to
a chirp's power would make neither mean what its source measured. Nor are
the classes summed into each other: each figure was measured against one
interfering spreading factor, and orthogonality between two others says
nothing about their sum.

This is the difference between a medium and a referee. A collision is not a
property of a transmission — it is what happened at one antenna. Two stations
that cannot hear each other transmit over one another constantly; the station
between them keeps whichever frame is loud enough to be worth keeping, a
station out of one transmitter's reach never notices the collision at all,
and a receiver that hears three weak neighbours at once can lose a frame
that any one of them alone would have left it.

The interference list is held on the frame rather than recomputed from the
frames still in flight, so a reception that ends long after its interferer has
been pruned still knows what spoiled it. Scheduling an `rx_end` does not settle
it: a frame still in the air when a second one starts is spoiled retroactively
for receivers already told it was arriving, which is exactly what a radio does.

A receiver that starts transmitting while a frame is arriving loses it,
whatever else is on the air: the radio is half duplex.

## The figures

Every figure the verdict uses is in one table at the top of `ether.py`, with
its source beside it:

| Figure | Value | Source |
|---|---|---|
| thermal noise | −174 dBm/Hz | kTB at 290 K |
| noise figure | 6 dB (setting) | the SX1262's order of magnitude |
| demodulation threshold | −7.5 dB at SF7, 2.5 dB lower per step | SX1261/2 datasheet |
| same-SF rejection | 6 dB | Semtech's specification, as Croce et al. quote it |
| inter-SF rejection | −8 … −25 dB | Croce et al., IEEE Comm. Letters 22(4), 2018, Table II (SX1272) |
| sense threshold | 10·log10(BW/1 kHz) − 102 dBm | ETSI EN 300 220-1 V3.1.1, 5.21.2 |

Croce et al. measure the same-SF threshold at about 1 dB; the medium uses
Semtech's 6 dB, because it is the chip's own figure and because the verdict
takes the worst piece of a frame, not its average. SF5 and SF6, which the
SX1262 has and the SX1272 measurement does not, take the SF7 row and column.

## Why the ether owns the lock

A receiver follows one frame at a time, and which one is decided at the
preamble: a decodable frame takes the receiver when it is not demodulating
another, or when it leads the one in progress by the same-SF figure, and
then the earlier frame is lost there. The chip model has no lock rule of its
own. It follows the frame the ether last began on it; a frame the receiver
does not lock on to is sent as energy, an `rx_begin` marked `"cad": true`,
and its `rx_end` — `crc`, so the map still shows the collision — is one the
chip drops, because it is not the frame it follows.

The lock is here because only the ether sees the sum. A chip told of frames
one by one would compare each new one to the one it has and nothing else,
and would decide differently from the verdict the ether then gives it.

The lock is released when the frame ends, when its `rx_end` goes out, when
the slot states any mode but `RX`, and when the station transmits.

## The pairwise rule

`Ether(pairwise=True)`, or `--pairwise`, rules the way a simpler medium does,
on the same levels from the same table: a frame is delivered where it matches
and clears the demodulation threshold; a later frame takes the receiver only
when its level, rounded to the whole dB the station is shown, leads the one
in progress by 6 dB; and a frame survives only by leading each interferer on
its carrier that this receiver could decode at the interferer's own settings
by 6 dB, one at a time, with nothing summed and no spreading factor spared.
CAD is told only of frames it could decode. It is there so a run can be
compared, frame for frame, with one ruled that way.

Matching is on the **last stated** values, not on anything the ether infers.
This is why a station publishes a `state` on every command that changes its mode
or carrier, and why a model that forgot to would go deaf silently. The one thing
the ether does infer is a transmitter's own deafness: a `tx` is not a `state`,
so a station that never said it had left `RX` would otherwise be told about
frames arriving during its own, and a hidden terminal would look like a station
ignoring what it could hear.

## A frame's two names

A station numbers its own transmissions and knows nothing of anyone else's, so
two stations can have frames in the air under the same number — which happens
the moment a testbed is restarted, since every station starts counting again. So
the ether gives each frame a number of its own and uses that one in `rx_begin`
and `rx_end`. A receiver following one frame while a second arrives, and a
reader matching an end back to its beginning, both need a name that is unique
across the air, and only the medium can issue one.

## Delivery

`rx_begin` goes out immediately, so the receiver can arm its preamble, sync and
header interrupts on the offsets. `rx_end` is a timer at the frame's stated
span. Nothing re-reads the frame in between — a receiver that leaves `RX`
mid-frame is not told, and discards the reception itself.

Both carry the link's level: `rx_begin` as `level`, which is what an
instantaneous RSSI reads and what carrier sense acts on, and `rx_end` as `rssi`
with `snr` the same figure above the noise floor. Both are rounded to whole dB
on the wire, because the station reads them as integers; the lock and the
verdict are decided on the unrounded figures, so a frame 6.4 dB up takes the
receiver and one 5.6 dB up does not, whatever the numbers the receiver is
shown. The pairwise rule's lock is the exception: it compares the rounded
figures, as a chip comparing the integers it was given would.

The payload is passed through as the base64 string it arrived as. The ether
never decodes it, which is what keeps it honest: it cannot accidentally know
anything about Reticulum.

## Events, and the rule about subscribers

`on_tx`, `on_rx` and `on_station` exist so something can watch the air without
reading the record — the map, at sixty frames a second. They are plain
callables, and an exception from one is logged and swallowed. A page that has
gone away, or a subscriber with a bug in it, must not be able to stop the
medium: the ether's job is the frames, and everything watching is optional.

## What is deliberately not here

- **Fading and per-frame variation.** A pair's loss is the table's and is
  the same for every frame between them. No shadowing beyond what the table
  was computed with, no multipath, no antenna pattern, no rain.
- **The CRC band.** The threshold is the spreading factor's own, and above it a
  frame is delivered. A real receiver also has a few dB above that threshold
  where a frame locks but fails its CRC at a probability. `welcome` already
  carries a `seed` so that band, when it arrives, has a reproducible generator
  to draw from.
- **Bandwidth and offset in the rejection figures.** The inter-SF figures
  were measured at one bandwidth on one carrier; a transmission at another
  bandwidth, or partly overlapping the band, is classed by its spreading
  factor and counted at its whole power.
- **A referee.** The ether does not judge a station's behaviour — it does not
  check that a transmission was preceded by carrier sense, or that a duty cycle
  was respected. The record is there so something else can.
