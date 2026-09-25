# The simulated testbed — internals

Why the testbed is shaped the way it is, and the rules anything added to it
has to obey. [README.md](README.md) is how to run it and where every file
lives; this is the reasoning underneath.

## The idea in one paragraph

A station is a whole firmware, compiled for Linux and run as an ordinary
process. The cut between the real code and the simulated part is the **SPI
bus**: everything above it — the LoRa driver, its carrier sense and airtime
accounting, the Reticulum stack above that — is the same source that runs on a
board, and what sits below it is a model of an SX1262 that hands its
transmissions to a medium instead of to an antenna. Two firmwares meet on one
medium this way, each above its own driver:

```
reticulous (ESP-IDF host target)            berlinmesh (Rust, std)
Reticulum / LXMF / web UI                   Node, LoRaIface
        │                                           │
   iface-lora, RadioLib                        Sx1262Radio
        │  RadioLibHal                              │  embedded-hal
   VirtualHal ── GPIO shim                     simesh-hal
        │                                           │
        └──────────── the chip model (radio/), linked by each
                            │  UDP, JSON
                        the ether                   who hears what, and when
```

## Why a host port and not an emulator

An emulator runs the image on a modelled CPU: faithful to the silicon, one
core per station, and slow. A host port compiles the same sources for the
machine you are on: fast enough that two dozen stations are nothing, and
faithful to nothing below the C. The trade is deliberate. What the testbed is
for is protocol behaviour across several nodes over time — announces, paths,
carrier sense, retries, messages — and none of that lives in the instruction
set. What it cannot catch is anything that depends on the chip being a chip:
timing at the microsecond, memory layout, cache behaviour, a peripheral's
errata.

## One process

`simd.py` is the ether, the stations, the proxy and the control server in one
asyncio loop, and the stations' ptys on a second loop in a thread of their
own (below). It could have been four processes talking over sockets. One is
better for one reason that matters:

> **Positions reach the medium by method call.**

Dragging a station on the map is `ether.place()` between two frames. There is
no wire to define, nothing to serialize, no version to keep in step, and no
window in which the map and the medium disagree about where something is. The
ether keeps a `main()` and a `--scenario` flag so it still runs alone for its
own tests, but the testbed never uses that path.

The price is that one crash takes the lot down. That is the right price here:
a testbed is a person's session, and a medium that outlived its stations, or
stations that outlived their medium, would be a worse thing to debug than a
process that stopped.

## Nothing blocks

Every wait is an awaitable. A framed-RPC query is a future the drain
completes, with a timeout, and the websocket handlers are coroutines. The one
rule that follows: **a subscriber must not be able to stop the medium.** The
ether calls its `on_tx`/`on_rx`/`on_station` subscribers inline and swallows
what they raise, because a page that has gone away is not the air's problem.

## The ptys have a thread of their own

In virtual time the main loop runs every barrier, and a barrier waits for the
slowest thing on that loop. So what the stations print is not read there:
every station's pty is a reader on a second event loop, in a thread of
simd's own (`stations.Ptys`, started with the first station), which reads
it, takes the framed-RPC replies out
(`rpc.FrameDemux`), appends the rest to the station's log and watches for the
capability marker. The main loop is handed only what it acts on, with
`call_soon_threadsafe` and in the order it was read: a reply for the station's
RPC client, the marker, the console bytes while somebody has the station's
console window open, and the end of the stream. The log file is the pty
thread's alone from a station's first start, so a restart's banner and the
last bytes of the process before it land in the order they happened; when a
start ends, the thread reads what is left on the pty before it closes it.

Both loops keep the rule above. The pty thread blocks in nothing but its
loop's own wait, and the main loop writes to a pty the way it always did,
non-blocking, with what the pty will not take held for its writer, so a frame
is never cut.

## One port, and `Host` decides

