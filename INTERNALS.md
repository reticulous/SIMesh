# SIMesh — internals

Why the testbed is shaped the way it is, and the rules anything added to it
has to obey. [README.md](README.md) is how to run it and where every file
lives; this is the reasoning underneath.

## The idea in one paragraph

A station is a whole firmware, compiled for Linux and run as an ordinary
process. The cut between the real code and the simulated part is the **SPI
(serial peripheral interface) bus**: everything above it — the LoRa driver,
its carrier sense and airtime accounting, the Reticulum stack above that — is
the same source that runs on a board, and what sits below it is a model of an
SX1262 that hands its transmissions to a medium instead of to an antenna. The
medium rules on every frame at every receiver from a table of every pair's
path loss, which follows from where the nodes stand on their geodata. Two
firmwares meet on one medium this way, each above its own driver:

```
reticulous (ESP-IDF host target)            sergeyculum (Rust, std)
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

The other ways there are were weighed and are not used. Wokwi runs in the
cloud on a budget of minutes, sandboxes custom chips with no sockets, and has
no diagram of several microcontrollers. A QEMU (Quick Emulator) fork with a
register-level SX1262 inherits the emulator's ceiling and adds a fork to keep.
Mininet-WiFi and wmediumd model 802.11 only, with no LoRa physics and no
virtual time.

## Its own launcher, and its own container

SIMesh simulates whatever firmware has a station kind. Reticulous is one;
Sergeyculum is another, and a Meshtastic or MeshCore node would be as much
its business. So it is not a verb of the tool that builds one of those
firmwares: it has its own launcher (`simesh`), its own image and its own
supply of prebuilt stations, and a clone runs with no firmware tree beside
it. A workspace is for building a station of one's own, into a catalogue of
its `builds/` like any other build; everything else about SIMesh is the same
without one.

**A build is a stamped device file, not a build tree.** What was last
compiled in a tree is anyone's guess, so a firmware that publishes simesh
builds is only run from a catalogue, where each build has a stamp and a
name. A compiled build (`devices/local/<project>_<catalogue>.yaml`, run in
place from its tree) stands in only for a project that publishes none yet,
Sergeyculum today, until it does.

**Stations need Linux.** Each binds a `127.x.y.z` address of its own and
the virtual-time shim is an `LD_PRELOAD` library, and neither exists on
another kernel. On Linux `simesh` runs natively; anywhere else it runs
itself inside its own image. That image is small on purpose and is not the
firmware build container: Ubuntu 24.04, because a device package's ELF
(executable and linkable format) binary is
dynamically linked against that release's C library, C++ runtime, zlib and
libbsd; the host's own architecture, because a package runs only on the one
it was built for; python3 with aiohttp and pyyaml, Node for the page, a C
toolchain and cmake for the chip library, cargo for the planner and the
`sergeyculum` kind. It copies nothing from the tree. `simesh` mounts the
directory holding SIMesh at its own path, so a compiled build's `../../../sergey`
resolves to the same files inside the image as outside it, and runs as the host's user, so what it writes is theirs. The
image is tagged by the Dockerfile's checksum and rebuilt when it changes.

**Stations come as device files** ([NODE.md](NODE.md)): the ELF, its
`/fixed` tree, any tools its kind needs and a `node.yaml`, zipped under the
catalogue's own naming with `hw-simesh-<arch>` as the board, and published by
the same catalogue build that publishes board images. A catalogue is already
how a build reaches people, with a stamp per build and a listing a program
can read; a second channel for the same firmware would be a second thing to
keep in step. The listing marks each link with its target, so a flasher
leaves the packages out, and SIMesh finds them by the entry's prefix. One
file format serves every firmware, so a kind knows nothing of where its
binary came from: a device imported on the page, fetched from a catalogue or
described by a compiled build's `node.yaml` resolves to the same executable,
`/fixed`, tools and environment.

**A device's name carries its catalogue.** One `make-builds` run stamps every
catalogue it builds with the same datetime, so `stable` and `dev` packages
of one run share a filename and differ only in where they came from. A
build is therefore named `<project>_<catalogue>_<stamp>`, and what a script
usually wants is `<project>_<catalogue>_latest`: the newest in that
catalogue, whichever that is when it is used.

**Latest is surveyed, and fetched only when used.** A catalogue publishes a
build a day or more; downloading each one ahead of need would fill a disk
with builds nobody ran, and keeping several per catalogue made every listing
a question of which one a name meant. So a survey reads the listings only
and notes the newest build per project and catalogue, with its `node.yaml`
read out of the zip (by range requests on the web: the central directory at
its end, then that one member) so a listing says what it plays without the
catalogue having to carry it; a `_latest` build is
downloaded the first time a simulation uses it, and removed as soon as a
survey sees a newer one, fetched or not, so there is never more than one per
catalogue and never a question of which. A build worth keeping is saved,
which is a copy under its stamped name that nothing removes. A run keeps
where each build was and, when that has been removed, resolves the name
again on resume.

**The port is 8800.** Round, and clear of everything else that runs beside
it: spangap's 9000–9011, the planner's 8787, the front's per-simulation
control and ether ports counting up from 9100 and 7100, a station's CLI
(command line) on 8081 and Reticulum's TCP (Transmission Control Protocol)
interface on 4242. It lives in
`simesh`, `front.py`, `proxy.py`, `test_front.py` and the docs, and nowhere
else.

## One process per simulation

`simd.py` is the ether, the stations, the proxy and the control server in one
asyncio loop, and the stations' ptys (pseudo-terminals, their consoles) on a second loop in a thread of their
own (below). It could have been four processes talking over sockets. One is
better for one reason that matters:

> **Losses reach the medium by method call.**

Dragging a station on the map recomputes its row of the run's loss table,
and the new tables go into the ether whole between two frames. There is no
wire to define, nothing to serialize, no version to keep in step, and no
window in which the map and the medium disagree about which row is current.
The ether keeps a `main()` that reads a nodeset and a
directory of tables, so it still runs alone for its own tests, but the
testbed never uses that path.

The price is that one crash takes the lot down. That is the right price here:
a testbed is a person's session, and a medium that outlived its stations, or
stations that outlived their medium, would be a worse thing to debug than a
process that stopped.

## Several simulations: a front and its children

```
browser/driver ── localhost:8800 ──► front.py ──► simd (lora)   127.0.0.1:9100, ether :7100, 127.16.0.0/22, runs/lora/
                                              └─► simd (supe)   127.0.0.1:9101, ether :7101, 127.20.0.0/22, runs/supe/