The control page and every station's web UI are on the same published port.
The front listener reads just enough of each request to find `Host`:
`<name|id>.sim.localhost` is proxied to port 80 on that station's own address,
and everything else is proxied to the aiohttp app, which is bound to a
loopback port nothing outside can reach.

That shape — a proxy in front of the control app, rather than the control app
routing to stations — is what keeps a station's URL space its own. The page's
websocket, the station's websockets and the station's absolute links all work
because after the head is read the connection is a raw byte pump and the
station is answering on its own origin.

A name resolves through the loaded scenario, a number through arithmetic. Both
because a scenario's nodes get renamed and a bookmark should survive it, and
because the proxy alone — for a station set somebody started another way — has
no scenario to ask.

## Two things worth keeping, and why they are not one thing

A network has a design and it has a history, and conflating them makes both
useless. A saved thing that always carried the stations' state could not
describe a network that has not happened yet; a saved thing that never carried
it could not bring one back.

So there are two. A **scenario** is the design — geometry, air, setup lines —
and is one readable file. A **snapshot** is a scenario plus every station's
state store. Loading a scenario is a factory reset of the whole testbed;
loading a snapshot restores a moment.

That split is what makes a scenario *reproducible*: load it twice and you get
the same network twice, because there is nothing in it that could have been
inherited from the last run. A scenario that quietly kept the previous
network's identities would behave differently on the second load than on the
first, and the difference would be invisible in the file.