```

One simulation is one simd, and several are several simds, each a child
process of `front.py`, which owns the one port the container publishes.
Nothing is folded into one process, for two reasons:

- **The loop is the pace limit.** At a hundred stations one simd's event loop
  is what a virtual-time run waits on. Two simulations in one loop would each
  run at half the pace, and a slow one would slow the other. As processes they
  share only the host's cores.
- **A crash is one simulation's.** The paragraph above holds per simulation:
  a simd that falls over takes its medium and its stations with it, and
  nothing else. The front keeps the registry row, exited, with the child's
  last lines, until somebody stops it.

A child is a simd with the flags a person would give it by hand — its own
`--bind` on loopback, `--ether`, `--net` and `--run` — so everything that is
true of one simd is true of each child, and a child can be driven directly on
its own port. The front allocates those four so that two children never
share any of them, and checks that a port is free on the host before it hands
it out, so a simd started by hand on one is stepped around. A network is
given out only when no socket on the host is bound to any of its addresses
(read from `/proc/net/tcp` and `/proc/net/udp`) and the front wins a
non-blocking `flock` on `$TMPDIR/simesh-nets/<net>.lock`, held on an open
descriptor until the simulation ends and dropped by the kernel if the front
dies. The lock is what keeps two fronts apart in the window between giving a
block out and its stations binding it (the loss table and the child's start
come between); the bound check is what steps around a simd started by hand,
which takes no lock. The children's networks start at `127.16.0.0/22`, and
everything below is left to simds started by hand.

Each child has a process group of its own, which its stations inherit. A
child that dies without stopping its stations leaves them in that group, and
the front kills the group when it sees the child exit: stations left behind
would hold their addresses and their ether port against the next simulation
given them. Each child also asks the kernel to send it SIGTERM when the front
dies (`PR_SET_PDEATHSIG`), so a front killed outright does not leave
simulations running with nothing in front of them.

The front reads each child's output line by line, as it comes, into
`runs/<name>/simd.log` and a tail kept for the page. The line in which simd
says its control page is listening is what makes a child ready; there is no
polling of its port.

**The registry** is fed by one extra control socket per child, the front's
own, opened with `?quiet=1`: simd leaves `tx`, `rx`, `radio` and `levels` off
a quiet socket, and those are nearly all the traffic on a busy network and
nothing the registry reads. From the `snapshot`, `node`, `node_gone`,
`nodeset`, `clock` and `error` messages on it the front keeps what each
simulation loaded, its stations' statuses, its clock and its plan, and sends
the whole registry to every socket on the front once a wall second.

**A child starts on a whole run.** Before it starts, the front loads and
checks the geodata, the nodeset and the script, takes the
loss tables from the cache or computes them, and lays the run directory out;
the child then loads that directory as it would one it was handed by hand,
and settles each node's firmware from the run's rules (the script's, handed
over by its runner), fetching what they name, before a station of it
starts: a fetch is a wait the child's own loop can take.
All of that is work a person waits on with a page open, so it is the
front's, with its progress on every page, and none of it can stall a
running simulation's loop. The table is a subprocess (`losses.py`) whose
progress is read line by line, never a computation on the front's loop.

**Control through the front.** A page or a driver holds one websocket to the
front, and the front holds one websocket per child that socket uses. What a
child sends goes back with `"sim": <name>` spliced into the text rather than
parsed and re-encoded, because on a busy map that is thousands of messages a
second. A page selects one simulation and its socket to any other is closed,
so a page is never sent a map it is not showing; a driver names the
simulation in each message, or once in `?sim=`.

**Answers go to everyone, so an id is the asker's own.** simd broadcasts
each `command_result` to every page and driver on it, as it does everything
else. A driver matches an answer to its question by the `id` it sent, and
that id carries a random prefix of the driver's own: two drivers counting
from one would each take the other's answers.

**`sim` at the top of a message means a child said it.** The front splices
`sim` into everything a child sends, and a page and a driver tell a child's
messages from the front's own answers by it: an answer carrying `sim` is
never matched to the question it answers, and whoever asked waits forever.
So a front answer that names a simulation calls the field anything else
(`script_run` answers `simulation`).

**A run that ends pauses, it does not stop.** A simulation started for a
script's run is the run's, and when `main` ends the network it built (its
identities, paths, message history) is the thing worth keeping. The runner
asks the front to pause it; the front stops the child, which flushes every
station, and only then copies the run into its own `paused/`, as a snapshot
would be, since state copied from a running station can be half-written.
Resuming is a snapshot load into a new run directory, so the paused run
and its report stay as they were written. The runner, not the front,
pauses and then reports, so a script run from a shell ends the same way as
one run from the page; the front pauses only what a runner that died left
running.

**Scripts run beside the front and the simulation, never in them.** A script
is a process of its own (`simesh.runner`) that starts its simulation, or
attaches to one, over the front's port like any other driver; its output is
read line by line and sent to every page. A script that loops, blocks or
dies takes nothing else with it. What a station must be given at its first
boot, which happens inside simd whenever a station comes up with no state,
travels there as data (`on_first_boot()`'s rules: a selection and lines),
not as the script's code, so no script code ever runs in simd's loop.

**A script is synchronous.** It reads as it runs: `firmware(...)`, then
`exec(...)`, then `wait(60)`, each returning when it is done, with no
`await` to forget (a forgotten one silently does nothing) and no `async
def main` to wrap it in. The library holds the simulation on an event loop
of its own in a thread beside the script's and hands each call to it. What
a script loses is easy concurrency; what a driver needs of it, many sends
at their own instants, is `after=` and `wait=False`, and the traffic driver
keeps its own async core on the library's loop.

**What must happen at one instant is one message.** A driver's asks cross
a websocket to simd and its answers come back the same way, and in a
virtual-time run T does not wait for that: while every station is idle the
ether moves T straight to the next thing any of them needs, which can be
minutes on. A driver that asks a sender for its route, waits for the
answer, then asks it to send, sends minutes late, and its sends bunch into
bursts. So steps that belong together travel as one `sequence`
(`Sim.sequence`), which simd runs back to back at the T the first is due:
the traffic driver's route and send are one.

**Declarations start the simulation.** `time()`, `firmware()` and
`on_first_boot()` are collected until the script first does something, and
that starts its simulation with them, so a simulation is always started
knowing what its nodes run and in what time; `time()` after that is
refused, because the ether keeps one clock for its whole run.

**The estimate.** simd knows T and the pace but not what the run is for; only
the driver knows when its traffic hour ends. So a driver sends `plan`, its
phases and the T each ends at, and simd puts it in every `clock`. The front
keeps each child's T against the wall for the last two minutes and divides
what is left of the plan by that pace. Two minutes, because the pace of a
virtual-time run moves with what the stations are doing — a warm-up is slower
than a quiet drain — and a whole-run average would still be answering for
the first minute an hour later. A real-time run's pace is one.

## Nothing blocks

Every wait is an awaitable. A framed RPC (remote procedure call) query on a
station's console is a future the drain
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

In a virtual run the ether asks, before T moves past stations that have run,
for their ptys to be read to the end (`stations.catch_up`, on this thread);
the main loop hears it is done behind everything those reads handed it, so a
reply is acted on at the T it was printed at.

Both loops keep the rule above. The pty thread blocks in nothing but its
loop's own wait, and the main loop writes to a pty the way it always did,
non-blocking, with what the pty will not take held for its writer, so a frame
is never cut; in a virtual run the bytes wait there, too, until the ether says
the station is in step (`Ether.sync`).

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

A name resolves through the run's nodeset, an id through arithmetic. Both
because a nodeset's nodes get renamed and a bookmark should survive it, and
because the proxy alone — for a station set somebody started another way — has
no nodeset to ask.

The front is the same proxy with a different resolver. `alpha.lora.sim.localhost`
has two labels: the front routes on the second to that simulation's port and
forwards the request untouched, and the child's own listener routes on the
first to the station. A bare `alpha.sim.localhost` goes to the one running
simulation when there is exactly one, and otherwise is refused with the names
to choose from.

## Device, geodata, nodeset, script: split along what changes on its own

```
geodata ──┐
          ├──► loss table (derived, per band, cached) ── + antennas + offsets ──┐