Half of a network is its identities, keys, paths and message history, which is
why snapshots exist at all and why they are a directory rather than a file.
A snapshot is the stores, so what it brings back is what each firmware
reloads from its store at boot: a `reticulous` station's paths only when
`s.rnsd.dir.persist_routes` is on, and only as of its last directory write
([README.md](README.md#a-scenario-and-a-snapshot)).

## The run is a copy, and a snapshot is taken live

The stations run in `testbed/run/`, never in a scenario or a snapshot. Without a
working copy there would be no moment at which the thing on disk was not
already changed, since the stations are always writing.

A snapshot is taken **while the stations run**. Waiting for quiescence would
mean stopping a network to photograph it, and a testbed is watched rather than
batched. What makes that honest is the flush below; what makes it safe is that
the firmware's store commits whole files, which is a guarantee it already has
to meet for a power cut.

`run/` is overwritten rather than removed and remade, because the ether's
record is open inside it and belongs to the process rather than to any one
scenario. Logs and the record stay there and are never copied out: they are an
account of one run, and a scenario is what to run.

## Reset, factory reset, and why neither is called "apply"

Two verbs, and the difference is exactly the difference a person expects:

- **Reset** presses reset. The process exits, the supervisor brings it back,
  and the state store is untouched.
- **Factory reset** stops the station, deletes its `state/`, and starts it
  again. On the way back up the directory is empty, which is precisely what a
  node clicked onto the map for the first time is — so the setup lines run
  again by the ordinary path and not by a special case.

There is deliberately no verb that re-sends the setup lines to a running
station. "Re-apply" reads like a repair and behaves like one sometimes and not
others: a line that is a setting takes effect, a line that is an action happens
twice. What replaces it is **Run command**, which types one CLI line at every
running station and shows what each said. That is more versatile — retune the
whole testbed, survey it, create something on all of it — and it is honest
about being a thing you did rather than a state you restored.

## Why the store is flushed before anything is taken away

What follows is the `reticulous` kind's; a kind whose store writes through
(`berlinmesh`) has nothing to flush, and its `flush` does nothing.

`s.storage.flash_delay` is 60 seconds by default: a write sits in RAM for up to
a minute before the store commits. On a board that is a power-cut window and
entirely fair. Here it would make two things lie.

A station **reset moments after its setup lines ran** would come back with none
of them, and the testbed would be claiming a configuration it never made
durable. So simd sends `save` at the end of the setup lines, and again a few
seconds later — not everything setup asks for lands at once, and an LXMF
identity reaches the store some seconds after the command that created it has
returned.

A **snapshot** copied out of a store with a minute of writes still in RAM would
be a picture of a moment that never quite existed. So every running station is
flushed before the copy, and before any stop or reset.

All of it is best effort with a short timeout. A station that will not answer
is one whose store cannot be flushed, and refusing to stop it over that would
be worse than losing the last minute.

`save` is simd's to send rather than the file's because it is not a setting.
Everything in the file describes what a station *is*; `save` is about making
that description stick.

## Setup lines, and the macros

A scenario configures its stations by typing at them. The alternative — a
schema of settings the testbed knows the names of — would have to grow every
time the firmware grew one, and would be a second place for a setting's name to
live.

The lines are expanded per station: `{name}`, `{id}` and `{addr}`. That is what
lets one shared list say node-specific things, and it is why simd adds no
settings of its own. The name is the node's identity in three places at once —
the map label, the proxy hostname and the station's own name — and must not
drift; the macro gets that guarantee without simd typing anything: `hostname {name}`
is one shared line, visible in the file and editable, and it cannot disagree
per node because there is only one of it.

A macro the list does not define is left exactly as written. A CLI line is
somebody's text and may legitimately contain braces, and silently emptying
something that only looked like a macro is worse than passing it through for
the station to complain about.

The lines run only on an empty store, which is what makes it safe for them to
contain `lxmf create {name}` — a line that is emphatically not idempotent.
Nothing re-runs them against a configured station; a factory reset empties the
store first.

Whether a station has been set up is sampled **at the fork**, not when the CLI
answers: the firmware writes `state/boot` moments after it starts, and the
answer the setup step needs is the one from before it ran.

## The WebRTC relay, and why it is a relay rather than a second transport

A station's web UI reads every `s.*` value over a `storage:1` WebRTC
DataChannel. There is no HTTP path for it, so without a DataChannel the page
loads and then knows nothing: no hostname, no settings, no Activity.

The obvious fix was a host-only WebSocket transport carrying the same
merge-patches. It was the wrong one. A transport that exists only in the
testbed is a code path a board never runs, so the thing being tested stops
being the thing that ships — which is the one promise this testbed makes.

So the station speaks real WebRTC here, from the same source, and what is
sim-only is the plumbing that makes it reachable:

- **Signalling.** simd keeps `/webrtc` for itself — the one station route the
  front listener does not forward — and terminates the WebSocket on both
  sides, so the SDP answer arrives as a parsed message. It rewrites the
  connection line and candidate to the relay's own address and drops the
  station's, which point inside the container and would only cost the browser
  timeouts. The browser's cookie goes up with it, because signalling is behind
  the station's own login and a relay that dropped it would be introducing a
  stranger.
- **Media.** One UDP port in front of every station. The first packet of an
  ICE session is a STUN binding request whose USERNAME begins with the
  answerer's ufrag — the same ufrag simd read out of that station's answer —
  so the packet says which station it belongs to without simd having to
  understand anything else about it. After that the browser's address is
  pinned to a flow with its own socket, and both directions are forwarded
  bytes-for-bytes.

Nothing is decrypted or inspected past the STUN username: DTLS and SCTP are
end-to-end between the browser and the station exactly as on a board.

What this cost the firmware is three functions —
`webrtc_port.{h,cpp}`: the local addresses to advertise, the address the socket
binds, and a CRC32 the chip has in ROM. The rest of ICE, DTLS and SCTP built
for the host unchanged. The bind is the one that matters and is easy to miss:
a chip has a network stack to itself and binds the wildcard, while here every
station is a process on one stack, so each binds its own loopback address or
the second one to start finds the port taken.

## Status, and what `up` means

`stopped` → `starting` → `setup` → `up`, with `restarting` for the gap after an
exit nobody asked for. `up` is **the station answering the door its kind
talks through** — a `reticulous` station's framed RPC, a `berlinmesh`
station's `rncfg detect` — not the process existing: a firmware process that has forked
but not finished booting is not a station you can do anything with, and the
map should not claim otherwise.

`transport` is not status and is not read from the scenario. simd asks each
running station every few seconds, through its kind, because the setting is
live and a person can flip it on the station itself — the map should show
what the station thinks, not what the scenario last said. A kind that cannot
be asked shows it as unknown.

## Kinds, and the rules that come with more than one firmware

Everything the testbed knows about one firmware lives in its kind
(`testbed/kinds/`); simd, the supervisor and the page know only the kind's
methods. The station contract ([STATION.md](STATION.md)) is what every kind
shares, and it is small on purpose: an identity, a directory, an address and
the ether, in `SIMESH_*`, and a console on stdin/stdout.

**A kind supplies what its firmware reads.** A firmware that reads other
names for the contract's values gets them from its kind's `env`, beside the
contract's own; the contract does not grow to fit one firmware.

**A shared line is in one dialect.** The scenario's `setup:` goes to nodes of
the first kind only, and **Run command** goes to one kind at a time. The same
text typed at another firmware means something else or nothing, and a testbed
that sent it anyway would be reporting an answer to a question it never asked.

**Whether a station is set up is the kind's to say, and it is sampled at the
fork.** Each firmware leaves its own mark in `state/` on a first boot, moments
after it starts; the answer the setup step needs is the one from before it
ran. Get it wrong one way and the lines run on every restart, the other way
and they never run.

**One conversation at a time on a station's door.** `rncfg` opens the
station's KISS pty per command; two at once interleave their frames and both
read garbage, so the `berlinmesh` kind holds a lock per station around every
invocation, the transport poll included. A `reticulous` station runs its
framed-RPC frames one after another and answers each with the id it was sent;
two queries in flight whose ids collided would each take the other's answer,
so its client holds a lock per station around every frame.

## Framed RPC, and why the testbed does not use the TCP CLI

A `reticulous` station is set up and asked things over framed RPC on its
console pty, the channel flashmon uses on a board's USB console. The TCP CLI
would work, but on a board it is closed until someone opens it, and a testbed
that needed it open would need the firmware to behave differently here; the
console is there from the first instant on both.

The pty drain is a state machine rather than a search, because a read can
end anywhere: in the middle of the magic, of a header or of a payload. Bytes
that might be the start of a frame are held until the next read settles them;
a false start gives them back to the text, first byte first, and reads the
rest again, because a magic can start inside a false one. A frame that is not
one — an id outside the range a query uses, a magic inside a payload, or a
remainder that has not arrived within a second — is given back the same way.
The device writes a whole reply under the console's write lock, so none of
that happens in a healthy run; it is there so that a line of noise costs one
answer and not the stream.

The device bounds a command at five seconds and cuts a reply that outgrows its
buffer at the last complete line, without saying so. A reply is therefore not
proof that a command finished: a line that came back at the bound may still
be running, and the next frame would find the command line busy. So a slow
line is followed by a question that confirms it landed, and a caller asks for
one key at a time rather than a subtree whose tail could be cut.

**Ids are unique across kinds.** The ether keys stations by id; two processes
answering under one id are one station to the medium, and two sockets on one
address. The scenario refuses a file that repeats one.

## The page

One Pinia store owns the websocket and everything on it. Every component reads
the store; every action is one store method that sends one message. A
reconnect replays the `snapshot`, so the page holds no state simd cannot
restate — which is the whole of what makes simd restartable under a page that
is open.

The map is a **canvas**. A grid, hundreds of pulses a minute and a drag at
60 Hz are all much cheaper drawn than laid out, and none of them wants to be
an element. The projection is the ether's own, equirectangular around the
scenario origin, so a pixel and a metre agree by construction rather than by
two implementations staying in step.

A drag sends `node_move` at a few Hz with `settle: false`, and once more on
release with `settle: true`. The ether is updated on every one, so a link
visibly fades as a station is pulled away; the scenario file is written only
on the last, so a drag is one edit rather than fifty.

Frames arrive as `tx` and `rx` and are drawn on the **browser's** clock: the
ether's microseconds are its own, and the only thing in a `tx` that means
anything here is how long the frame occupies the air. In a virtual-time run
that span is divided by the run's pace — the pace asked for, or for `max` the
pace the last `clock` message observed — so a ring lasts the frame's time on
the air as the run experiences it.

## What a reticulous station has instead of hardware

| | On a board | Here |
|---|---|---|
| identity | the chip's MAC | the node id, from the environment |
| `/fixed` | a read-only image in flash | a link to the build's merged data tree |
| `/state` | LittleFS on a partition | a directory the process `chdir()`ed into |
| NVS | a flash partition | a file under `/tmp`, sized from the built partition table |
| addresses | WiFi | one loopback address per station |
| console | a serial port | a pty, bridged to the map's terminal window, carrying framed RPC as a board's USB console does |
| radio | an SX1262 | a model, and the ether |

The pty matters: a station's stdin and stdout **are** its serial console, so
the supervisor holds the master end and the console window is that pty over a
websocket. Keystrokes go as binary frames and the terminal size as a JSON text
frame, so no byte a person can type is special to the transport.

## The one rule that makes interrupts real

This one is the reticulous firmware's, whose driver waits on DIO1; a driver
that polls the IRQ register over the bus never meets it.

The GPIO shim ([`hw-linux`](../hw-linux/README.md)) is a pin table, and the
whole reason it exists is a single behaviour:

> a level-triggered pin whose interrupt is enabled while its line is asserted
> fires immediately.

That is the property the LoRa driver's interrupt handling rests on — the
trampoline disables the interrupt, the task drains whatever raised it, and
re-enables; a line still high re-fires. Without it, a frame that completed
behind a disabled interrupt would be a hung task here and a serviced one on
hardware, and the testbed would be lying about the one path it most needs to
tell the truth about.

A handler runs on whichever task moved the line, which is what an interrupt
does. The shim reads and writes its table under a critical section but calls
the handler outside it: a handler ends in a yield, and on this port a critical
section is a per-thread signal mask with a global nesting count, so yielding
from inside it hands the section to the wrong thread.

## The seam: a bus, not a chip class

The model implements the **wire**, not the driver's idea of a radio. A
transmission arrives at it as a byte frame with an opcode, exactly as the
driver would put it on a bus, and the reply comes back as the status byte and
the data the datasheet describes. That placement is what gives the testbed its
value: the driver's own command sequences, its IRQ masks, its read-modify-write
of the sensitivity register and its interrupt handling all execute, unchanged
and unaware.

The model is deliberately shallow where depth would buy nothing: mode
transitions are instantaneous, BUSY is never busy, and the GFSK and LR-FHSS
modems and duty-cycled receive are refused. A `ready_at` field rides on
the wire from the start so the datasheet's timing table can be added later
without moving anything else.

Three things it is **not** shallow about, because everything above the bus
reads them:

- **A frame takes its time on the air.** The transmit timeline is the
  AN1200.13 time-on-air for the modem as configured, so a 250-byte frame at
  SF8/BW125 occupies the medium for two thirds of a second, carrier sense has
  something to sense, and two stations can be talking at once. A model that
  finished a transmission the instant it started would make every collision
  in the testbed impossible, and it would do it silently.
- **A receiver follows one frame at a time.** The medium reports every frame
  that reaches the antenna; the chip locks onto the first, and a frame that
  starts while it is demodulating is not received at all unless it leads the
  one in progress by the capture margin, in which case the receiver drops
  what it had and takes the louder one. Without that the driver would be
  handed whichever frame ended last, and the medium's verdict — which says
  one of the two survived — would mean nothing above the bus.
- **Channel activity detection answers.** `SetCad` runs for the symbols
  `SetCadParams` named, then raises `CAD_DONE`, with `CAD_DETECTED` when a
  frame this antenna has been told of is still on the air. A driver whose
  carrier sense is CAD waits for that answer and treats silence as a busy
  channel, so a model that accepted `SetCad` and never answered would make
  every transmission of such a driver fail seconds late, and it would look
  like a dead radio.

## The chip library, and the rules it keeps

The model and its ether link are one C++ library behind a C ABI
(`radio/include/simradio.h`), reaching the host only through a table of
services (`radio/src/services.h`). A station of any language links it. Each
rule below is a way the model breaks when a backend or a caller gets it wrong.

**The lock is recursive.** A timer callback takes the lock, and what it calls
can take it again; a plain mutex deadlocks on the first received frame.

**Pin callbacks and timer callbacks run with the lock released.** A host's
DIO1 callback may run a driver's interrupt handler on the spot, and that
handler issues SPI commands, each of which takes the lock. So the model
decides the line's level under the lock and calls the host after letting go,
and a backend's timer thread holds nothing when it calls in.

**Starting a timer that is running restarts it.** The receive timers are
re-armed for every frame; a start that was refused because the timer was
already armed would fire on the previous frame's schedule.

**The air is the antenna's, not the mode's.** What a CAD detects is any frame
this antenna was told of that has not yet left the air, whatever the chip did
in between: a driver goes RX, then standby, then CAD, and the frame it was
hearing is still there when the CAD looks. What the demodulator and the
instantaneous RSSI read is cleared on leaving RX, as a chip clears it.

**The medium tells a station in CAD about a frame, never how it ended.** A
CAD needs to learn of frames that start inside its window, or carrier sense
is blind exactly when two stations contend; a CAD demodulates nothing, so an
`rx_end` for it would be a reception that never happened.

**Close detaches, it does not free.** A slot's chip lives for the process,
because a timer may be about to fire on it; `simradio_close` stops its timers
and drops the host's callback, and opening the slot again powers it up fresh.

## Time

A run keeps **real time** or **virtual time** (`simd --time real|max|<k>x`),
for every station alike.

In real time `esp_timer` is `CLOCK_MONOTONIC` in microseconds from the first
reading, and every timed event in the model — the instant a preamble ends, a
header lands, a frame finishes — is a one-shot on the backend's timer. The
FreeRTOS tick is 100 Hz while a task runs and stops while every task is
blocked, so nothing is accurate below ten milliseconds; the
frames the driver sends take tens to hundreds of milliseconds, which is why
that is survivable. A station's `t` fields are its own clock, meaningful only
against each other inside one message, and the ether rebases every frame onto
its own clock before scheduling.

In virtual time the ether is the **conductor**: it owns conductor time T and
moves it only when every station has said it is idle
([`ether/INTERNALS.md`](ether/INTERNALS.md#the-barrier)). A run is then
limited by the work the stations do, not by the air: a quiet stretch of an
hour costs what the stations' timers cost to run, and a busy host slows the
run down instead of changing what happens in it. `max` goes as fast as that
allows; `<k>x` paces T at k times the wall clock, so a person can watch.

```
ether        welcome {t, mode: virtual, …}   the station's clock starts at T
station      runs until every thread is blocked, then  idle {seq, until}
ether        every station idle: T → min(until, the air's next instant)
ether        run {t} / rx_begin {t} / rx_end {t}      to each station due
station      conductor moves T, runs its timers and wakes due at T, owes an idle
```

**The station's side is `radio/src/conductor.cpp`**, in the chip library, so
every kind has it by linking `radio/`. It keeps the last T granted, runs the
model's timers and the host's **wakes** when a grant reaches them, works out
the next instant the station needs (`until`) and sends the idle. The model
reads T; the host reads **node time**, f(T), which is where a node's own
crystal — drift, an offset — goes. f is the identity unless the station's
environment has `SIMESH_CLOCK_PROFILE`, a piecewise-linear map given as
`T:node` pairs in microseconds, both increasing, slope 1 outside them
([STATION.md](STATION.md#the-environment)); `nodeOf` / `conductorOf` are the
only place it is defined.

**The C library's time is answered by a preloaded shim**,
`radio/build/libsimclock.so` (built from `radio/shim/simclock.c`), which every
station of a virtual run is started with (`LD_PRELOAD`, `SIMESH_TIME=virtual`,
`SIMESH_EPOCH_US`). The chip
library finds it by name when the station opens its link and hands it the
clock (`include/simclock.h`); from then on `clock_gettime`, `gettimeofday`
and `time` read node time (plus the run's epoch for the wall clocks), and
every sleep, `setitimer`, `poll`/`select`/`epoll_wait` timeout and
`pthread_cond_timedwait` ends when node time reaches it. A waiting thread
blocks on an eventfd of its own, which a wake writes, so a signal still ends
its wait exactly as it ends a real one. Two rules keep it honest:

- **Every wait's end is rounded up to a whole millisecond of node time.** A
  driver that spins on microsecond `nanosleep`s makes each one a barrier;
  rounded, they share one, and the ends of many threads' waits on many
  stations fall on the same instants.
- **Every C library function the shim wraps is resolved in its constructor**,
  before `main()`. A thread switched out by a signal inside a lazy `dlsym`
  holds the dynamic linker's lock, and the next thread to resolve a symbol
  waits on it for good.

**Idle is the station saying every thread is blocked**, and each kind has a
way to know it:

- a `reticulous` station on `hw-linux`: FreeRTOS's tickless idle
  ([`hw-linux`](../hw-linux/README.md#the-tick)), which runs only when every
  task is blocked and knows the tick the first of them is due at. The
  board's clock hooks ([`hw-linux`](../hw-linux/README.md#the-stations-clock))
  are implemented by `radio/backend/esp-idf/services.cpp`: `esp_timer` counts
  node time from the whole second of it in which the station joined, its next
  expiry and the next tick a task waits for are wakes, so `until` is the
  earlier of the two, and the link opens at board bring-up, since nothing in
  the station can wait on time before the ether has said what T is. The tick
  is not a timer: the board steps the tick count to node time every time T
  moves (`simradio_on_advance`), before anything due at the new T runs, and
  the port's `setitimer` is stopped. A station whose tasks sleep for a second
  wakes the run once in that second, not a hundred times; and because every
  station's clock is a whole number of seconds from every other's, the ticks
  of all of them fall on the same instants of T and share their barriers.
- a `berlinmesh` station, whose threads are plain pthreads: the shim's thread
  census (`SIMESH_IDLE=threads`). A thread counts as blocked while it is in
  one of the shim's waits, an untimed `pthread_cond_wait`, or a read on a
  blocking descriptor; when the last one blocks, the station is idle.

Neither can be told apart from a thread that is simply slow, so the
conductor also has a **busy watchdog**: a station that has not said idle
20 ms of wall time after it was last told anything, or last sent the ether
anything, says so anyway, with the
`until` it has — for a `reticulous` station, whose `until` is the next tick
while a task runs, a tick at a time. That is what keeps a thread spinning on the wall clock, or a
host descriptor nobody is watching, from stopping T. It also fires on honest
work that takes longer than 20 ms of wall — key generation at first boot,
a signature check under load — and then T moves on while the station is
still computing, so that work lands later in T by the ticks it spanned. How
often that happens depends on the host's speed and load; the `clock` message
counts it as `slow_idles`, and simd logs the stations concerned when the
run's pace drops below 2x.

**What comes from outside the run arrives between barriers.** A command
typed at a console, a framed-RPC query, a TCP connection: the ether knows
nothing of it, so T may move while a station deals with one, and a `tx` it
causes is stamped with the T the station last had and goes on the air at the
T the ether has. The testbed's own waits between commands (`Kind.pause`,
`simd`'s `sleep` and `after`) are on T, so a setup script and a driven run
land at the same instants in T whatever the pace.

## The rules a host-only file obeys on ESP-IDF's host target

Six, for the reticulous firmware and for `radio/backend/esp-idf`, and they
are not negotiable — each one is a way this port breaks.

**No FreeRTOS task blocks in a host system call.** The port only knows a task
is blocked when it blocked on a FreeRTOS primitive; a task sitting in `recv`
is, to the scheduler, the running task, and it starves everything below it.
So sockets and stdin are non-blocking, and the only waits are `select()`,
FreeRTOS primitives and `vTaskDelay`. `select()` is hw-linux's: it blocks the
task until a descriptor is ready, woken by a signal that lands on the running
task's thread the way the tick does ([`hw-linux`](../hw-linux/README.md#waiting-on-a-descriptor)),
so a quiet station's tasks sleep rather than poll. No busy-waiting.

**Nothing wakes at tick rate while nothing happens.** The kernel is tickless
([`hw-linux`](../hw-linux/README.md#the-tick)): while every task is blocked
the tick stops, and a station costs nothing until the first of them is due.
A task that loops on a one-tick delay or a one-tick timeout keeps the tick
running, and in a virtual-time run wakes the whole run every 10 ms of T. A
task waits on what it serves — its notification, `hwLinuxWait()` on its
descriptors and its inbox — for as long as nothing comes, and a timeout is
the time something is actually due, rounded **up** to a whole tick: rounded
down, a deadline inside the current tick is a wait of zero, and the task
spins until it comes — on a chip for up to a tick, and in a virtual-time run
until the busy watchdog lets T move, 20 ms of wall each time. Where the shared source polls on a chip,
the host's behaviour goes behind the board's weak `hwLinuxWait` or into
`src/host/`, and the chip's path stays as it is.

**The console writes with `write(2)`.** The tick signal can land inside a libc
call that is not async-signal-safe and switch to a task that makes the same
call. The log sink and the CLI assemble their line and put it on the
descriptor.

**Every task stack is at least 20 KB.** A task is a pthread and its stack is a
real mapping; the port's own floor is 16 KB and a host stack frame is several
times a Xtensa one. `spawnTask` raises anything smaller, and drops core
affinity — there is one core, and asking for the second is an assertion
failure.

**Chip-only code leaves, host-only code arrives.** Behind
`#if !CONFIG_IDF_TARGET_LINUX` or out of the source list; a separate file in
`src/host/` in preference to an `#ifdef` inside a function.

**The board is reached through weak symbols, never through a dependency.**
Host-only code keeps wanting the station's identity and addresses, and those
belong to the board straddle — which arrives with `--with` and is in nobody's
`requires:`. A platform or feature straddle may not depend on one. So a file
that needs `hwLinuxNodeId()`, `hwLinuxBindAddr()` or `hwLinuxEtherAddr()`
declares it `extern "C" __attribute__((weak))` with a sane default and lets it
resolve at executable link time. The same rule is why the chip model is
linked by the interface that drives it rather than by the board that wires it.

## What this cannot tell you

- Anything timed below the tick, and anything that depends on the chip's own
  timing — BUSY after a wake, a peripheral's ramp, an errata.
- Memory: the heap ignores capabilities and wraps libc, so PSRAM pressure,
  DMA-capable allocation and internal-RAM exhaustion are all invisible.
- The radio's physics below the path-loss model. There is no fading, no
  antenna pattern, no CRC band above the sensitivity threshold and no noise
  that varies with what else is in the air; a level is computed once from the
  geometry and is the same for every frame between one pair. See
  [`ether/INTERNALS.md`](ether/INTERNALS.md) for what the medium does
  and does not decide.
- Anything below the C: the compiler, the ABI and the word size are the
  host's. Code that assumes a 32-bit `long` or pointer fails here and not
  on the chip, which makes the host build a free audit of width assumptions,
  and a clean run here is not a clean run on a board.