nodeset ──┘  (positions, heights)                                               │
nodeset: antenna, role, radio, tags ───────────────────────────────────────────┤
script: firmware() rules, setup ───────────────────────────────────────────────┼──► simd ──► ether + stations ──► run
device files (by the rules' device names, fetched when used) ──────────────────┘
snapshot = geodata + nodeset + script + tables + rules + every station's store
```

A simulated network is several things that change for different reasons,
and each is its own file so that changing one leaves the others alone:

- a **device** is a station build. It changes when somebody builds one, and
  never because of where it runs;
- the **geodata** is the ground. It changes when the ground data does, and
  never because of a node. It names no node, and a nodeset names no geodata:
  a nodeset stands on any geodata whose extent holds one of its nodes, and
  synthetic ground lies at 0°, 0° so that a nodeset made on one synthetic
  ground stands on every other;
- the **nodeset** is which nodes stand where, with their maximum powers,
  antennas and tags, and nothing that is said to a station. What the page must know
  without running anything comes from tags and one shared file: a role tag
  (`transport`) is the transport's second ring, a `no-radio` tag a node
  without a radio, and `scripts/globals.py`'s radio the coverage, the links
  and the bands for every other node;
- the **script** is everything else the software is told, as Python,
  starting with what each node runs (`firmware()`): which firmware a
  network runs is the experiment, not the network, and the same nodeset run
  on Reticulous, on Sergeyculum and on a mix is three scripts of a line
  each, not three copies of every node. Its rules name nodes through tags
  and, as an escape hatch, by name, so one script fits any nodeset, and one
  nodeset serves scripts that differ by a few lines;
- the **loss table** follows from the geodata and the nodeset's geometry and
  from nothing else, so it is derived and cached under a hash of exactly
  those, and relabelling a node, changing its antenna, role, radio,
  firmware or offsets, or changing the script, never recomputes it.

**A firmware rule is a condition, kept.** `firmware(which, device)` holds
its selection as a condition over each node's facts (`simesh.select`), not
as the list of names it matches today, and the run keeps its rules. So a
node placed later or retagged runs what the rules say of it, and a rule
written at a script's top, before there is a simulation, means the same
thing whenever it is applied. The selection is Python's set algebra (`&`,
`|`, `-`, `~` over `nodes(field=value)`) because that is what a Python
reader already knows for "these and those", and a rule sent to a simulation
is that condition as plain data, which is why a function in it is refused.

**There is one board, and a node's maximum power says which way it is
built.** What sits between the firmware and the antenna connector is an
SX1262, and it is a node's property and not a build's: one device serves
every node. A node's `max_dbm` is its maximum power at the connector, 22 dBm
when it states none. At 22 dBm or below the node is a bare SX1262, whose
chip puts out that maximum itself; above it, up to 27 dBm, the node is an
SX1262 behind a GC1109 front-end module, as a Heltec V4 is, since no bare
SX1262 reaches past 22 dBm and the GC1109 is the front end the testbed has
measured. A figure above 27 dBm describes no node and is refused. A node is
sent at its maximum: the coverage draws it, and the startup script's radio
sets it (`tx_dbm="max"`) — said by the script, since a station is told
nothing behind a script's back. A lower power for one node is a lower
`max_dbm`, which the map then draws too. The station is told its board
(`SIMESH_BOARD`) by `testbed/boards.py`, whose front-end figures are the
Heltec V4 board straddle's own.

**A front end is modelled twice, with one curve.** The chip model
(`radio/src/model.cpp`) puts the board's transmit curve between what the
chip radiates and what the medium is handed, and adds its receive gain to
every level the chip reads, so the medium only ever deals in connector
power. Reticulous on `hw-linux` takes the same figures (`SPANGAP_BOARD`,
which the kind copies from `SIMESH_BOARD`) in place of its build's Kconfig
and converts the other way, antenna dBm to chip drive and chip RSSI to
connector RSSI, as it does on the board. Both round the curve the same way,
so a station asked for 27 dBm behind the GC1109 drives the chip at 18 and
the medium carries 27; a station told no front end is a bare SX1262, 22 dBm
at most. The node's maximum is the firmware's ceiling too, so a station left at its default power sends at its node's maximum.

**Antennas are a layer, like offsets.** A pattern's gain depends on the
direction to the other end, so a pair's gains are not one figure per node
but one per pair and direction, from the two antennas' tips in three
dimensions. They are taken off the model's loss when the tables are handed
to the medium (`losses.with_antennas`), as the offsets are added, so a new
antenna or a new aim never recomputes the model, and the medium still reads
one figure a pair. The tips need the ground under each node, which on a
pack is the terrain there, asked of the sidecar once per node and kept in
the run.

**Offsets are a layer, never baked in.** A nodeset's offsets (dB added to
one pair's computed loss, both ways) are where measurements correct the
model, and they are added when the tables are handed to the ether
(`losses.with_offsets`), in simd and in the analysis tools alike. The cached
table stays the model's own, so an offset is changed without a recompute,
and the model's error is the offsets themselves, to be driven towards zero.

A node is referred to **by name** everywhere, and its **id** is stored and
editable. The name is what a person means; the id is the station's network
identity, which its loopback address and MAC (media access control) address
follow from, so it has to be stable across saves and loads and unique across
kinds — two processes answering under one id are one station to the medium
and two sockets on one address. Changing it moves the station, which is why
the editor restarts a station with state when its id changes.

A network also has a history, and conflating it with the design makes both
useless: a saved thing that always carried the stations' state could not
describe a network that has not happened yet, and one that never carried it
could not bring one back. So the files are the design, and a **snapshot** is
a moment of a run: the geodata, the nodeset and the script as the run had
them, its loss tables, and every station's store. Starting from geodata,
nodeset and script is a factory reset of the whole network, reproducible
from files you can read; starting from a snapshot restores a moment, without
recomputing anything and without the planner.

A snapshot keeps its script's copy. Setup runs only on a station with no
state, so a script changed after the snapshot would never reach its
stations, and a station factory reset after the snapshot is loaded is set up
by the copy, the way the first run set it up; a changed script is a new
simulation from scratch.

Half of a network is its identities, keys, paths and message history, which is
why snapshots exist at all and why they are a directory rather than a file.
A snapshot is the stores, so what it brings back is what each firmware
reloads from its store at boot: a `reticulous` station's paths only when
`s.rnsd.dir.persist_routes` is on, and only as of its last directory write
([README.md](README.md#runs-and-snapshots)).

## A run is an output, and a snapshot is taken live

The stations run in a run directory, `runs/<name>/`, never in a nodeset or a
snapshot. Without a working copy there would be no moment at which the thing
on disk was not already changed, since the stations are always writing. The
run holds its own copies of everything the simulation reads — the geodata,
the nodeset, the script, the loss tables, the firmware rules and which build
each firmware resolved to — so that an edit made during the
run changes the run and not the files it started from, and so that the
analysis tools read the network as it was run and not as the files stand
now. A run is never written over: a new one goes beside it.

A snapshot is taken **while the stations run**. Waiting for quiescence would
mean stopping a network to photograph it, and a testbed is watched rather than
batched. What makes that honest is the flush below; what makes it safe is that
the firmware's store commits whole files, which is a guarantee it already has
to meet for a power cut.

Logs and the record stay in the run and are never copied into a snapshot:
they are an account of one run, and a snapshot is something to run.

**Nothing a run writes goes on a bind mount.** Stations' stores, the record
and the logs are written constantly, and a bind mount on Docker Desktop is
virtiofs: slow, with file semantics of its own that a store on flash never
meets.

**Two runs are compared by one version of the tools.** Every column of a
comparison is recomputed from both runs' records by the current analysis
code, never taken from an older report, so a difference between the columns
is a difference between the runs.

## Real ground: the planner as a sidecar

The ground data and the propagation model are SIMesh's own planner, the Rust
workspace in `planner/`: packs compiled from public terrain, clutter,
building and road data, and ITU-R (International Telecommunication Union,
radio sector) Recommendation P.1812-8 over a real profile. The crates came
from Sergey's planner and keep their names; SIMesh carries the ones it runs
(core, terrain, propag, opt, coverage, pack, buildings, import, render, web,
wasm, and its own job) and changes them as it needs, since SIMesh takes over
planning and simulation from the tools it grew out of, building the packs
included. The front runs the workspace's web server, `planner-web`, as a
**sidecar**, and asks it.

```
page  ── GET /planner/<geodata>/tile.bin ──► front ── GET /tile.bin ──► planner-web (that pack)
front ── losses.py ── GET /link.json?ax&ay&bx&by&tx_h&rx_h × pairs ──► planner-web
page  ── GET /planner/<geodata>/link.json ─► front ──────────────────► planner-web   the pair inspector
front ── GET /loss/start, /loss/status, /loss.bin, one node at a time ──► planner-web   coverage
```

- **A process, not a library**, because the planner is Rust and the front is
  Python, and what it serves over HTTP is exactly what SIMesh needs: tiles
  for the map, roads, buildings, places, and one pair's loss with its
  evidence.
- **One per pack in use**, because `planner-web` serves one pack. The front
  starts it on a free loopback port when the first simulation or page
  opens geodata on that pack and stops it when the last one lets go, so a
  front with no pack open runs none.
- **Behind the front's port**, as `/planner/<geodata>/…` with the prefix
  stripped: the page has one origin, `planner-web` needs no CORS
  (cross-origin resource sharing) handling it does not have, and only
  SIMesh's page calls it, so the planner's own page and its absolute paths
  never matter. The page renders the ground with the planner's own renderer,
  `planner-wasm`, from a copy vendored into the page's tree, so the page
  builds with no planner beside it.
- **Every cell is one `link.json`**, the same request the pair inspector
  makes, so a cell of the table is what the inspector shows for the same two
  nodes, near-field pairs included. `link.json` already composes
  everything a cell needs: the profile, the P.1812 call, a near-field model
  where the two are too close for P.1812, which model it used, and the
  Fresnel verdict. The planner's own pairwise sweep is not used: it drops
  every pair beyond a link budget and every pair under 250 m, and both are
  pairs the medium needs (below).
- **What the sidecar decides, and what it does not.** `link.json` takes no
  carrier and judges at the planner's EU868 parameters, 869.525 MHz, 50 % of
  time and 90 % of locations; a pack therefore has an 868 table only,
  and its header says so. A pair with an end off the pack is never heard
  here rather than asked, because the sidecar would clamp the point onto the
  pack's edge and answer for a place the node is not. A sidecar answers
  from the clutter raster alone until it has indexed the pack's buildings, a
  different number by tens of dB, so a table waits for the index before its
  first pair and throws away a reply given before it.
- **Coverage is the planner's own sweep, one node at a time.** A node's
  coverage raster is `planner-coverage`'s point-to-area sweep from its
  antenna, cut to a square around it by `loss.bin` and cached by the
  pack, the node's position and its height: path loss only, so a change of
  power, antenna or radio reuses it, and the page adds those, the antenna's
  gain toward each cell over the terrain under it, which it fetches on the
  raster's own grid. The sidecar holds
  one sweep and a new one cancels the last, so the front asks for one node
  at a time per sidecar; nothing is swept until a page asks with the layer on.
  The cache is keyed by what the sweep is asked, not by the code that
  answered, so a change to how the sweep computes empties
  `testbed/coverage/` by hand.
- **The sweep and `link.json` compose the terminal the same way.** For a
  terminal in the open below the clutter around it, P.2108 §3.1's
  correction A_h is to a loss computed from the clutter height, so both
  raise the terminal to that height on each bearing, run P.1812, then add
  A_h (`tx_model_h_m` beside `tx_terminal_db` in the sweep's parameters).
  Adding A_h to a run from the antenna itself charges the same obstruction
  twice.
- **An antenna inside a building is indoors, where it is.** One inside a
  footprint, below its roof, is not raised onto the roof and has no P.2108
  surroundings: both calculations take its own building out of its paths
  (the path starts inside it) and charge P.2109's median entry loss for a
  traditional building instead (`Indoor` in planner-web). Read as P.2108
  surroundings, a node on the ground inside the Fernsehturm's footprint was
  modelled from the tower's 253 m. `link.json` reports the antennas where
  they are (`tx_h`, `rx_h`), the heights the model ran at beside them
  (`model_tx_h`, `model_rx_h`), and each indoor end's entry loss, and its
  Fresnel check traces the real ray between the real antennas.
- **A snapshot carries its tables**, so it reloads without the planner, and
  a simulation on synthetic ground never needs one.

## Building a pack, and the node maps

```
front ── fetch into geodata/.cache/<source>/ ───────────► the sources' hosts (sources.py)
front ── planner-job pack-build, one JSON object on stdin ─► planner-job   (packbuild.py)
planner-job ── {"step","done","total"[,"part","parts"]} per line ─► front ── geodata_progress ─► pages
planner-job ── {"manifest": path} | {"error": sentence}, last ─► front: geodata/<name>/, its geodata.yaml
front ── planner-job nodes-import {source, file, bbox, companions, max_age_days, now_unix} ─► planner-job
planner-job ── {"nodes": [...], "report": {...}} ─► front ── nodeset.from_imported ─► nodesets/<name>.yaml
```

`planner-job` is one binary crate with two verbs, each one JSON object in on
standard input and JSON lines out on standard output, diagnostics on
standard error, exit 0 or non-zero with a last `{"error"}` line. It holds no
fetching: **the front fetches, the compiler compiles.** That keeps the
compiler testable from files, keeps every request to a public host in one
place (sources.py: one user agent, polite retries, a cache shared by every
build), and lets the front show a download's bytes and a compile's steps in
one row.

pack-build's steps, each only when it applies: `terrain`, `osm`,
`buildings`, `lidar`, `landcover`, `clutter`, `population`, `manifest`. Its
input names every file (the GLO-30 tiles, the WorldCover tiles, the ITU
maps' directory, the PBF extract, a directory of LoD2 CityGML, one of
Berlin's 1 m XYZ pairs, the Zensus CSV); it reads only the LoD2 tiles and
lidar pairs that meet its grid, and the front hands it a directory of links
to just the tiles this rectangle needs, since the cache holds every tile any
build fetched.

**The front chooses the sources; the compiler takes what it is given.**
`sources.plan` picks the best source for each part of the rectangle (Berlin's
1 m pairs and LoD2 where it touches Berlin, the Zensus grid where it touches
Germany, GLO-30 and OpenStreetMap everywhere) and says what each is used
for, so the page offers no choice and the dialog after **Build** reads the
same list. Given both `lod2_dir` and `osm_buildings`, the compiler takes
LoD2's buildings on the 1 km tiles in that directory that meet the grid and
OpenStreetMap's everywhere else: an OpenStreetMap building whose centroid
lies on one of those tiles is left out, so no building is counted twice.
The tile is the unit of LoD2's coverage, not the city boundary, so on a
tile that Berlin's border crosses the part outside Berlin has no buildings.
Both write to one `buildings.jsonl`, LoD2's lines first, each line's
`source` saying which, and the manifest carries both notices. A cell's
DataQuality code follows most of its built area: LoD2, else OSM tagged,
else OSM default.

**OpenStreetMap is one Geofabrik PBF extract, not Overpass.** One file per
region serves roads, places, peaks and masts, and buildings alike, and
Geofabrik's index gives every extract's outline, so the smallest one holding
the rectangle is chosen. Overpass would be four queries per region, is
rate-limited, and times out on a city's buildings. The selection is at the
top of `planner-pack/src/osm.rs`.

**Nodes are never ground.** A planner pack could carry a `Nodes` layer, the
deployed network baked in at build time. SIMesh's compiler never writes one,
and export, import and what the page is told all leave one out: nodes are
nodesets, which stand on any ground that holds them, and which the layers
on the Nodes tab show, merge and edit. The public node maps come in as
nodesets through `nodes-import`, whose report counts every row into one
bucket (kept, or dropped and why).

**The DataQuality layer's codes are wire values; `rank()` orders them.**
0 GLO-30 synthesized, 1 LoD2, 2 lidar 1 m, 3 OSM default heights, 4 OSM
tagged heights. A cell takes the best-ranked evidence it has: GLO-30 < OSM
default < OSM tagged < LoD2 < lidar.

## A table of every pair

The medium reads every level from a table: every ordered pair of nodes,
per band, its path loss at one frequency in the band
([LOSSTABLE.md](LOSSTABLE.md)). Three choices make it that shape.

**Every pair, not the pairs that can carry a frame.** A receiver's verdict
on a frame sums the interference of everything else on the air at it, and a
transmitter far too weak to be decoded there still adds to that sum; ten of
them can spoil a frame that none of them alone would touch. A table cut at a
link budget would drop exactly those contributions, and the medium would
report as clean a frame a real receiver loses. So nothing is cut for being
weak. A radius (30 km) bounds the work, and a pair beyond it, or off the
ground data, is never heard, flagged so a reader can tell it was not
computed.

**A table, not a model in the medium.** Synthetic ground's log-distance loss
and a pack's P.1812 are written into the same format, so the ether has
one code path and no propagation model of its own, and a table computed
once serves every run of that nodeset on that geodata. It serves the
nodeset's next edit too: a pair's loss depends on its two ends and the
model alone, so a nodeset with a few nodes moved, added or taken away takes
every pair whose ends are unchanged from the nearest cached table
(`losses.nearest_cached`: the most nodes at the same name, position and
height, under the same ground and model) and computes the rest
(`update_nodes`). A run works on its own
copy: a node moved during a run has its row and column recomputed into the
copy, never into the cache, and the ether keeps the old row until the new
one is in.

**A full matrix, per direction.** The two directions of a pair are separate
cells, because a loss measured from a running network is one direction at a
time and an offset or a measurement may differ by direction. The computed
loss does not: a path loses the same both ways, and `link.json` runs P.1812
(or the near-field model) both ways and gives the pair the mean. Run one way
only, P.1812 is not reciprocal as used here: its location variability is
the receiver's alone, and the profile is decimated from the transmitter, so
the two directions of one Mitte pair came out 14 dB apart. For 200 nodes the
matrix is 40,000 cells, about 280 KB.

**Per band, with a correction within it.** A table is computed at one
frequency `f0`, and a frame on carrier `f` in the same band sees
`loss + 20·log10(f/f0)`. Only the free-space term of a loss scales that way;
diffraction, troposcatter, the location spread and the terminal clutter
correction all carry the frequency differently, so the correction is good
across a band's few megahertz and wrong across bands. Each band (433, 868,
915 MHz) has its own table, and 2.4 GHz has none: the SX1262 is the only chip
here, and the planner's clutter correction stops at 3 GHz.

## Reception at the receiver

A collision is not a property of a transmission; it is what happened at one
antenna. So the ether rules per receiver, per frame, from everything arriving
there, in two questions and two tests. [`ether/INTERNALS.md`](ether/INTERNALS.md#reception-two-tests-the-worst-piece-deciding)
is the whole of it; this is its shape.

**The received power** of a transmission at a receiver is the transmit power
the frame states, plus each antenna's gain toward the other end, minus the
table's loss for that direction and the within-band correction; the gains
are on the table the medium is handed, as the offsets are.

**Who is affected, and who can decode, are two questions.** Every
transmission whose channel overlaps the receiver's counts towards its
interference, whatever its spreading factor (SF) or sync word, because a LoRa
demodulator hears every chirp in its band. Only a frame whose bandwidth,
spreading factor and sync word match the receiver's state, on its carrier,
can be decoded. An off-band transmission contributes nothing.

**Two tests, over the whole frame, the worst stretch deciding.** The frame's
air is cut wherever the set of overlapping transmissions changes, and in
every piece:

1. the signal over thermal noise is at or above the spreading factor's
   demodulation threshold;
2. the signal over each **class** of interference is at or above that
   class's rejection figure, where a class is every overlapping transmission
   at one spreading factor, summed in milliwatts before the test.

They are two tests and not one weighted sum: the demodulation threshold is
against noise and is negative, the same-SF figure is against a chirp and is
positive, and adding noise to a chirp's power would make neither mean what
its source measured. Nor are the classes summed into each other, because
each figure was measured against one interfering spreading factor.

**The figures**, all in one table at the top of `ether/ether.py` with their
sources ([`ether/INTERNALS.md`](ether/INTERNALS.md#the-figures)):

| Figure | Value | Source |
|---|---|---|
| thermal noise | −174 dBm/Hz, plus the noise figure (6 dB) | kTB at 290 K; the SX1262's order of magnitude |
| demodulation threshold | −7.5 dB at SF7, 2.5 dB lower per step | SX1261/2 datasheet |
| same-SF rejection | 6 dB | Semtech's specification |
| inter-SF rejection | −8 … −25 dB | Croce et al., "Impact of LoRa Imperfect Orthogonality", IEEE Communications Letters 22(4), 2018, measured on the SX1272 |
| sense threshold | 15 dB over the ETSI (European Telecommunications Standards Institute) EN 300 220-1 sensitivity limit, −81 dBm at 125 kHz | EN 300 220-1 V3.1.1, 5.21.2 |

Carrier sense and CAD (channel activity detection) answer from the same air:
busy when a decodable frame, or summed in-band energy over the sense
threshold, is there.

**The pairwise rule** (`--pairwise`) rules on the same levels the simpler
way, one interferer at a time against a 6 dB margin with nothing summed, so a
run can be compared frame for frame with one ruled that way.

## Why the ether owns the lock

A receiver follows one frame at a time, and which one is decided at the
preamble: a decodable frame takes the receiver when it is not demodulating
another, or when it leads the one in progress by the same-SF figure. That
decision is the ether's, and the chip model has no lock rule of its own.

The lock needs what only the ether sees: everything arriving at the antenna,
summed. A chip told of frames one by one could compare each new one only to
the one it has, and would lock differently from the verdict the ether then
gives it — the medium saying one frame survived while the chip had already
let go of it. So the chip follows whichever frame the ether last began on
it; a frame the receiver does not lock on to is sent as energy (an
`rx_begin` marked `"cad": true`), which raises the level the chip's
instantaneous RSSI (received signal strength indication) and its CAD read
and is never demodulated. [`ether/INTERNALS.md`](ether/INTERNALS.md#why-the-ether-owns-the-lock)
has when the lock is taken and released.

## Reset, factory reset, and why neither is called "apply"

Two verbs, and the difference is exactly the difference a person expects:

- **Reset** presses reset. The process exits, the supervisor brings it back,
  and the state store is untouched.
- **Factory reset** stops the station, deletes its `state/`, and starts it
  again. On the way back up the directory is empty, which is precisely what a
  node clicked onto the map for the first time is — so setup runs again by
  the ordinary path and not by a special case.

There is deliberately no verb that re-runs setup on a running station. "Re-apply" reads like a repair and behaves like one sometimes and not
others: a line that is a setting takes effect, a line that is an action happens
twice. What replaces it is **Run command**, which types one CLI line at the
stations chosen and shows what each said, and a script's intents, which say
one thing to every kind in its own lines. That is more versatile — retune
the whole testbed, survey it, create something on all of it — and it is
honest about being a thing you did rather than a state you restored. A
node's tags changed on a running simulation are no exception: what they
mean to the first-boot rules takes effect at its next first boot, a
factory reset.

## Why the store is flushed before anything is taken away

What follows is the `reticulous` kind's; a kind whose store writes through
(`sergeyculum`) has nothing to flush, and its `flush` does nothing.

`s.storage.flash_delay` is 60 seconds by default: a write sits in RAM for up to
a minute before the store commits. On a board that is a power-cut window and
entirely fair. Here it would make two things lie.

A station **reset moments after its setup ran** would come back with none
of it, and the testbed would be claiming a configuration it never made
durable. So simd sends `save` at the end of setup, and again a few
seconds later — not everything setup asks for lands at once, and an LXMF
(Lightweight Extensible Message Format) identity reaches the store some seconds after the command that created it has
returned.

A **snapshot** copied out of a store with a minute of writes still in RAM would
be a picture of a moment that never quite existed. So every running station is
flushed before the copy, and before any stop or reset.

All of it is best effort with a short timeout. A station that will not answer
is one whose store cannot be flushed, and refusing to stop it over that would
be worse than losing the last minute.

`save` is simd's to send rather than the script's because it is not a
setting. Everything a script says describes what a station *is*; `save` is
about making that description stick.

## First boot: the name, then the first-boot rules in order

```
simd ── framed RPC probe … s.sys.reset_reason answers ──► station   up: booted
simd ── its name, in its kind's lines ─────────────────────────────────► station
simd ── what the first-boot rules give it, in the rules' order ────────► station
        startup.py's: role(…), radio(…), each nodeset's own lines, radio_up()
        then the script's own lines
simd ── save, and again once what setup asked for has landed ──────────► station
```

A station is set up by typing at it. The alternative — a schema of settings
the testbed knows the names of — would have to grow every time the firmware
grew one, and would be a second place for a setting's name to live. So
nothing is declared in the nodeset: every setting is a script's first-boot
rule, a line typed as written or an **intent** (`role`, `radio`,
`radio_up`) each kind says in its own lines, so one rule serves every
firmware. What the page must know without a station running is read from
the same places the rules are written from: the tags the rules select on,
and `scripts/globals.py`, whose radio the startup script sets. Per-node
differences are selections (`nodes(tag=...)`) and macros, never code, which
is what lets the rules travel to simd as data.

**Every world starts from one script.** `scripts/startup.py` says the
roles, the radio from `globals.py`, then includes each nodeset's own
`nodesets/<name>.py` (`for nodeset in nodesets(): include(...)`), then
starts the radios. A script includes it among its declarations, so what a
traffic study runs on any world is readable in three files the editor opens
together, and a world's peculiarities (a TCP gateway, a sync word) live
with the world, not in the study.

**Up means booted, not answering.** A `reticulous` station answers framed
RPC from early in boot, before its services' `onInit` has run, and a setting
typed then can be undone by that init: a radio's frequency, set at the first
instant, was gone by the time the radio started. So the kind counts a station
up only once `s.sys.reset_reason` reads, which the boot writes after every
service's init, and which on a first boot, the only one that is set up, is
absent until then.

**The radio is started last.** A radio reads its settings when it starts —
the community radius it hands to rnsd, whether SUPE is on — so a setting
made after the radio is up waits for the next start. That is why starting
it is an intent of its own (`radio_up`: `lora up` for a `reticulous`
station, nothing for a kind whose radio needs no start), and why the
startup script says it after each nodeset's own setup. A script's own
first-boot lines come after the include, so what the radio reads belongs
in a nodeset's setup or before the include.

**A role the firmware forgets is said at every boot.** Setup runs once, on
a station with no state; a `sergeyculum` station keeps `transport on|off` in
RAM only, so a reset would bring it back a client while its node is tagged
transport. A kind whose role does not survive a restart says so
(`role_volatile`), and simd says the role intents of its first-boot rules
again whenever such a station comes up with state.

Lines are expanded per station: `{name}`, `{id}` and `{addr}`,
`{addr:<node>}` for another node's address, and `{max_dbm}`, the node's
maximum power. That is what lets one shared
line say node-specific things, and it is why simd adds no settings of its
own. An address is always a macro and never written down, because it follows
from the id and from the network the front gave that simulation, which
differs between two simulations of one nodeset.

The name is the node's identity in three places at once —
the map label, the proxy hostname and the station's own name — and must not
drift; simd says the name itself, before any rule, in the kind's own line
(`hostname {name}`),
so it cannot disagree per node.

A macro the list does not define is left exactly as written. A CLI line is
somebody's text and may legitimately contain braces, and silently emptying
something that only looked like a macro is worse than passing it through for
the station to complain about.

Setup runs only on an empty store, which is what makes it safe for it to
contain `lxmf create {name}` — a line that is emphatically not idempotent.
Nothing re-runs it against a configured station; a factory reset empties the
store first.

Whether a station has been set up is sampled **at the fork**, not when the CLI
answers: the firmware writes `state/boot` moments after it starts, and the
answer the setup step needs is the one from before it ran.

## The WebRTC relay, and why it is a relay rather than a second transport

```
browser ──ws  alpha.lora.sim.localhost:8800/webrtc──► front ──ws──► simd ──ws──► alpha   signalling
browser ──udp localhost:8800─────────────────────────► front ──udp─► simd ──udp─► alpha   the channel
```

A station's web UI reads every `s.*` value over a `storage:1` WebRTC
DataChannel. There is no HTTP path for it, so without a DataChannel the page
loads and then knows nothing: no hostname, no settings, no Activity.

A host-only WebSocket transport carrying the same merge-patches would be the
obvious alternative, and the wrong one. A transport that exists only in the
testbed is a code path a board never runs, so the thing being tested stops
being the thing that ships — which is the one promise this testbed makes.

So the station speaks real WebRTC here, from the same source, and what is
sim-only is the plumbing that makes it reachable:

- **Signalling.** simd keeps `/webrtc` for itself — the one station route the
  front listener does not forward — and terminates the WebSocket on both
  sides, so the SDP (session description protocol) answer arrives as a
  parsed message. It rewrites the connection line and candidate to the
  relay's own address and drops the station's, which point at an address
  the browser cannot reach and would only cost it timeouts. The browser's
  cookie goes up with it, because signalling is behind the station's own
  login and a relay that dropped it would be introducing a stranger.
- **Media.** One UDP port in front of every station. The first packet of an
  ICE (interactive connectivity establishment) session is a STUN (session
  traversal utilities for NAT) binding request whose USERNAME begins with
  the answerer's ufrag — the same ufrag simd read out of that station's
  answer — so the packet says which station it belongs to without simd
  having to understand anything else about it. After that the browser's
  address is pinned to a flow with its own socket, and both directions are
  forwarded bytes-for-bytes.

Nothing is decrypted or inspected past the STUN username: DTLS (datagram
transport layer security) and SCTP (stream control transmission protocol)
are end-to-end between the browser and the station exactly as on a board.

Behind the front the relay is relayed, by the same code one level up. The
front keeps `/webrtc` for itself as simd does and opens the child's with
the browser's `Host`, so the child knows which station is meant. The child
points the answer at its own relay, the front points it again at the
published port and ties the answer's ufrag to the child's relay; the first
STUN packet then finds its flow at each relay by the same ufrag. The child
sees the front as a browser, and the station sees the child's relay as it
always does. `webrtc.bridge` is the signalling half for both, and `Relay` the
media half.

What this cost the firmware is three functions —
`webrtc_port.{h,cpp}`: the local addresses to advertise, the address the socket
binds, and a CRC32 (the 32-bit cyclic redundancy check) the chip has in ROM. The rest of ICE, DTLS and SCTP built
for the host unchanged. The bind is the one that matters and is easy to miss:
a chip has a network stack to itself and binds the wildcard, while here every
station is a process on one stack, so each binds its own loopback address or
the second one to start finds the port taken.

## Status, and what `up` means

`stopped` → `starting` → `setup` → `up`, with `restarting` for the gap after an
exit nobody asked for. `up` is **the station booted and answering the door
its kind talks through** — a `reticulous` station's framed RPC once its boot
has written `s.sys.reset_reason`, a `sergeyculum` station's `rncfg detect` —
not the process existing: a firmware process that has forked but not
finished booting is not a station you can do anything with, and the map
should not claim otherwise.

A station's **role** — `transport`, `router`, `repeater` or `client` — is not
status. Its node's role tag names one, which the startup script says to it
and the page draws before anything runs; once it runs, simd asks each
station every few seconds, through its kind, because the setting is live
and a person can flip it on the station itself — the map should show what
the station thinks, not what it was told at its first boot. A kind that
cannot be asked leaves the tag's on show. Roles are a kind's words for what a station does, so
the map draws a forwarding ring for any firmware without knowing its
protocol.

## Kinds, and the rules that come with more than one firmware

Everything the testbed knows about one firmware lives in its kind
(`testbed/kinds/`); simd, the supervisor and the page know only the kind's
methods. The station contract ([STATION.md](STATION.md)) is what every kind
shares, and it is small on purpose: an identity, a directory, an address and
the ether, in `SIMESH_*`, and a console on stdin/stdout.

**One chip model for every kind, below the driver.** Every kind links the
same `radio/`, whatever its language. A rewrite per language would drift on
exactly the details the testbed exists to hold constant, and a seam above
the driver, at a `LoRaRadio`-style level, would skip BUSY, CAD, the sync-word
register write and a CAD cutting off a reception, which are what break on
boards.

**Protocol parts sit behind their protocol.** The ether, the record, the
map, airtime per carrier and link geometry know no protocol; LXMF traffic,
delivery analysis and SUPE frame classes live under `simesh.reticulum`, so a
firmware of another protocol gets everything generic and nothing that
misreads it.

**A mixed SUPE run builds `sergeyculum` without `supe-band`.** That switch
drops channel 9 on duty-cycle readings, while `reticulous` uses all nine
channels with polite spectrum access; the two would derive different
schedules, and the run would measure the mismatch instead of the protocol.

**A kind supplies what its firmware reads.** A firmware that reads other
names for the contract's values gets them from its kind's `env`, beside the
contract's own; the contract does not grow to fit one firmware.

**A line is in one dialect; an intent is in every one.** A line goes only to
stations of one kind: **Run command** and a script's `run` refuse a choice
of stations that spans kinds unless a kind narrows it. The same text typed
at another firmware means something else or nothing, and a testbed that sent
it anyway would be reporting an answer to a question it never asked. An
intent (`announce`, `message`, `set_role`, …) is what is meant rather than
what is typed, so it goes to every kind and each says it in its own lines; a
kind with no way to say it answers that it has none, for its stations alone.

**Whether a station is set up is the kind's to say, and it is sampled at the
fork.** Each firmware leaves its own mark in `state/` on a first boot, moments
after it starts; the answer the setup step needs is the one from before it
ran. Get it wrong one way and setup runs on every restart, the other way
and it never runs.

**One conversation at a time on a station's door.** `rncfg` opens the
station's KISS pty (KISS, "keep it simple, stupid", is the serial framing radio modems speak) per command; two at once interleave their frames and both
read garbage, so the `sergeyculum` kind holds a lock per station around every
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
The station writes a whole reply under the console's write lock, so none of
that happens in a healthy run; it is there so that a line of noise costs one
answer and not the stream.

The station bounds a command at five seconds and cuts a reply that outgrows its
buffer at the last complete line, without saying so. A reply is therefore not
proof that a command finished: a line that came back at the bound may still
be running, and the next frame would find the command line busy. So a slow
line is followed by a question that confirms it landed, and a caller asks for
one key at a time rather than a subtree whose tail could be cut.

**A query is retried only after a whole timeout with no answer at all**, the
five-second bound plus margin (eight seconds), and at most twice. A slow
command that must not run twice, such as `lxmf create`, is then never run
again by a retry.

**A driver takes T from a `command_result`, not from `clock`.** `clock`
comes once per wall second, so at a high pace it is tens of seconds of T
stale, and a driver that scheduled from it would send every message that
late.

**Ids are unique across kinds.** The ether keys stations by id; two processes
answering under one id are one station to the medium, and two sockets on one
address. A nodeset refuses a file that repeats one, and its editor an id
another node has.

## The page

Pinia stores hold the page's state, split as the data is: `catalog` (what
the store holds — devices, geodata, nodesets, scripts, snapshots — and the
script runs with their output), `geodata` (which ground is on show, and for
a pack its manifest and the sidecar's base path), `display` (how the map is
shown, per tab), `nodes` (the Nodes tab's active layer, its selection, its
dirty state, and the other layers, as their files stand), `coverage` (the nodes' rasters) and `sim` (the running simulations
and the attached one's live state); one socket store owns the websocket
they all speak through, and `lib/front` matches each editor verb's answer to
its request. Every component reads the stores; every action is one store
method that sends one message. A reconnect replays the `snapshot`, so the
page holds no state simd cannot restate — which is the whole of what makes
simd restartable under a page that is open. The active layer is the one
exception, deliberately: it is the page's own until it is saved. That is
also why Save visible as sends the layers as they stand to the front to be
merged there (`nodeset.merge`), rather than naming files: the active one's
unsaved edits are part of what is saved.

**A reply the sidecar cut short is never drawn as if it were whole.** The
sidecar caps a footprint reply's vertices and fills it in the pack's order,
not the requested box's, so a capped reply covers some of the box, in no
useful shape. Footprints are asked for in fixed squares of the ground, each
kept once fetched, and a square whose reply says it was cut is asked for
again as four. Coverage rasters are held the same way, per node and per
position: an answer names the node and the place it was asked for, answers
add to what is held rather than replace it, and one that lands late, or
for a node since moved, is never read as the node's; and the map's repaint
key includes which raster each node was drawn from, so a raster arriving is
a repaint.

**One map, two places.** The map page (`NodesPage`) is one component in
two modes. On the Nodes tab it edits the active layer, with the other shown
layers drawn beside it; opened from a
running simulation's row it stands on the Simulations tab in place of the
list and shows that run's stations live, and the same edits (move, set,
tag, offset, remove) go to the run's own copy as messages to its simd. The
map, the editor and the tags panel read one list of nodes (`nodes.list`) and
edit through one set of actions, so none of them knows which mode it is in.
Going to the Nodes tab closes an open simulation, so that tab only ever
shows nodesets.

**Coverage is combined once per set of nodes.** On a pack each node's raster
is merged into one grid of the best margin for the nodes on show, kept for
the last few sets (the whole network, and each selection lately shown); a
raster that lands later is merged into the grids that want it rather than
rebuilding them, and a repaint is one lookup a cell whatever the count.

The same page is served by the front and by a simd on its own, and tells them
apart by the front's `hello`. Behind the front the store keeps the registry
and the attached simulation beside the one simulation's state it always kept;
a reconnect re-sends the selection, which the front forgot with the socket,
and the child's `snapshot` restates the rest. A message whose `sim` is not the
attached one is dropped: it is from a socket the front was still closing when
the selection changed. Served by a simd on its own there is only the Nodes
tab, attached to it.

The map is a **canvas**. Ground tiles, thousands of building outlines,
hundreds of pulses a minute and a drag at 60 Hz are all much cheaper drawn
than laid out, and none of them wants to be an element. It is drawn in the
geodata's own metres — a pack's UTM (Universal Transverse Mercator) zone, the
same transverse Mercator the nodes' positions are projected with for the
planner, or synthetic ground's nautical mile to the minute from 0°, 0°, as
`geodata.py` projects it — so a pixel and a metre agree by construction
rather than by two implementations staying in step.

**Coverage is a margin, worked out on the page.** The layer is the best
decoding margin at each point over the nodes shown: each node's transmit
power and its antenna's gain toward a receiver 2 m over the ground there,
less its path loss there, less its own threshold (its SF and
bandwidth over the noise floor). On a pack the loss is the node's raster
from the front; on synthetic ground it is the log-distance formula. It is
painted once per settled view into a surface of its own, a cell every few
pixels, so panning costs nothing, and asked for only while the Nodes tab is
on show with the layer on. It is drawn in bands of what the margin is good
for rather than a ramp (`COVERAGE_BANDS`: indoors too from 21 dB, outdoors
only from 6, the edge from 0), because the question a map is asked is
whether a place is served, and a ramp makes the eye judge dB.

**Under a heatmap only what is picked keeps its colour.** Population and
coverage are one at a time, and while one is on the ground is drawn grey:
the base, roads and buildings from grey copies of their images (made once
per paint, since `ctx.filter` is not in every browser), offsets and rings
through one colour function that greys them. A red road or a yellow ring
over a coverage map would read as coverage. The nodes and the selected
node's links keep their colours: they are what the map is being looked at
for, and a link's colour is a verdict of its own.

A drag on a simulation's map sends `nodeset_move` at a few Hz with
`settle: false`, and once more on release with `settle: true`. Only the
settled move is written into the run's nodeset, logged, and has its row of the
loss table recomputed, so a drag is one edit and one row rather than fifty;
the station is drawn stale until its row is in.

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
| NVS (non-volatile storage) | a flash partition | a file under `/tmp`, sized from the built partition table |
| addresses | WiFi | one loopback address per station |
| console | a serial port | a pty, bridged to the map's terminal window, carrying framed RPC as a board's USB console does |
| radio | an SX1262 | a model, and the ether |

The pty matters: a station's stdin and stdout **are** its serial console, so
the supervisor holds the master end and the console window is that pty over a
websocket. Keystrokes go as binary frames and the terminal size as a JSON text
frame, so no byte a person can type is special to the transport.

## The one rule that makes interrupts real

This one is the reticulous firmware's, whose driver waits on DIO1, the
chip's interrupt line; a driver that polls the IRQ (interrupt request)
register over the bus never meets it.

The GPIO (general-purpose input/output) shim ([`hw-linux`](../hw-linux/README.md)) is a pin table, and the
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
- **A receiver follows one frame at a time.** The chip follows the frame
  the medium last began on it, and takes an `rx_end` only for that one. The
  medium decides which that is, because only it sees everything arriving at
  the antenna summed: a frame the receiver does not lock on to comes as an
  `rx_begin` marked `"cad": true`, which raises the air's level for RSSI and
  CAD and is not demodulated. Without one frame at a time the driver would be
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

The model and its ether link are one C++ library behind a C ABI (application
binary interface, `radio/include/simradio.h`), reaching the host only through a table of
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

A virtual-time run is nearly serial: T moves only when every station is idle,
so more cores buy more runs side by side (other seeds, other scripts), not a
faster run. A real-time run never shares a machine with a build, because the
tick drift a loaded host adds makes it unreproducible for reasons that have
nothing to do with the protocol.

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

**The shim is also the station's randomness** when the environment carries
`SIMESH_SEED` (a kind sets it from the ether's seed): `getentropy`,
`getrandom` and `syscall(SYS_getrandom)` — ESP-IDF's host `esp_random` and
mbedtls's platform entropy between them — draw from a splitmix64 counter keyed
by the seed and `SIMESH_NODE_ID`. It needs no welcome, so it holds from the
first draw. A call reserves all its words in one atomic step and takes no
lock, so a thread switched out mid-call neither blocks another nor changes
its bytes. With the same seed and epoch, a station draws the same bytes in the
same order in every run, and what is left to differ between two runs is what
comes from outside them (below).

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
- a `sergeyculum` or `microreticulum` station, whose threads are plain
  pthreads: the shim's thread census (`SIMESH_IDLE=threads`). A thread counts
  as blocked while it is in one of the shim's waits, an untimed
  `pthread_cond_wait`, or a read on a blocking descriptor that is not a file;
  when the last one blocks, the station is idle. A read on a regular file
  does not count, however long it takes: nothing outside the process ends
  it, and counting it let a station reading its stored tables at boot look
  idle mid-work, with no wake held, and T run seconds ahead of it. A thread
  counts as running again the moment it is woken, not when it next gets the
  CPU: its wake firing, or a `pthread_cond_signal`/`pthread_cond_broadcast`
  on the condition it waits on. Otherwise a thread that signals another and
  then blocks itself would leave every thread counted blocked while the one
  it woke has work to do. A timed wait's deadline is broadcast holding the
  waiter's mutex, tried for 10 ms of wall time, since the waiter has joined
  the census a moment before its wait gives the mutex up, and a broadcast in
  between would be lost.

Neither can be told apart from a thread that is waiting where nothing can
see it, so the conductor also has a **busy watchdog**: a station that has not
said idle 20 ms of wall time after it was last told anything, or last sent
the ether anything, says so anyway, with the `until` it has — for a
`reticulous` station, whose `until` is the next tick while a task runs, a
tick at a time. That is what keeps a thread spinning until T moves, or a host
descriptor nobody is watching, from stopping T. It must not fire on honest
work — key generation at first boot, a PBKDF2 of a community passphrase, a
signature check under load — or T would move on while the station is still
computing, by however many ticks the host's speed makes the work span. So the
shim holds the watchdog's timer back while another of the station's threads
is on the CPU: it reads the timerfd for the watchdog, and while a thread is
running and the process is spending user time or reading and writing, it
sets the timer again and waits on. A spin — a task yielding in a loop, which
on this host is signal-mask calls in the kernel and nothing read or written —
is let through after 50 ms of looking at it, and anything after 10 s. Work
then takes no T, the same in every run; the `clock` message counts idles that
took the watchdog's time as `slow_idles`, and simd logs the stations
concerned when the run's pace drops below 2x.

**What comes from outside the run lands at an instant of T.** The ether
turns everything that can wake a station other than its own messages into an
instant ([`ether/INTERNALS.md`](ether/INTERNALS.md#what-does-not-come-over-the-air)):

```
testbed      sleep ends at T; T holds until what it woke has run
testbed      typed(sid, n) — T waits;  sync(sid) — station told T, idle
testbed      writes the pty;  station reads, shim → ether  read {tty, total}
ether        run {t: the station's T} — it owes an idle for that work
station      prints a reply, idles;  testbed reads every pty that ran, then T moves
writer       shim → ether  wrote {tcp/A>B, n, go}, waits
ether        quiet: reader told T, then  go  (lowest station first)
reader       reads, shim → ether  read {tcp/A>B, n};  ether → reader  run
```

The shim does the station's side of it, with no help from the firmware: it
counts what `read` takes from descriptor 0 and reports the running total once
a read leaves nothing waiting; it asks before a `write`/`send` on a TCP
connection to another station, on a socket of the writing thread's own
(`SIMESH_ETHER`), and reports what `read`/`recv` take from one; and it makes
a station's TCP connection to a loopback address leave from the station's own
address (`SIMESH_BIND_ADDR`), so both ends of it say which station they are.
Its reports go on the chip library's own socket to the ether, found from the
`hello` sent on it, so each is ahead of the idle that follows it. The
testbed's waits are on T as well: `Kind.pause`, `simd`'s `sleep`, `after`
and the role poll, and a framed-RPC query's timeout. With the same seed and
epoch, two runs of the same network put the same frames on the air at the
same instants.

What is still outside: a TCP connection from something that is not a station
(the page's proxy to a station's web UI), a station's UDP to another, the
files it shares with the testbed (`rncfg`'s KISS socket for a `sergeyculum`
station), and anything a person does, which lands at whatever T the run has
reached; and a firmware that reads its console other than by `read` on
descriptor 0 holds T a second each time it is typed at.

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
- Memory: the heap ignores capabilities and wraps libc, so pressure on the
  external PSRAM (pseudo-static RAM), DMA-capable (direct memory access)
  allocation and internal-RAM exhaustion are all invisible.
- The radio's physics below the path-loss model. There is no fading, no
  antenna pattern, no band above the sensitivity threshold where a frame
  fails its CRC (cyclic redundancy check) at a probability, and no noise
  floor but the thermal one; a pair's loss is the table's and is the same
  for every frame between them. On real ground that loss is P.1812's
  statistical figure at 50 % of time and 90 % of locations, not a
  measurement of that path. See
  [`ether/INTERNALS.md`](ether/INTERNALS.md) for what the medium does
  and does not decide.
- Anything below the C: the compiler, the ABI and the word size are the
  host's. Code that assumes a 32-bit `long` or pointer fails here and not
  on the chip, which makes the host build a free audit of width assumptions,
  and a clean run here is not a clean run on a board.
- Races between cores: the port runs one core. Scheduling faults look
  different too, since preemption runs through signals, so starvation and
  priority inversion are not the chip's.
- The boot chain, partitions, OTA (over-the-air) updates, safe mode,
  watchdogs, panics, core dumps and reset reasons (`esp_restart` is a process
  exit); growing the state partition at runtime, a factory reset by
  partition, and the updater.
- WiFi, BLE (Bluetooth Low Energy) and ESP-NOW; sleep and power management,
  GPIO wake included; and every I2C and SPI peripheral but the radio (GPS,
  IMU, RTC, SD card, battery ADC).

## What to model next, and what not

A simulator that is subtly wrong is worse than one that leaves a thing out:
it gives confidence rather than information. So every physical figure the
ether uses is to be held against a measurement on real boards, kept as a
regression check, and what gets modelled next goes in this order: capture,
wrong sync word, deafness while retuning, occupancy, the noise sum, the CRC
band. Fading, multipath, antenna patterns and clock drift come after all of
those, if at all; past that point the cost grows and the answers do not
change.

## Still to build

**The medium and the record**

- **Mode changes take the datasheet's time, and the ether honours
  `ready_at`.** The model charges each transition: from `STDBY_RC` to
  XOSC, FS, RX or TX 150 µs or the TCXO (temperature-compensated crystal
  oscillator) delay `SetDIO3AsTCXOCtrl` programmed; XOSC to FS 40 µs; FS to
  RX 40 µs; FS to TX the power-amplifier ramp from `SetTxParams`; a wake from
  SLEEP 150 µs plus 500 µs; `Calibrate` 3.5 ms. `ether_link.cpp` already
  sends `ready_at`; the ether treats a frame whose `t0` falls before a
  receiver's `ready_at` as energy with no lock. That makes a wrong retune or
  turnaround constant a frame loss that reproduces.
- **A loss cause for every frame at every station in range**, in the record:
  `no_rx`, `tx`, `settling`, `off_channel`, `wrong_rate`, `wrong_sync`,
  `below_threshold`, lost lock, `crc` from interference, `left_rx`, with a
  summary per station and per pair. A protocol claim ("`settling` cannot
  happen here") and a departure policy are judged by these.
- **A referee over `record.tsv`**: it reads the channel plan and the `state`,
  `tx`, `rx_begin` and `rx_end` lines and names the line that breaks a rule:
  more than 100 s of transmission in any hour per 500 kHz channel, less than
  100 ms off-time before returning to a frequency, radiated power over a
  channel's cap, a `tx` without the sense window of RX or CAD before it on
  that carrier, a `tx` from a slot that is inside a reception.
- **The CRC band**: in the `crc_margin_db` (3 dB) above the demodulation
  threshold, a locked frame ends as `crc` with a probability falling linearly
  from 1 to 0, drawn from the run's seeded generator.
- **`next_instant()` from a heap**: the pending `until`s kept in a heap keyed
  by instant and station, updated on idle, retraction and leave, stale
  entries dropped when popped, instead of a scan of every station per
  barrier.
- **Runs off the bind mount**: a run's directory on the container's own
  filesystem or a tmpfs, with the page and the analysis tools reading it
  there.
- **Hardware checks**: RSSI (received signal strength) at two known
  distances and the channel-switch success rate on real boards, kept as
  regression checks on the ether and the loss model. Time on air is checked
  against a board, not only against the formula.

**Stations and kinds**

- **The Python reference Reticulum as a station kind**, the oracle: under the
  time shim with `SIMESH_IDLE=threads`, attached to a `reticulous` station's
  radio through the RNode-over-TCP endpoint (`s.lora.rnode.tcp`) or through
  `iface-tcp`. It answers what `rnsd` does when an interface stops taking
  packets, and whether real Reticulum agrees with our neighbour and identity
  inference.
- **`sergeyculum` stations built with `--profile sim`** (opt-level 3), from
  `target/sim/simesh`, in `devices/local/sergeyculum_local.yaml` and the
  README's developer loop; release is opt-level `z` with LTO (link-time
  optimisation), several times slower at signature checks.
- **`libudev-dev` in SIMesh's image**: `rncfg` links `libudev` through
  `serialport`, so Sergeyculum's Cargo workspace does not build in the image
  without it.
- **The mixed-kind walkthrough** in the README: announces crossing both ways,
  a path through our transports, a two-frame split both ways, carrier sense
  under contention, and the hidden terminal, each with its commands and what
  `seq.py` shows.
- **The chip model as a submodule** of `simesh-radio-sys`, as an alternative
  to `SIMESH_RADIO_DIR` or the workspace layout.
- **Tests of a station's own screen**: the `lcdmirror` framebuffer tap in
  `lcd_lvgl.cpp`'s flush callback without its WebRTC half, `tinylcd`'s 1-bit
  buffer read directly, and LVGL's `LV_USE_TEST` for time, injected input and
  image comparison. No panel is emulated.
- **An `hw-qemu` board**, outside SIMesh: a UART0 console, OpenCores Ethernet
  for WiFi, no ADC user, no Bluetooth, to ask regularly whether the real
  binary still boots. LittleFS does not yet format the state partition there.

**Running and analysing**

- **Freeze and step**: T held at the barrier while the map, the consoles and
  every station's web UI stay live, and a step that moves T on by a given
  amount and holds it again. Pause stops simd; this does not.
- **Single stations off and on** with their state kept (drawn grey), as
  `sel.stop()` and `sel.start()` in the library and on the node menu.
- **The run analyses as tools** in `testbed/simesh/reticulum/` beside
  `delivery.py`, reading a run's logs and record: `peer left` withdrawals and
  the routes they dropped; directory entries, full blob pools and evictions;
  neighbour-table rows and evictions (from `table … evicted, … gone
  silent`); paths a reloaded snapshot kept; calling-channel power and rate
  steps per target. `callkind` classes a short HEADER_2 frame correctly.
- **Traffic scripts for SUPE's departure policy**, the one pure function
  `should_channel switch(peer_state, queue_state, channel_state) -> no | now |
  wait_until(t)`, each reporting losses by cause: an interactive exchange that
  waits on every delivery proof (where a naive hold timer is strictly
  harmful), a receiver-driven resource transfer, many stations on one
  transport, a peer that is not there, and SUPE and plain-LoRa nodes sharing a
  channel (whether SUPE is a good neighbour). The same runs settle SUPE's
  stated timing constants: turnaround, retune gap, burst gap, guard, seed gap
  and the schedule's spacings, jitters and lifetimes. No policy is committed
  in code before these have measured it.
- **Run options on the page**: a paced `<k>x`, a build override, and a new
  simulation started from a snapshot.
- **Which build a simulation runs** on its row (source and stamp), and on the
  Firmware tab the scripts and simulations that use each build.
- **An LR2021 model** beside the SX1262's, and a node that can be one.
- **Meta commands carried by the firmware**: each device zip naming its own
  implementations of a standard list of meta commands (`max_tx_pwr`,
  `send_msg`, …), in place of the kinds' intents, so a new firmware brings
  them with it. Until then the kinds say them, and a Meshtastic kind adds
  its own lines.
- **Firmware changed mid-run as an experiment**: `firmware()` in `main`
  restarts the nodes it changes with their state kept; an upgrade test
  across firmwares whose stores differ needs a rule for what becomes of it.
- **An editable node id** in the editor, refusing one another node has and
  saying that the station restarts.
- **The snapshots in `testbed/snapshots/`**, still in the scenario format,
  retaken from converted runs or removed.
- **A live-reload mode** for working on the page: the Quasar dev server beside
  the front, with `/ws` and `/api` proxied.
- **Fewer costs per barrier in simd**: the UDP send per station and the JSON
  encoding are its largest.

**Ground and losses**

- **433 and 915 MHz tables on a pack**: `link.json` takes a carrier, so a
  pack gives all three bands.
- **A batched pair request** in `planner-web`, parallel over pairs, instead
  of one request per cell.
- **A geodata editor** on the Geodata tab's map, and synthetic terrains other
  than `flat`.
- **Losses from a running network**: measured cells (the table's flag bit 4
  and `samples`), a lower bound for a pair that does not hear, and the
  residuals as calibration. Below the noise floor RSSI reads the noise, so
  received power there is SNR (signal-to-noise ratio) plus the floor; the
  median is taken per direction over time; a pair that does not hear gets a
  bound from the sensitivity, not a value; the sender's power is needed per
  frame, because SUPE varies it.
- **x86_64 device packages** beside the aarch64 ones, from a builder of that
  architecture.
